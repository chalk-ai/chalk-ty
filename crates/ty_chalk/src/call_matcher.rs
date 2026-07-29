use ruff_db::files::FileRange;
use ruff_python_ast::name::Name;
use ty_python_semantic::Db;
use ty_python_semantic::chalk::{
    CallDefinitionOriginKind, CallModuleProvenance, CallTarget, ChalkClassRelation, Definition,
    KnownCallTarget, ModuleOrigin, chalk_call_definition_origin, chalk_receiver_module_relation,
};
use ty_python_semantic::types::Type;

use crate::supported_functions::{
    CallKind, SupportedFuncs, SupportedSignature, SupportedTy, current_supported_functions,
};
use crate::type_matcher::{TypeMatch, match_supported_type};

/// One source-ordered argument to a transient call match.
#[derive(Clone, Copy, Debug)]
pub(crate) enum ObservedArgument<'a, 'db> {
    Positional(Type<'db>),
    Keyword { name: &'a str, ty: Type<'db> },
}

/// One statically possible target of a transient call match.
#[derive(Clone, Copy, Debug)]
pub(crate) enum ObservedCallTarget<'a, 'db> {
    Resolved(CallTarget<'db>),
    Known(KnownCallTarget),
    ModuleProvenance(&'a CallModuleProvenance),
    Deferred,
}

/// Transient native inputs for matching one statically possible call target.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ObservedCall<'a, 'db> {
    pub(crate) target: ObservedCallTarget<'a, 'db>,
    pub(crate) arguments: &'a [ObservedArgument<'a, 'db>],
    pub(crate) receiver: Option<Type<'db>>,
}

/// Stable semantic identity of a matched call target.
#[derive(Clone, Debug, Eq, Hash, PartialEq, salsa::Update, get_size2::GetSize)]
pub(crate) enum CallMatchIdentity<'db> {
    Definition(Definition<'db>),
    Known(KnownCallTarget),
    ModuleSymbol { module: Box<str>, symbol: Name },
}

/// Compact registry identity retained for later deterministic diagnostics.
#[derive(Clone, Debug, Eq, Hash, PartialEq, salsa::Update, get_size2::GetSize)]
pub(crate) struct CallMatchTarget<'db> {
    pub(crate) identity: CallMatchIdentity<'db>,
    pub(crate) kind: CallKind,
    pub(crate) name: Name,
    pub(crate) display_label: Box<str>,
    #[get_size(ignore)]
    pub(crate) definition_range: Option<FileRange>,
    /// The registry parameter occupied by the receiver and omitted from method suggestions.
    pub(crate) receiver_parameter: Option<u32>,
}

/// Why a concrete call target did not match.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, salsa::Update, get_size2::GetSize)]
pub enum CallNoMatchReason {
    MissingRegistryEntry,
    SignatureMismatch,
}

/// Three-valued result of matching one statically possible call target.
#[derive(Clone, Debug, Eq, Hash, PartialEq, salsa::Update, get_size2::GetSize)]
pub(crate) enum CallMatch<'db> {
    Match(CallMatchTarget<'db>),
    NoMatch {
        target: CallMatchTarget<'db>,
        reason: CallNoMatchReason,
    },
    Inconclusive,
}

/// Matches one statically possible call target against the current Chalk registry.
///
/// Native [`Type`] values are consumed only while this function runs and are never retained in the
/// result.
pub(crate) fn match_call_target<'db>(
    db: &'db dyn Db,
    call: ObservedCall<'_, 'db>,
) -> CallMatch<'db> {
    Matcher {
        db,
        supported: current_supported_functions(),
    }
    .match_call(call)
}

#[derive(Clone, Copy)]
enum ActualType<'db> {
    Native(Type<'db>),
    SyntheticModule {
        name: &'db str,
        origin: ModuleOrigin,
    },
}

