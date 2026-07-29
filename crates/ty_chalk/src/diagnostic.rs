use std::borrow::Cow;
use std::cmp::Ordering;

use ruff_db::files::{File, FileRange};
use ruff_db::source::source_text;
use ruff_text_size::{Ranged, TextRange};

use crate::CallNoMatchReason;
use crate::active_project::ChalkProjectInput;
use crate::reachability::{UnsupportedCallCandidate, project_candidates};
use crate::supported_functions::{
    SupportedCall, current_supported_functions, present_supported_signatures,
};
use crate::suppression::{InvalidSuppressionReason, SuppressionProblemKind};

/// Keeps the source excerpt useful as a short inline diagnostic detail.
const MAX_OBSERVED_CALL_CHARACTERS: usize = 160;

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct ChalkDiagnostic {
    file: File,
    range: TextRange,
    kind: ChalkDiagnosticKind,
}

impl ChalkDiagnostic {
    pub fn file(&self) -> File {
        self.file
    }

    pub fn range(&self) -> TextRange {
        self.range
    }

    pub fn kind(&self) -> &ChalkDiagnosticKind {
        &self.kind
    }

    pub fn unsupported_function_details(&self) -> Option<&UnsupportedFunctionDetails> {
        let ChalkDiagnosticKind::UnsupportedFunction(details) = &self.kind else {
            return None;
        };
        Some(details)
    }

    pub fn severity(&self) -> ChalkDiagnosticSeverity {
        match self.kind {
            ChalkDiagnosticKind::ResolverCycle => ChalkDiagnosticSeverity::Error,
            ChalkDiagnosticKind::UnsupportedFunction(_)
            | ChalkDiagnosticKind::UnknownSuppression { .. }
            | ChalkDiagnosticKind::InvalidSuppression { .. } => ChalkDiagnosticSeverity::Warning,
        }
    }

