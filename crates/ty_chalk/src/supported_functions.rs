use std::{collections::BTreeMap, sync::OnceLock};

#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, salsa::Update, get_size2::GetSize,
)]
pub(crate) enum CallKind {
    Builtin,
    Method,
    Attribute,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct SupportedCall {
    kind: CallKind,
    name: String,
}

impl SupportedCall {
    #[must_use]
    pub(crate) fn new(kind: CallKind, name: impl Into<String>) -> Self {
        Self {
            kind,
            name: name.into(),
        }
    }

    #[must_use]
    fn kind(&self) -> CallKind {
        self.kind
    }

    #[must_use]
    fn name(&self) -> &str {
        &self.name
    }
}

#[derive(Clone, Debug)]
pub(crate) struct SupportedFuncs {
    impls: BTreeMap<SupportedCall, Box<[SupportedSignature]>>,
}

impl SupportedFuncs {
    fn from_impls(impls: BTreeMap<SupportedCall, Vec<SupportedSignature>>) -> Self {
        Self {
            impls: impls
                .into_iter()
                .map(|(call, signatures)| (call, signatures.into_boxed_slice()))
                .collect(),
        }
    }

    #[must_use]
    #[cfg(test)]
    fn len(&self) -> usize {
        self.impls.len()
    }

    #[must_use]
    pub(crate) fn signatures(&self, kind: CallKind, name: &str) -> Option<&[SupportedSignature]> {
        self.impls
            .get(&SupportedCall::new(kind, name))
            .map(Box::as_ref)
    }

    #[cfg(test)]
    pub(crate) fn entries(
        &self,
    ) -> impl ExactSizeIterator<Item = (&SupportedCall, &[SupportedSignature])> {
        self.impls
            .iter()
            .map(|(call, signatures)| (call, signatures.as_ref()))
    }
}

#[derive(Clone, Debug)]
pub(crate) struct SupportedSignature {
    args: Box<[SupportedArg]>,
}

impl SupportedSignature {
    #[must_use]
    pub(crate) fn args(&self) -> &[SupportedArg] {
        &self.args
    }
}

#[derive(Clone, Debug)]
pub(crate) struct SupportedArg {
    ty: SupportedTy,
    argument_name: Option<String>,
    has_default: bool,
}

impl SupportedArg {
    #[must_use]
    pub(crate) const fn ty(&self) -> &SupportedTy {
        &self.ty
    }

    #[must_use]
    pub(crate) fn argument_name(&self) -> Option<&str> {
        self.argument_name.as_deref()
    }