#[derive(Clone, Copy)]
enum ActualArgument<'a, 'db> {
    Positional(ActualType<'db>),
    Keyword { name: &'a str, ty: ActualType<'db> },
}

impl<'a, 'db> From<ObservedArgument<'a, 'db>> for ActualArgument<'a, 'db> {
    fn from(argument: ObservedArgument<'a, 'db>) -> Self {
        match argument {
            ObservedArgument::Positional(ty) => Self::Positional(ActualType::Native(ty)),
            ObservedArgument::Keyword { name, ty } => Self::Keyword {
                name,
                ty: ActualType::Native(ty),
            },
        }
    }
}

#[derive(Clone, Copy)]
struct ProtocolOperation {
    builtin: &'static str,
    method: &'static str,
    receiver_argument: usize,
}

const PROTOCOL_OPERATIONS: &[ProtocolOperation] = &[
    ProtocolOperation {
        builtin: "bool",
        method: "__bool__",
        receiver_argument: 0,
    },
    ProtocolOperation {
        builtin: "len",
        method: "__len__",
        receiver_argument: 0,
    },
];

enum ClassifiedCall<'a, 'db> {
    Blanket(CallMatchTarget<'db>),
    MissingRegistryEntry(CallMatchTarget<'db>),
    Registry {
        target: CallMatchTarget<'db>,
        arguments: Vec<ActualArgument<'a, 'db>>,
        protocol: Option<ProtocolOperation>,
    },
    Deferred,
}

struct Matcher<'db> {
    db: &'db dyn Db,
    supported: &'static SupportedFuncs,
}

impl<'db> Matcher<'db> {
    fn match_call<'a>(&self, call: ObservedCall<'a, 'db>) -> CallMatch<'db> {
        let classified = match call.target {
            ObservedCallTarget::Deferred => ClassifiedCall::Deferred,
            ObservedCallTarget::Known(known) => Self::classify_known(known, call),
            ObservedCallTarget::ModuleProvenance(provenance) => {
                Self::classify_module_provenance(provenance)
            }
            ObservedCallTarget::Resolved(target) => self.classify_resolved(target, call),
        };

        match classified {
            ClassifiedCall::Blanket(target) => CallMatch::Match(target),
            ClassifiedCall::MissingRegistryEntry(target) => CallMatch::NoMatch {
                target,
                reason: CallNoMatchReason::MissingRegistryEntry,
            },
            ClassifiedCall::Deferred => CallMatch::Inconclusive,
            ClassifiedCall::Registry {
                target,
                arguments,
                protocol,
            } => {
                let result = if let Some(protocol) = protocol {
                    if self
                        .supported
                        .signatures(CallKind::Builtin, protocol.builtin)
                        .is_none()
                        || self
                            .supported
                            .signatures(CallKind::Method, protocol.method)
                            .is_none()
                    {
                        return CallMatch::NoMatch {
                            target,
                            reason: CallNoMatchReason::MissingRegistryEntry,
                        };
                    }
                    self.match_protocol(protocol, &arguments)
                } else {
                    let Some(signatures) = self.supported.signatures(target.kind, &target.name)
                    else {
                        return CallMatch::NoMatch {
                            target,
                            reason: CallNoMatchReason::MissingRegistryEntry,
                        };
                    };
                    self.match_signatures(signatures, &arguments)
                };

                match result {
                    TypeMatch::Match => CallMatch::Match(target),
                    TypeMatch::NoMatch => CallMatch::NoMatch {
                        target,
                        reason: CallNoMatchReason::SignatureMismatch,
                    },
                    TypeMatch::Inconclusive => CallMatch::Inconclusive,
                }
            }
        }
    }

    fn classify_known<'a>(
        known: KnownCallTarget,
        call: ObservedCall<'a, 'db>,
    ) -> ClassifiedCall<'a, 'db> {
        match known {
            KnownCallTarget::StrStartswith => {
                let Some(receiver) = call.receiver else {
                    return ClassifiedCall::Deferred;
                };
                let mut arguments = Vec::with_capacity(call.arguments.len() + 1);
                arguments.push(ActualArgument::Positional(ActualType::Native(receiver)));
                arguments.extend(call.arguments.iter().copied().map(ActualArgument::from));
                ClassifiedCall::Registry {
                    target: CallMatchTarget {
                        identity: CallMatchIdentity::Known(known),
                        kind: CallKind::Method,
                        name: Name::new_static("startswith"),
                        display_label: "str.startswith".into(),
                        definition_range: None,
                        receiver_parameter: Some(0),
                    },
                    arguments,
                    protocol: None,
                }
            }
        }
    }

    fn classify_module_provenance<'a>(
        provenance: &CallModuleProvenance,
    ) -> ClassifiedCall<'a, 'db> {
        if !is_chalk_namespace(provenance.module_name()) {
            return ClassifiedCall::Deferred;
        }
        if provenance.ownership_origin() != ModuleOrigin::ThirdParty {
            return ClassifiedCall::Deferred;
        }
        ClassifiedCall::Blanket(CallMatchTarget {
            identity: CallMatchIdentity::ModuleSymbol {
                module: provenance.module_name().into(),
                symbol: Name::new(provenance.symbol_name()),
            },
            kind: CallKind::Method,
            name: Name::new(provenance.symbol_name()),
            display_label: format!("{}.{}", provenance.module_name(), provenance.symbol_name())
                .into(),
            definition_range: None,
            receiver_parameter: provenance.receiver_parameter(),
        })
    }

