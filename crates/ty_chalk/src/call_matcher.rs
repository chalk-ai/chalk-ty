use ruff_db::files::FileRange;
use ruff_python_ast::name::Name;
use ty_python_semantic::Db;
use ty_python_semantic::chalk::{
    CallDefinitionOriginKind, CallModuleProvenance, CallTarget, ChalkClassRelation, ChalkTypeShape,
    Definition, KnownCallTarget, ModuleOrigin, chalk_call_definition_origin,
    chalk_receiver_module_relation, chalk_type_shape,
};
use ty_python_semantic::types::Type;

use crate::supported_functions::{
    CallKind, SupportedFuncs, SupportedSignature, SupportedTy, current_supported_functions,
};
use crate::type_matcher::{TypeMatch, match_supported_type};

const MAX_CALL_ALTERNATIVES: usize = 256;

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
enum ActualType<'a, 'db> {
    Native(Type<'db>),
    SyntheticModule { name: &'a str, origin: ModuleOrigin },
}

#[derive(Clone, Copy)]
enum ActualArgument<'a, 'db> {
    Positional(ActualType<'a, 'db>),
    Keyword {
        name: &'a str,
        ty: ActualType<'a, 'db>,
    },
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
                Self::classify_module_provenance(provenance, call)
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
        provenance: &'a CallModuleProvenance,
        call: ObservedCall<'a, 'db>,
    ) -> ClassifiedCall<'a, 'db> {
        if !matches!(
            provenance.ownership_origin,
            ModuleOrigin::StandardLibrary | ModuleOrigin::ThirdParty
        ) {
            return ClassifiedCall::Deferred;
        }
        let mut target = CallMatchTarget {
            identity: CallMatchIdentity::ModuleSymbol {
                module: provenance.module.as_ref().into(),
                symbol: Name::new(provenance.symbol.as_str()),
            },
            kind: CallKind::Method,
            name: Name::new(provenance.symbol.as_str()),
            display_label: format!(
                "{}.{}",
                provenance.module.as_ref(),
                provenance.symbol.as_str()
            )
            .into(),
            definition_range: None,
            receiver_parameter: provenance.receiver_parameter,
        };
        if provenance.ownership_origin == ModuleOrigin::ThirdParty
            && is_chalk_namespace(provenance.module.as_ref())
        {
            return ClassifiedCall::Blanket(target);
        }

        if provenance.module.as_ref() == "builtins" {
            target.kind = CallKind::Builtin;
            return ClassifiedCall::Registry {
                protocol: protocol_operation(provenance.symbol.as_str()),
                target,
                arguments: call
                    .arguments
                    .iter()
                    .copied()
                    .map(ActualArgument::from)
                    .collect(),
            };
        }

        target.receiver_parameter = Some(0);
        let receiver = match provenance.receiver_parameter {
            Some(_) => {
                let Some(receiver) = call.receiver else {
                    return ClassifiedCall::Deferred;
                };
                ActualType::Native(receiver)
            }
            None => ActualType::SyntheticModule {
                name: provenance.module.as_ref(),
                origin: provenance.ownership_origin,
            },
        };
        let mut arguments = Vec::with_capacity(call.arguments.len() + 1);
        arguments.push(ActualArgument::Positional(receiver));
        arguments.extend(call.arguments.iter().copied().map(ActualArgument::from));
        ClassifiedCall::Registry {
            target,
            arguments,
            protocol: None,
        }
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
        let display_label: Box<str> = if origin.module.is_empty() {
            origin.qualified_symbol.as_ref().into()
        } else {
            format!("{}.{}", origin.module, origin.qualified_symbol).into()
        };
        let definition_range = Some(origin.definition_range());

        if origin.ownership_origin == ModuleOrigin::ThirdParty
            && is_chalk_namespace(origin.module.as_ref())
        {
            return ClassifiedCall::Blanket(CallMatchTarget {
                identity,
                kind: CallKind::Method,
                name: Name::new(origin.symbol.as_str()),
                display_label,
                definition_range,
                receiver_parameter: (origin.kind == CallDefinitionOriginKind::Method
                    && call.receiver.is_some())
                .then_some(0),
            });
        }

        if origin.kind == CallDefinitionOriginKind::ClassConstructor {
            if origin.ownership_origin != ModuleOrigin::StandardLibrary
                || origin.module.as_ref() != "builtins"
            {
                return ClassifiedCall::Deferred;
            }
            let protocol = protocol_operation(origin.symbol.as_str());
            return ClassifiedCall::Registry {
                target: CallMatchTarget {
                    identity,
                    kind: CallKind::Builtin,
                    name: Name::new(origin.symbol.as_str()),
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

        let kind = origin.kind;
        if kind == CallDefinitionOriginKind::NestedFunction {
            return ClassifiedCall::Deferred;
        }
        if kind == CallDefinitionOriginKind::Method && call.receiver.is_none() {
            return ClassifiedCall::Deferred;
        }

        let is_builtin = origin.ownership_origin == ModuleOrigin::StandardLibrary
            && origin.module.as_ref() == "builtins"
            && kind == CallDefinitionOriginKind::TopLevelFunction;
        let protocol = is_builtin
            .then(|| protocol_operation(origin.symbol.as_str()))
            .flatten();
        let target = CallMatchTarget {
            identity,
            kind: if !is_builtin {
                CallKind::Method
            } else {
                CallKind::Builtin
            },
            name: Name::new(origin.symbol.as_str()),
            display_label,
            definition_range,
            receiver_parameter: (!is_builtin).then_some(0),
        };

        if matches!(
            origin.ownership_origin,
            ModuleOrigin::FirstParty | ModuleOrigin::Extra | ModuleOrigin::Other
        ) {
            return ClassifiedCall::MissingRegistryEntry(target);
        }
        if !matches!(
            origin.ownership_origin,
            ModuleOrigin::StandardLibrary | ModuleOrigin::ThirdParty
        ) {
            return ClassifiedCall::Deferred;
        }

        let mut arguments = Vec::with_capacity(call.arguments.len() + 1);
        if target.kind == CallKind::Method && protocol.is_none() {
            let receiver = match (kind, call.receiver) {
                (_, Some(receiver)) => ActualType::Native(receiver),
                (CallDefinitionOriginKind::TopLevelFunction, None) => ActualType::SyntheticModule {
                    name: origin.module.as_ref(),
                    origin: origin.ownership_origin,
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

    fn match_protocol<'a>(
        &self,
        protocol: ProtocolOperation,
        arguments: &[ActualArgument<'a, 'db>],
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

    fn match_signatures<'a>(
        &self,
        signatures: &[SupportedSignature],
        arguments: &[ActualArgument<'a, 'db>],
    ) -> TypeMatch {
        let Some(alternatives) = self.argument_alternatives(arguments) else {
            return TypeMatch::Inconclusive;
        };
        TypeMatch::all_results(alternatives.iter().map(|arguments| {
            TypeMatch::any_results(signatures.iter().map(|signature| {
                let Some(bound) = bind_signature(signature, arguments) else {
                    return TypeMatch::NoMatch;
                };
                self.match_bound(signature, &bound)
            }))
        }))
    }

    fn argument_alternatives<'a>(
        &self,
        arguments: &[ActualArgument<'a, 'db>],
    ) -> Option<Vec<Vec<ActualArgument<'a, 'db>>>> {
        let mut combinations = vec![Vec::with_capacity(arguments.len())];
        for argument in arguments {
            let (types, name) = match *argument {
                ActualArgument::Positional(ty) => (self.actual_type_alternatives(ty)?, None),
                ActualArgument::Keyword { name, ty } => {
                    (self.actual_type_alternatives(ty)?, Some(name))
                }
            };
            if combinations.len().checked_mul(types.len())? > MAX_CALL_ALTERNATIVES {
                return None;
            }

            let mut expanded = Vec::with_capacity(combinations.len() * types.len());
            for combination in &combinations {
                for ty in &types {
                    let mut combination = combination.clone();
                    combination.push(match name {
                        Some(name) => ActualArgument::Keyword { name, ty: *ty },
                        None => ActualArgument::Positional(*ty),
                    });
                    expanded.push(combination);
                }
            }
            combinations = expanded;
        }
        Some(combinations)
    }

    fn actual_type_alternatives<'a>(
        &self,
        actual: ActualType<'a, 'db>,
    ) -> Option<Vec<ActualType<'a, 'db>>> {
        let ActualType::Native(actual) = actual else {
            return Some(vec![actual]);
        };
        let mut pending = vec![(actual, 0)];
        let mut alternatives = Vec::new();
        while let Some((actual, depth)) = pending.pop() {
            if depth >= MAX_CALL_ALTERNATIVES {
                return None;
            }
            match chalk_type_shape(self.db, actual) {
                ChalkTypeShape::Expanded(expanded) => pending.push((expanded, depth + 1)),
                ChalkTypeShape::Union(elements) => {
                    if alternatives.len() + pending.len() + elements.len() > MAX_CALL_ALTERNATIVES {
                        return None;
                    }
                    pending.extend(elements.iter().map(|element| (*element, depth + 1)));
                }
                _ => alternatives.push(ActualType::Native(actual)),
            }
        }
        Some(alternatives)
    }

    fn match_bound<'a>(
        &self,
        signature: &SupportedSignature,
        bound: &[Option<ActualType<'a, 'db>>],
    ) -> TypeMatch {
        TypeMatch::all_results(signature.args().iter().zip(bound).filter_map(
            |(expected, actual)| actual.map(|actual| self.match_actual(actual, expected.ty())),
        ))
    }

    fn match_actual(&self, actual: ActualType<'_, 'db>, expected: &SupportedTy) -> TypeMatch {
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

fn bind_signature<'a, 'db>(
    signature: &SupportedSignature,
    arguments: &[ActualArgument<'a, 'db>],
) -> Option<Vec<Option<ActualType<'a, 'db>>>> {
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

#[cfg(test)]
mod tests {
    use ruff_db::files::{File, system_path_to_file};
    use ruff_db::parsed::parsed_module;
    use ruff_db::system::{
        DbWithTestSystem as _, DbWithWritableSystem as _, SystemPath, SystemPathBuf,
    };
    use ruff_python_ast as ast;
    use ty_project::{ProjectMetadata, TestDb};
    use ty_python_semantic::{HasType, SemanticModel};

    use super::{
        CallKind, CallMatch, CallMatchIdentity, CallNoMatchReason, KnownCallTarget,
        ObservedArgument, ObservedCall, ObservedCallTarget, match_call_target,
    };

    #[derive(Debug, Eq, PartialEq)]
    enum IdentitySummary {
        Definition,
        Known(KnownCallTarget),
        ModuleSymbol { module: String, symbol: String },
    }

    #[derive(Debug, Eq, PartialEq)]
    enum MatchSummary {
        Match {
            identity: IdentitySummary,
            kind: CallKind,
            name: String,
            receiver_parameter: Option<u32>,
        },
        NoMatch {
            identity: IdentitySummary,
            kind: CallKind,
            name: String,
            receiver_parameter: Option<u32>,
            reason: CallNoMatchReason,
        },
        Inconclusive,
    }

    fn setup(main: &str, files: &[(&str, &str)]) -> (TestDb, File) {
        setup_at("/main.py", main, files)
    }

    fn setup_at(main_path: &str, main: &str, files: &[(&str, &str)]) -> (TestDb, File) {
        let project = ProjectMetadata::new("test", SystemPathBuf::from("/"));
        let mut db = TestDb::new(project);
        db.init_program().unwrap();

        for (path, source) in files {
            if let Some(parent) = SystemPath::new(path).parent() {
                db.memory_file_system()
                    .create_directory_all(parent)
                    .unwrap();
            }
            db.write_file(SystemPath::new(path), source).unwrap();
        }
        if let Some(parent) = SystemPath::new(main_path).parent() {
            db.memory_file_system()
                .create_directory_all(parent)
                .unwrap();
        }
        db.write_file(SystemPath::new(main_path), main).unwrap();
        let file = system_path_to_file(&db, main_path).unwrap();
        (db, file)
    }

    fn call_matches(db: &TestDb, file: File, call_index: usize) -> Vec<CallMatch<'_>> {
        let parsed = parsed_module(db, file).load(db);
        let call = parsed
            .syntax()
            .body
            .iter()
            .filter_map(ast::Stmt::as_expr_stmt)
            .filter_map(|statement| statement.value.as_call_expr())
            .nth(call_index)
            .unwrap();
        let model = SemanticModel::new(db, file);
        let arguments = call
            .arguments
            .iter_source_order()
            .map(|argument| match argument {
                ast::ArgOrKeyword::Arg(argument) => {
                    ObservedArgument::Positional(argument.inferred_type(&model).unwrap())
                }
                ast::ArgOrKeyword::Keyword(keyword) => ObservedArgument::Keyword {
                    name: keyword.arg.as_ref().unwrap().id.as_str(),
                    ty: keyword.value.inferred_type(&model).unwrap(),
                },
            })
            .collect::<Vec<_>>();
        let receiver = call
            .func
            .as_attribute_expr()
            .and_then(|attribute| attribute.value.inferred_type(&model));
        let targets = model.chalk_call_targets(call);
        let module_provenance = model.chalk_call_module_provenance(call);
        let mut matches = targets
            .targets
            .iter()
            .map(|target| {
                match_call_target(
                    db,
                    ObservedCall {
                        target: ObservedCallTarget::Resolved(*target),
                        arguments: &arguments,
                        receiver,
                    },
                )
            })
            .chain(targets.known_targets.iter().map(|target| {
                match_call_target(
                    db,
                    ObservedCall {
                        target: ObservedCallTarget::Known(*target),
                        arguments: &arguments,
                        receiver,
                    },
                )
            }))
            .collect::<Vec<_>>();
        matches.extend(module_provenance.iter().map(|provenance| {
            match_call_target(
                db,
                ObservedCall {
                    target: ObservedCallTarget::ModuleProvenance(provenance),
                    arguments: &arguments,
                    receiver,
                },
            )
        }));
        if targets.has_unresolved {
            matches.push(match_call_target(
                db,
                ObservedCall {
                    target: ObservedCallTarget::Deferred,
                    arguments: &arguments,
                    receiver,
                },
            ));
        }
        let mut unique = Vec::with_capacity(matches.len());
        for result in matches {
            if !unique.contains(&result) {
                unique.push(result);
            }
        }
        unique
    }

    fn has_match(matches: &[CallMatch<'_>], kind: CallKind, name: &str) -> bool {
        matches.iter().any(|result| {
            matches!(
                result,
                CallMatch::Match(target) if target.kind == kind && target.name.as_str() == name
            )
        })
    }

    fn has_no_match(
        matches: &[CallMatch<'_>],
        kind: CallKind,
        name: &str,
        reason: CallNoMatchReason,
    ) -> bool {
        matches.iter().any(|result| {
            matches!(
                result,
                CallMatch::NoMatch {
                    target,
                    reason: actual_reason,
                } if target.kind == kind
                    && target.name.as_str() == name
                    && *actual_reason == reason
            )
        })
    }

    #[test]
    fn builtin_match_type_mismatch_and_missing_binding() {
        let (db, file) = setup(
            r#"
abs(1)
abs("x")
abs()
"#,
            &[],
        );

        assert!(has_match(
            &call_matches(&db, file, 0),
            CallKind::Builtin,
            "abs"
        ));
        for index in [1, 2] {
            assert!(has_no_match(
                &call_matches(&db, file, index),
                CallKind::Builtin,
                "abs",
                CallNoMatchReason::SignatureMismatch,
            ));
        }
    }

    fn summarize(matches: Vec<CallMatch<'_>>) -> Vec<MatchSummary> {
        fn identity(identity: CallMatchIdentity<'_>) -> IdentitySummary {
            match identity {
                CallMatchIdentity::Definition(_) => IdentitySummary::Definition,
                CallMatchIdentity::Known(known) => IdentitySummary::Known(known),
                CallMatchIdentity::ModuleSymbol { module, symbol } => {
                    IdentitySummary::ModuleSymbol {
                        module: module.into(),
                        symbol: symbol.as_str().into(),
                    }
                }
            }
        }

        matches
            .into_iter()
            .map(|result| match result {
                CallMatch::Match(target) => MatchSummary::Match {
                    identity: identity(target.identity),
                    kind: target.kind,
                    name: target.name.as_str().into(),
                    receiver_parameter: target.receiver_parameter,
                },
                CallMatch::NoMatch { target, reason } => MatchSummary::NoMatch {
                    identity: identity(target.identity),
                    kind: target.kind,
                    name: target.name.as_str().into(),
                    receiver_parameter: target.receiver_parameter,
                    reason,
                },
                CallMatch::Inconclusive => MatchSummary::Inconclusive,
            })
            .collect()
    }

    #[test]
    fn imported_alias_and_module_qualified_function_use_semantic_module_receiver() {
        let (db, file) = setup(
            r#"
import math
from math import sqrt as square_root

square_root(4.0)
math.sqrt(4.0)
"#,
            &[],
        );

        for index in 0..2 {
            assert_eq!(
                summarize(call_matches(&db, file, index)),
                [MatchSummary::Match {
                    identity: IdentitySummary::Definition,
                    kind: CallKind::Method,
                    name: "sqrt".into(),
                    receiver_parameter: Some(0),
                }]
            );
        }
    }

    #[test]
    fn bound_static_and_class_method_receivers_are_concrete() {
        let (db, file) = setup(
            r#"
import datetime

class C:
    def bound(self, value: int) -> int:
        return value

    @staticmethod
    def static(value: int) -> int:
        return value

    @classmethod
    def class_method(cls, value: int) -> int:
        return value

class DerivedDateTime(datetime.datetime): ...

C().bound(1)
C.static(1)
C.class_method(1)
datetime.datetime.now()
DerivedDateTime.now()
DerivedDateTime(2024, 1, 1).now()
"#,
            &[],
        );

        for (index, name) in ["bound", "static", "class_method"].into_iter().enumerate() {
            assert_eq!(
                summarize(call_matches(&db, file, index)),
                [MatchSummary::NoMatch {
                    identity: IdentitySummary::Definition,
                    kind: CallKind::Method,
                    name: name.into(),
                    receiver_parameter: Some(0),
                    reason: CallNoMatchReason::MissingRegistryEntry,
                }]
            );
        }
        for index in 3..6 {
            assert_eq!(
                summarize(call_matches(&db, file, index)),
                [MatchSummary::Match {
                    identity: IdentitySummary::Definition,
                    kind: CallKind::Method,
                    name: "now".into(),
                    receiver_parameter: Some(0),
                }]
            );
        }
    }

    #[test]
    fn missing_external_entry_and_per_target_alternatives() {
        let (db, file) = setup(
            r#"
from math import sqrt
from external import unsupported

unsupported(1.0)
(sqrt if flag else unsupported)(4.0)
"#,
            &[(
                "/external.py",
                "def unsupported(value: float) -> float: ...\n",
            )],
        );

        assert_eq!(
            summarize(call_matches(&db, file, 0)),
            [MatchSummary::NoMatch {
                identity: IdentitySummary::Definition,
                kind: CallKind::Method,
                name: "unsupported".into(),
                receiver_parameter: Some(0),
                reason: CallNoMatchReason::MissingRegistryEntry,
            }]
        );

        assert_eq!(
            summarize(call_matches(&db, file, 1)),
            [
                MatchSummary::Match {
                    identity: IdentitySummary::Definition,
                    kind: CallKind::Method,
                    name: "sqrt".into(),
                    receiver_parameter: Some(0),
                },
                MatchSummary::NoMatch {
                    identity: IdentitySummary::Definition,
                    kind: CallKind::Method,
                    name: "unsupported".into(),
                    receiver_parameter: Some(0),
                    reason: CallNoMatchReason::MissingRegistryEntry,
                },
            ]
        );
    }

    #[test]
    fn named_defaults_and_binding_failures() {
        let (db, file) = setup(
            r#"
round(number=1)
round(number=1, ndigits=2)
round(1, 2, 3)
round(1, number=2)
round(value=1)
"#,
            &[],
        );

        for index in 0..2 {
            assert_eq!(
                summarize(call_matches(&db, file, index)),
                [MatchSummary::Match {
                    identity: IdentitySummary::Definition,
                    kind: CallKind::Builtin,
                    name: "round".into(),
                    receiver_parameter: None,
                }]
            );
        }
        for index in 2..5 {
            assert_eq!(
                summarize(call_matches(&db, file, index)),
                [MatchSummary::NoMatch {
                    identity: IdentitySummary::Definition,
                    kind: CallKind::Builtin,
                    name: "round".into(),
                    receiver_parameter: None,
                    reason: CallNoMatchReason::SignatureMismatch,
                }]
            );
        }
    }

    #[test]
    fn positional_after_keyword_is_a_binding_mismatch() {
        let (db, file) = setup("round(1, 2)\n", &[]);
        let parsed = parsed_module(&db, file).load(&db);
        let call = parsed
            .syntax()
            .body
            .first()
            .and_then(ast::Stmt::as_expr_stmt)
            .and_then(|statement| statement.value.as_call_expr())
            .unwrap();
        let model = SemanticModel::new(&db, file);
        let types = call
            .arguments
            .args
            .iter()
            .map(|argument| argument.inferred_type(&model).unwrap())
            .collect::<Vec<_>>();
        let target = model.chalk_call_targets(call).targets[0];

        let result = match_call_target(
            &db,
            ObservedCall {
                target: ObservedCallTarget::Resolved(target),
                arguments: &[
                    ObservedArgument::Keyword {
                        name: "number",
                        ty: types[0],
                    },
                    ObservedArgument::Positional(types[1]),
                ],
                receiver: None,
            },
        );
        assert!(matches!(
            result,
            CallMatch::NoMatch {
                reason: CallNoMatchReason::SignatureMismatch,
                ..
            }
        ));
    }

    #[test]
    fn bool_and_len_retain_source_call_identity_while_matching_protocol_methods() {
        let (db, file) = setup(
            r#"
bool("value")
bool(object())
bool()
len("value")
len(1)
"#,
            &[],
        );

        assert!(has_match(
            &call_matches(&db, file, 0),
            CallKind::Builtin,
            "bool"
        ));
        for index in [1, 2] {
            assert!(has_no_match(
                &call_matches(&db, file, index),
                CallKind::Builtin,
                "bool",
                CallNoMatchReason::SignatureMismatch,
            ));
        }
        assert!(has_match(
            &call_matches(&db, file, 3),
            CallKind::Builtin,
            "len"
        ));
        assert!(has_no_match(
            &call_matches(&db, file, 4),
            CallKind::Builtin,
            "len",
            CallNoMatchReason::SignatureMismatch,
        ));
    }

    #[test]
    fn known_startswith_uses_ordinary_method_registry_path() {
        let (db, file) = setup(
            r#"
"abc".startswith("a")
"abc".startswith(1)
"#,
            &[],
        );

        assert_eq!(
            summarize(call_matches(&db, file, 0)),
            [MatchSummary::Match {
                identity: IdentitySummary::Known(KnownCallTarget::StrStartswith),
                kind: CallKind::Method,
                name: "startswith".into(),
                receiver_parameter: Some(0),
            }]
        );
        assert_eq!(
            summarize(call_matches(&db, file, 1)),
            [MatchSummary::NoMatch {
                identity: IdentitySummary::Known(KnownCallTarget::StrStartswith),
                kind: CallKind::Method,
                name: "startswith".into(),
                receiver_parameter: Some(0),
                reason: CallNoMatchReason::SignatureMismatch,
            }]
        );
    }

    #[test]
    fn targets_retain_deterministic_labels_and_definition_locations() {
        let (db, file) = setup(
            r#"
import datetime
from chalk.functions import if_then_else, missing
from math import sqrt

sqrt(4.0)
bool("value")
len("value")
"abc".startswith("a")
datetime.datetime.now()
if_then_else(True, 1, 0)
missing()
"#,
            &[],
        );

        for (index, label, has_definition_range) in [
            (0, "math.sqrt", true),
            (1, "builtins.bool", true),
            (2, "builtins.len", true),
            (3, "str.startswith", false),
            (4, "datetime.datetime.now", true),
            (5, "chalk.functions.if_then_else", true),
            (6, "chalk.functions.missing", false),
        ] {
            let matches = call_matches(&db, file, index);
            let target = matches
                .iter()
                .find_map(|result| match result {
                    CallMatch::Match(target) | CallMatch::NoMatch { target, .. } => Some(target),
                    CallMatch::Inconclusive => None,
                })
                .unwrap();
            assert_eq!(target.display_label.as_ref(), label, "call {index}");
            assert_eq!(
                target.definition_range.is_some(),
                has_definition_range,
                "call {index}"
            );
        }
    }

    #[test]
    fn first_party_chalk_packages_do_not_gain_blanket_support_from_their_name() {
        let (db, file) = setup_at(
            "/chalk/__init__.py",
            r#"
def custom(value): ...

class Model:
    def custom_method(self, value): ...

custom(object())
Model().custom_method(object())
Model()
"#,
            &[],
        );

        for (index, name) in ["custom", "custom_method"].into_iter().enumerate() {
            assert_eq!(
                summarize(call_matches(&db, file, index)),
                [MatchSummary::NoMatch {
                    identity: IdentitySummary::Definition,
                    kind: CallKind::Method,
                    name: name.into(),
                    receiver_parameter: Some(0),
                    reason: CallNoMatchReason::MissingRegistryEntry,
                }]
            );
        }
        assert_eq!(
            summarize(call_matches(&db, file, 2)),
            [MatchSummary::Inconclusive]
        );
    }

    #[test]
    fn first_party_shadow_modules_do_not_claim_stdlib_or_chalk_registry_ownership() {
        let (math_db, math_file) = setup_at(
            "/math.py",
            r#"
def sqrt(value: float) -> float: ...
sqrt(4.0)
"#,
            &[],
        );
        assert_eq!(
            summarize(call_matches(&math_db, math_file, 0)),
            [MatchSummary::NoMatch {
                identity: IdentitySummary::Definition,
                kind: CallKind::Method,
                name: "sqrt".into(),
                receiver_parameter: Some(0),
                reason: CallNoMatchReason::MissingRegistryEntry,
            }]
        );

        let (chalk_db, chalk_file) = setup_at(
            "/chalk.py",
            "def custom(value): ...\ncustom(object())\n",
            &[],
        );
        assert_eq!(
            summarize(call_matches(&chalk_db, chalk_file, 0)),
            [MatchSummary::NoMatch {
                identity: IdentitySummary::Definition,
                kind: CallKind::Method,
                name: "custom".into(),
                receiver_parameter: Some(0),
                reason: CallNoMatchReason::MissingRegistryEntry,
            }]
        );
    }

    #[test]
    fn local_runtime_shadow_does_not_match_through_vendored_stdlib_stub() {
        let (db, file) = setup(
            "import datetime\n\ndatetime.datetime.now()\n",
            &[(
                "/datetime.py",
                "class datetime:\n    @classmethod\n    def now(cls): ...\n",
            )],
        );

        assert_eq!(
            summarize(call_matches(&db, file, 0)),
            [MatchSummary::NoMatch {
                identity: IdentitySummary::Definition,
                kind: CallKind::Method,
                name: "now".into(),
                receiver_parameter: Some(0),
                reason: CallNoMatchReason::MissingRegistryEntry,
            }]
        );
    }

    #[test]
    fn constructors_require_registered_builtins_and_defer_general_classes() {
        let (db, file) = setup(
            r#"
from builtins import int as Integer
from external import External

class Outer:
    class Inner: ...

int("1")
Integer("1")
dict()
External()
Outer.Inner()
"#,
            &[("/external.py", "class External: ...\n")],
        );

        for index in 0..2 {
            assert_eq!(
                summarize(call_matches(&db, file, index)),
                [MatchSummary::Match {
                    identity: IdentitySummary::Definition,
                    kind: CallKind::Builtin,
                    name: "int".into(),
                    receiver_parameter: None,
                }]
            );
        }
        assert_eq!(
            summarize(call_matches(&db, file, 2)),
            [MatchSummary::NoMatch {
                identity: IdentitySummary::Definition,
                kind: CallKind::Builtin,
                name: "dict".into(),
                receiver_parameter: None,
                reason: CallNoMatchReason::MissingRegistryEntry,
            }]
        );
        for index in 3..5 {
            assert_eq!(
                summarize(call_matches(&db, file, index)),
                [MatchSummary::Inconclusive]
            );
        }
    }

    #[test]
    fn bound_method_alias_without_a_receiver_is_inconclusive() {
        let (db, file) = setup(
            r#"
import datetime

now = datetime.datetime.now
now()
"#,
            &[],
        );

        assert_eq!(
            summarize(call_matches(&db, file, 0)),
            [MatchSummary::Inconclusive]
        );
    }

    #[test]
    fn dynamic_targets_and_unknown_components_are_inconclusive() {
        let (db, file) = setup(
            r#"
from typing import Any

def unresolved(value: Any):
    return value

unresolved(1)(2)

value: Any
len(value)
"#,
            &[],
        );

        assert!(
            call_matches(&db, file, 0)
                .iter()
                .any(|result| result == &CallMatch::Inconclusive)
        );
        assert!(
            call_matches(&db, file, 1)
                .iter()
                .any(|result| result == &CallMatch::Inconclusive)
        );
    }
}