    #[must_use]
    pub(crate) const fn has_default(&self) -> bool {
        self.has_default
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum SupportedTy {
    Any {
        nullable: bool,
    },
    Bool {
        nullable: bool,
    },
    Bytes {
        nullable: bool,
    },
    Class {
        nullable: bool,
        module: String,
        name: String,
    },
    Counter {
        nullable: bool,
        items: Box<SupportedTy>,
    },
    Date {
        nullable: bool,
    },
    DateTime {
        nullable: bool,
    },
    Dict {
        nullable: bool,
        key_type: Box<SupportedTy>,
        value_type: Box<SupportedTy>,
    },
    Float {
        nullable: bool,
    },
    FrozenSet {
        nullable: bool,
        items: Box<SupportedTy>,
    },
    Generator {
        nullable: bool,
        items: Box<SupportedTy>,
    },
    HashlibHash {
        nullable: bool,
    },
    Int {
        nullable: bool,
    },
    Iterable {
        nullable: bool,
        items: Box<SupportedTy>,
    },
    Json {
        nullable: bool,
    },
    List {
        nullable: bool,
        items: Box<SupportedTy>,
    },
    Module {
        nullable: bool,
        name: String,
    },
    None {
        nullable: bool,
    },
    ReMatch {
        nullable: bool,
    },
    RePattern {
        nullable: bool,
    },
    RequestsHttpResponse {
        nullable: bool,
    },
    Set {
        nullable: bool,
        items: Box<SupportedTy>,
    },
    SequenceMatcher {
        nullable: bool,
    },
    Str {
        nullable: bool,
    },
    SubClassOf {
        ty_name: String,
        match_nullable: bool,
    },
    Time {
        nullable: bool,
    },
    Timedelta {
        nullable: bool,
    },
    TimeZone {
        nullable: bool,
    },
    Tuple {
        nullable: bool,
        items: Vec<SupportedTy>,
        is_variable: bool,
    },
    Other {
        nullable: bool,
        name: String,
    },
}

impl SupportedTy {
    pub(crate) const fn accepts_none(&self) -> bool {
        match self {
            Self::Any { nullable }
            | Self::Bool { nullable }
            | Self::Bytes { nullable }
            | Self::Class { nullable, .. }
            | Self::Counter { nullable, .. }
            | Self::Date { nullable }
            | Self::DateTime { nullable }
            | Self::Dict { nullable, .. }
            | Self::Float { nullable }
            | Self::FrozenSet { nullable, .. }
            | Self::Generator { nullable, .. }
            | Self::HashlibHash { nullable }
            | Self::Int { nullable }
            | Self::Iterable { nullable, .. }
            | Self::Json { nullable }
            | Self::List { nullable, .. }
            | Self::Module { nullable, .. }
            | Self::ReMatch { nullable }
            | Self::RePattern { nullable }
            | Self::RequestsHttpResponse { nullable }
            | Self::Set { nullable, .. }
            | Self::SequenceMatcher { nullable }
            | Self::Str { nullable }
            | Self::Time { nullable }
            | Self::Timedelta { nullable }
            | Self::TimeZone { nullable }
            | Self::Tuple { nullable, .. }
            | Self::Other { nullable, .. } => *nullable,
            Self::None { .. } => true,
            Self::SubClassOf { match_nullable, .. } => *match_nullable,
        }
    }
}

#[path = "supported_functions/current_snapshot.rs"]
mod current_snapshot;
#[path = "supported_functions/presentation.rs"]
mod presentation;

pub(crate) use presentation::present_supported_signatures;

static CURRENT_SUPPORTED_FUNCS: OnceLock<SupportedFuncs> = OnceLock::new();

#[must_use]
pub(crate) fn current_supported_functions() -> &'static SupportedFuncs {
    CURRENT_SUPPORTED_FUNCS.get_or_init(current_snapshot::supported_funcs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_snapshot_counts() {
        let funcs = current_supported_functions();
        assert_eq!(funcs.len(), 306);
        assert_eq!(
            funcs
                .entries()
                .map(|(_, signatures)| signatures.len())
                .sum::<usize>(),
            884
        );
    }

    #[test]
    fn current_snapshot_representative_metadata() {
        let funcs = current_supported_functions();

        let len = funcs.signatures(CallKind::Builtin, "len").unwrap();
        assert!(matches!(
            len[0].args(),
            [SupportedArg {
                ty: SupportedTy::Any { nullable: true },
                argument_name: None,
                has_default: false,
            }]
        ));

        let md5 = funcs.signatures(CallKind::Method, "md5").unwrap();
        let used_for_security = &md5[0].args()[2];
        assert_eq!(used_for_security.argument_name(), Some("usedforsecurity"));
        assert!(used_for_security.has_default());

        let value = funcs.signatures(CallKind::Attribute, "value").unwrap();
        assert!(matches!(
            value[0].args(),
            [SupportedArg {
                ty: SupportedTy::SubClassOf {
                    ty_name,
                    match_nullable: false,
                },
                ..
            }] if ty_name == "TyEnum"
        ));

        let eq = funcs.signatures(CallKind::Method, "__eq__").unwrap();
        assert!(eq.iter().any(|signature| {
            matches!(
                signature.args(),
                [
                    SupportedArg {
                        ty: SupportedTy::Int { nullable: true },
                        ..
                    },
                    SupportedArg {
                        ty: SupportedTy::Int { nullable: true },
                        ..
                    },
                ]
            )
        }));
    }
}