    fn classify_resolved<'a>(
        &self,
        target: CallTarget<'db>,
        call: ObservedCall<'a, 'db>,
    ) -> ClassifiedCall<'a, 'db> {
        let Some(origin) = chalk_call_definition_origin(self.db, target.definition) else {
            return ClassifiedCall::Deferred;
        };
        let identity = CallMatchIdentity::Definition(target.definition);
        let display_label: Box<str> = if origin.module_name().is_empty() {
            origin.qualified_symbol().into()
        } else {
            format!("{}.{}", origin.module_name(), origin.qualified_symbol()).into()
        };
        let definition_range = Some(origin.definition_range());

        if origin.ownership_origin() == ModuleOrigin::ThirdParty
            && is_chalk_namespace(origin.module_name())
        {
            return ClassifiedCall::Blanket(CallMatchTarget {
                identity,
                kind: CallKind::Method,
                name: Name::new(origin.symbol_name()),
                display_label,
                definition_range,
                receiver_parameter: (origin.kind() == CallDefinitionOriginKind::Method
                    && call.receiver.is_some())
                .then_some(0),
            });
        }

        if origin.kind() == CallDefinitionOriginKind::ClassConstructor {
            if origin.ownership_origin() != ModuleOrigin::StandardLibrary
                || origin.module_name() != "builtins"
            {
                return ClassifiedCall::Deferred;
            }
            let protocol = protocol_operation(origin.symbol_name());
            return ClassifiedCall::Registry {
                target: CallMatchTarget {
                    identity,
                    kind: CallKind::Builtin,
                    name: Name::new(origin.symbol_name()),
                    display_label,
                    definition_range,
                    receiver_parameter: None,
                },
                arguments: call
                    .arguments
                    .iter()
                    .copied()
                    .map(ActualArgument::from)
                    .collect(),
                protocol,
            };
        }

        let kind = origin.kind();
        if kind == CallDefinitionOriginKind::NestedFunction {
            return ClassifiedCall::Deferred;
        }
        if kind == CallDefinitionOriginKind::Method && call.receiver.is_none() {
            return ClassifiedCall::Deferred;
        }

        let is_builtin = origin.ownership_origin() == ModuleOrigin::StandardLibrary
            && origin.module_name() == "builtins"
            && kind == CallDefinitionOriginKind::TopLevelFunction;
        let protocol = is_builtin
            .then(|| protocol_operation(origin.symbol_name()))
            .flatten();
        let target = CallMatchTarget {
            identity,
            kind: if !is_builtin {
                CallKind::Method
            } else {
                CallKind::Builtin
            },
            name: Name::new(origin.symbol_name()),
            display_label,
            definition_range,
            receiver_parameter: (!is_builtin).then_some(0),
        };

        if matches!(
            origin.ownership_origin(),
            ModuleOrigin::FirstParty | ModuleOrigin::Extra | ModuleOrigin::Other
        ) {
            return ClassifiedCall::MissingRegistryEntry(target);
        }
        if !matches!(
            origin.ownership_origin(),
            ModuleOrigin::StandardLibrary | ModuleOrigin::ThirdParty
        ) {
            return ClassifiedCall::Deferred;
        }

        let mut arguments = Vec::with_capacity(call.arguments.len() + 1);
        if target.kind == CallKind::Method && protocol.is_none() {
            let receiver = match (kind, call.receiver) {
                (_, Some(receiver)) => ActualType::Native(receiver),
                (CallDefinitionOriginKind::TopLevelFunction, None) => ActualType::SyntheticModule {
                    name: origin.module_name(),
                    origin: origin.ownership_origin(),
                },
                _ => return ClassifiedCall::Deferred,
            };
            arguments.push(ActualArgument::Positional(receiver));
        }
        arguments.extend(call.arguments.iter().copied().map(ActualArgument::from));

        ClassifiedCall::Registry {
            protocol,
            target,
            arguments,
        }
    }

    fn match_protocol(
        &self,
        protocol: ProtocolOperation,
        arguments: &[ActualArgument<'_, 'db>],
    ) -> TypeMatch {
        let Some(builtin_signatures) = self
            .supported
            .signatures(CallKind::Builtin, protocol.builtin)
        else {
            return TypeMatch::NoMatch;
        };
        let Some(method_signatures) = self.supported.signatures(CallKind::Method, protocol.method)
        else {
            return TypeMatch::NoMatch;
        };

        TypeMatch::any_results(builtin_signatures.iter().map(|builtin_signature| {
            let Some(bound) = bind_signature(builtin_signature, arguments) else {
                return TypeMatch::NoMatch;
            };
            let builtin_match = self.match_bound(builtin_signature, &bound);
            let Some(Some(receiver)) = bound.get(protocol.receiver_argument) else {
                return TypeMatch::NoMatch;
            };
            let mut method_arguments = Vec::with_capacity(bound.len());
            method_arguments.push(ActualArgument::Positional(*receiver));
            method_arguments.extend(bound.iter().enumerate().filter_map(|(index, argument)| {
                if index == protocol.receiver_argument {
                    None
                } else {
                    argument.map(ActualArgument::Positional)
                }
            }));
            builtin_match.all(self.match_signatures(method_signatures, &method_arguments))
        }))
    }

    fn match_signatures(
        &self,
        signatures: &[SupportedSignature],
        arguments: &[ActualArgument<'_, 'db>],
    ) -> TypeMatch {
        TypeMatch::any_results(signatures.iter().map(|signature| {
            let Some(bound) = bind_signature(signature, arguments) else {
                return TypeMatch::NoMatch;
            };
            self.match_bound(signature, &bound)
        }))
    }

    fn match_bound(
        &self,
        signature: &SupportedSignature,
        bound: &[Option<ActualType<'db>>],
    ) -> TypeMatch {
        TypeMatch::all_results(signature.args().iter().zip(bound).filter_map(
            |(expected, actual)| actual.map(|actual| self.match_actual(actual, expected.ty())),
        ))
    }

    fn match_actual(&self, actual: ActualType<'db>, expected: &SupportedTy) -> TypeMatch {
        if let SupportedTy::Module { name, .. } = expected {
            return match actual {
                ActualType::SyntheticModule {
                    name: actual,
                    origin,
                } => {
                    if name != actual {
                        TypeMatch::NoMatch
                    } else {
                        match origin {
                            ModuleOrigin::StandardLibrary | ModuleOrigin::ThirdParty => {
                                TypeMatch::Match
                            }
                            ModuleOrigin::FirstParty
                            | ModuleOrigin::Extra
                            | ModuleOrigin::Other => TypeMatch::NoMatch,
                            ModuleOrigin::Namespace | ModuleOrigin::Unresolved => {
                                TypeMatch::Inconclusive
                            }
                        }
                    }
                }
                ActualType::Native(actual) => {
                    match chalk_receiver_module_relation(self.db, actual, name) {
                        ChalkClassRelation::Match => TypeMatch::Match,
                        ChalkClassRelation::NoMatch => TypeMatch::NoMatch,
                        ChalkClassRelation::Unavailable => TypeMatch::Inconclusive,
                    }
                }
            };
        }

        match actual {
            ActualType::SyntheticModule { .. } => TypeMatch::NoMatch,
            ActualType::Native(actual) => match_supported_type(self.db, actual, expected),
        }
    }
}