    pub fn code(&self) -> &'static str {
        match self.kind {
            ChalkDiagnosticKind::UnsupportedFunction(_) => "unsupported-function",
            ChalkDiagnosticKind::ResolverCycle => "resolver-cycle",
            ChalkDiagnosticKind::UnknownSuppression { .. } => "unknown-chalk-suppression",
            ChalkDiagnosticKind::InvalidSuppression { .. } => "invalid-chalk-suppression",
        }
    }

    pub fn message(&self) -> Cow<'_, str> {
        match &self.kind {
            ChalkDiagnosticKind::UnsupportedFunction(_) => {
                Cow::Borrowed("Call is not supported by the static accelerator")
            }
            ChalkDiagnosticKind::ResolverCycle => {
                Cow::Borrowed("Resolver call graph contains a cycle")
            }
            ChalkDiagnosticKind::UnknownSuppression { code } => {
                Cow::Owned(format!("Unknown Chalk suppression code: {code}"))
            }
            ChalkDiagnosticKind::InvalidSuppression { reason } => Cow::Owned(format!(
                "Invalid Chalk suppression directive: {}",
                invalid_suppression_reason(*reason)
            )),
        }
    }
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum ChalkDiagnosticKind {
    UnsupportedFunction(UnsupportedFunctionDetails),
    ResolverCycle,
    UnknownSuppression { code: Box<str> },
    InvalidSuppression { reason: InvalidSuppressionReason },
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct UnsupportedFunctionDetails {
    targets: Box<[UnsupportedTargetDetail]>,
    observed_call: Option<Box<str>>,
    supported_signatures: Box<[Box<str>]>,
}

impl UnsupportedFunctionDetails {
    pub fn targets(&self) -> &[UnsupportedTargetDetail] {
        &self.targets
    }

    pub fn observed_call(&self) -> Option<&str> {
        self.observed_call.as_deref()
    }

    pub fn supported_signatures(&self) -> &[Box<str>] {
        &self.supported_signatures
    }
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct UnsupportedTargetDetail {
    label: Box<str>,
    reason: CallNoMatchReason,
    location: Option<FileRange>,
}

impl UnsupportedTargetDetail {
    pub fn label(&self) -> &str {
        &self.label
    }

    pub fn reason(&self) -> CallNoMatchReason {
        self.reason
    }

    pub fn location(&self) -> Option<FileRange> {
        self.location
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ChalkDiagnosticSeverity {
    Warning,
    Error,
}

pub fn chalk_diagnostics_for_file(
    db: &dyn ty_project::Db,
    project: ChalkProjectInput,
    file: File,
) -> Vec<ChalkDiagnostic> {
    if !project.source_files(db).contains(&file) {
        return Vec::new();
    }

    let candidates = project_candidates(db, project);
    let source = source_text(db, file);
    let mut diagnostics = Vec::new();
    diagnostics.extend(
        candidates
            .unsupported_for_file(file)
            .map(|candidate| ChalkDiagnostic {
                file,
                range: candidate.range,
                kind: ChalkDiagnosticKind::UnsupportedFunction(unsupported_function_details(
                    db, candidate, &source,
                )),
            }),
    );
    diagnostics.extend(
        candidates
            .cycles_for_file(file)
            .map(|candidate| ChalkDiagnostic {
                file,
                range: candidate.range,
                kind: ChalkDiagnosticKind::ResolverCycle,
            }),
    );

    diagnostics.extend(
        candidates
            .suppression_problems_for_file(file)
            .map(|problem| ChalkDiagnostic {
                file,
                range: problem.range,
                kind: match problem.kind {
                    SuppressionProblemKind::Invalid(reason) => {
                        ChalkDiagnosticKind::InvalidSuppression { reason }
                    }
                    SuppressionProblemKind::UnknownCode => {
                        ChalkDiagnosticKind::UnknownSuppression {
                            code: source[problem.range].into(),
                        }
                    }
                },
            }),
    );
    diagnostics.sort_by(|left, right| {
        left.range
            .start()
            .cmp(&right.range.start())
            .then_with(|| left.range.end().cmp(&right.range.end()))
            .then_with(|| left.code().cmp(right.code()))
    });
    diagnostics
}

fn unsupported_function_details(
    db: &dyn ty_project::Db,
    candidate: &UnsupportedCallCandidate<'_>,
    source: &str,
) -> UnsupportedFunctionDetails {
    let mut targets = candidate
        .targets
        .iter()
        .map(|unsupported| UnsupportedTargetDetail {
            label: unsupported.target.display_label.clone(),
            reason: unsupported.reason,
            location: unsupported.target.definition_range,
        })
        .collect::<Vec<_>>();
    targets.sort_by(|left, right| {
        left.label
            .cmp(&right.label)
            .then_with(|| reason_rank(left.reason).cmp(&reason_rank(right.reason)))
            .then_with(|| compare_location(db, left.location, right.location))
    });
    targets.dedup();

    let calls = candidate
        .targets
        .iter()
        .filter(|unsupported| unsupported.reason == CallNoMatchReason::SignatureMismatch)
        .map(|unsupported| {
            SupportedCall::new(unsupported.target.kind, unsupported.target.name.as_str())
        })
        .collect::<Vec<_>>();
    let supported_signatures = present_supported_signatures(current_supported_functions(), &calls)
        .into_iter()
        .map(String::into_boxed_str)
        .collect::<Vec<_>>()
        .into_boxed_slice();

    let observed = &source[candidate.range];
    let observed_call = (!supported_signatures.is_empty()
        && !observed.contains(['\n', '\r'])
        && observed.chars().count() <= MAX_OBSERVED_CALL_CHARACTERS)
        .then(|| observed.into());

    UnsupportedFunctionDetails {
        targets: targets.into_boxed_slice(),
        observed_call,
        supported_signatures,
    }
}

const fn reason_rank(reason: CallNoMatchReason) -> u8 {
    match reason {
        CallNoMatchReason::MissingRegistryEntry => 0,
        CallNoMatchReason::SignatureMismatch => 1,
    }
}

fn compare_location(
    db: &dyn ty_project::Db,
    left: Option<FileRange>,
    right: Option<FileRange>,
) -> Ordering {
    match (left, right) {
        (None, None) => Ordering::Equal,
        (None, Some(_)) => Ordering::Less,
        (Some(_), None) => Ordering::Greater,
        (Some(left), Some(right)) => left
            .file()
            .path(db)
            .as_str()
            .cmp(right.file().path(db).as_str())
            .then_with(|| left.range().start().cmp(&right.range().start()))
            .then_with(|| left.range().end().cmp(&right.range().end())),
    }
}

fn invalid_suppression_reason(reason: InvalidSuppressionReason) -> &'static str {
    match reason {
        InvalidSuppressionReason::ExpectedIgnore => "expected `ignore` after `chalk:`",
        InvalidSuppressionReason::Blanket => "suppression code is required",
        InvalidSuppressionReason::ExpectedCodeList => "expected a bracketed suppression code list",
        InvalidSuppressionReason::MissingClosingBracket => "missing closing `]`",
        InvalidSuppressionReason::EmptyCodeList => "suppression code list cannot be empty",
        InvalidSuppressionReason::MalformedCodeList => "malformed suppression code list",
        InvalidSuppressionReason::TrailingContent => {
            "unexpected content after suppression code list"
        }
    }
}