fn bind_signature<'db>(
    signature: &SupportedSignature,
    arguments: &[ActualArgument<'_, 'db>],
) -> Option<Vec<Option<ActualType<'db>>>> {
    let parameters = signature.args();
    let mut bound = vec![None; parameters.len()];
    let mut next_positional = 0;
    let mut saw_keyword = false;

    for argument in arguments {
        match *argument {
            ActualArgument::Positional(ty) => {
                if saw_keyword || next_positional == parameters.len() {
                    return None;
                }
                bound[next_positional] = Some(ty);
                next_positional += 1;
            }
            ActualArgument::Keyword { name, ty } => {
                saw_keyword = true;
                let index = parameters
                    .iter()
                    .position(|parameter| parameter.argument_name() == Some(name))?;
                if bound[index].replace(ty).is_some() {
                    return None;
                }
            }
        }
    }

    parameters
        .iter()
        .zip(&bound)
        .all(|(parameter, actual)| parameter.has_default() || actual.is_some())
        .then_some(bound)
}

fn protocol_operation(name: &str) -> Option<ProtocolOperation> {
    PROTOCOL_OPERATIONS
        .iter()
        .copied()
        .find(|operation| operation.builtin == name)
}

fn is_chalk_namespace(module: &str) -> bool {
    ["chalk", "chalkdf"].into_iter().any(|namespace| {
        module == namespace
            || module
                .strip_prefix(namespace)
                .is_some_and(|suffix| suffix.starts_with('.'))
    })
}
