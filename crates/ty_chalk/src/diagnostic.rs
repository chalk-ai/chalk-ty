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
    pub file: File,
    pub range: TextRange,
    pub kind: ChalkDiagnosticKind,
}

impl ChalkDiagnostic {
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
    pub targets: Box<[UnsupportedTargetDetail]>,
    observed_call: Option<Box<str>>,
    pub supported_signatures: Box<[Box<str>]>,
}

impl UnsupportedFunctionDetails {
    pub fn observed_call(&self) -> Option<&str> {
        self.observed_call.as_deref()
    }
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct UnsupportedTargetDetail {
    pub label: Box<str>,
    pub reason: CallNoMatchReason,
    pub location: Option<FileRange>,
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

#[cfg(test)]
mod tests {
    use std::hash::{DefaultHasher, Hash, Hasher};

    use ruff_db::Db as _;
    use ruff_db::files::{File, FileRange, system_path_to_file};
    use ruff_db::source::source_text;
    use ruff_db::system::{
        DbWithTestSystem as _, DbWithWritableSystem as _, SystemPath, SystemPathBuf,
    };
    use ruff_python_ast::PythonVersion;
    use ruff_python_ast::name::Name;
    use ruff_text_size::{TextLen as _, TextRange, TextSize};
    use ty_module_resolver::SearchPathSettings;
    use ty_project::{ProjectMetadata, TestDb};
    use ty_python_core::platform::PythonPlatform;
    use ty_python_core::program::{FallibleStrategy, Program, ProgramSettings};
    use ty_python_semantic::chalk::KnownCallTarget;
    use ty_python_semantic::{PythonVersionSource, PythonVersionWithSource};

    use crate::CallNoMatchReason;
    use crate::active_project::ChalkProjectInput;
    use crate::call_matcher::{CallMatchIdentity, CallMatchTarget};
    use crate::reachability::{UnsupportedCallCandidate, UnsupportedTarget};
    use crate::supported_functions::CallKind;

    use super::{
        ChalkDiagnostic, ChalkDiagnosticKind, UnsupportedFunctionDetails,
        chalk_diagnostics_for_file, unsupported_function_details,
    };

    fn setup(main: &str, files: &[(&str, &str)]) -> (TestDb, File) {
        let project = ProjectMetadata::new("test", SystemPathBuf::from("/"));
        let mut db = TestDb::new(project);
        db.init_program().unwrap();

        for (path, source) in files {
            db.write_file(SystemPath::new(path), source).unwrap();
        }
        db.write_file(SystemPath::new("/main.py"), main).unwrap();
        let file = system_path_to_file(&db, "/main.py").unwrap();
        (db, file)
    }

    fn target<'db>(
        label: &str,
        kind: CallKind,
        name: &str,
        reason: CallNoMatchReason,
        location: Option<FileRange>,
    ) -> UnsupportedTarget<'db> {
        UnsupportedTarget {
            target: CallMatchTarget {
                identity: CallMatchIdentity::Known(KnownCallTarget::StrStartswith),
                kind,
                name: Name::new(name),
                receiver_parameter: None,
                display_label: label.into(),
                definition_range: location,
            },
            reason,
        }
    }

    fn details<'db>(
        db: &'db TestDb,
        file: File,
        targets: Vec<UnsupportedTarget<'db>>,
    ) -> UnsupportedFunctionDetails {
        let source = source_text(db, file);
        let candidate = UnsupportedCallCandidate {
            file,
            range: TextRange::new(TextSize::new(0), source_text(db, file).text_len()),
            targets: targets.into_boxed_slice(),
        };
        unsupported_function_details(db, &candidate, &source)
    }

    #[test]
    fn sorts_and_deduplicates_mixed_target_details() {
        let (db, file) = setup("abs(\"x\")", &[("/a.py", "a"), ("/z.py", "z")]);
        let a = system_path_to_file(&db, "/a.py").unwrap();
        let z = system_path_to_file(&db, "/z.py").unwrap();
        let a_location = Some(FileRange::new(a, TextRange::new(0.into(), 1.into())));
        let z_location = Some(FileRange::new(z, TextRange::new(0.into(), 1.into())));
        let details = details(
            &db,
            file,
            vec![
                target(
                    "beta",
                    CallKind::Builtin,
                    "missing",
                    CallNoMatchReason::MissingRegistryEntry,
                    None,
                ),
                target(
                    "alpha",
                    CallKind::Builtin,
                    "abs",
                    CallNoMatchReason::SignatureMismatch,
                    z_location,
                ),
                target(
                    "alpha",
                    CallKind::Builtin,
                    "missing",
                    CallNoMatchReason::MissingRegistryEntry,
                    z_location,
                ),
                target(
                    "alpha",
                    CallKind::Builtin,
                    "missing",
                    CallNoMatchReason::MissingRegistryEntry,
                    a_location,
                ),
                target(
                    "alpha",
                    CallKind::Builtin,
                    "missing",
                    CallNoMatchReason::MissingRegistryEntry,
                    a_location,
                ),
            ],
        );

        assert_eq!(details.targets.len(), 4);
        assert_eq!(
            details
                .targets
                .iter()
                .map(|target| (target.label.as_ref(), target.reason))
                .collect::<Vec<_>>(),
            [
                ("alpha", CallNoMatchReason::MissingRegistryEntry),
                ("alpha", CallNoMatchReason::MissingRegistryEntry),
                ("alpha", CallNoMatchReason::SignatureMismatch),
                ("beta", CallNoMatchReason::MissingRegistryEntry),
            ]
        );
        assert_eq!(details.targets[0].location, a_location);
        assert_eq!(details.targets[1].location, z_location);
    }

    #[test]
    fn missing_only_omits_observed_call_and_supported_signatures() {
        let (db, file) = setup("missing()", &[]);
        let details = details(
            &db,
            file,
            vec![target(
                "missing",
                CallKind::Builtin,
                "missing",
                CallNoMatchReason::MissingRegistryEntry,
                None,
            )],
        );

        assert_eq!(details.observed_call(), None);
        assert!(details.supported_signatures.is_empty());
    }

    #[test]
    fn signature_mismatch_includes_short_observed_call_and_suggestions() {
        let (db, file) = setup("abs(\"x\")", &[]);
        let details = details(
            &db,
            file,
            vec![target(
                "builtins.abs",
                CallKind::Builtin,
                "abs",
                CallNoMatchReason::SignatureMismatch,
                None,
            )],
        );

        assert_eq!(details.observed_call(), Some("abs(\"x\")"));
        assert!(
            details
                .supported_signatures
                .iter()
                .all(|signature| signature.starts_with("abs("))
        );
        assert!(!details.supported_signatures.is_empty());
    }

    #[test]
    fn suggestions_apply_one_global_cap_and_sentinel() {
        let (db, file) = setup("selected()", &[]);
        let details = details(
            &db,
            file,
            vec![
                target(
                    "builtins.round",
                    CallKind::Builtin,
                    "round",
                    CallNoMatchReason::SignatureMismatch,
                    None,
                ),
                target(
                    "builtins.abs",
                    CallKind::Builtin,
                    "abs",
                    CallNoMatchReason::SignatureMismatch,
                    None,
                ),
                target(
                    "builtins.len",
                    CallKind::Builtin,
                    "len",
                    CallNoMatchReason::SignatureMismatch,
                    None,
                ),
            ],
        );

        assert_eq!(details.supported_signatures.len(), 3);
        assert!(
            details
                .supported_signatures
                .last()
                .is_some_and(|signature| signature.starts_with("... ["))
        );
    }

    #[test]
    fn protocol_diagnostics_present_source_call_signatures() {
        let project_metadata = ProjectMetadata::new("test", SystemPathBuf::from("/src"));
        let mut db = TestDb::new(project_metadata);
        for path in ["/src", "/site-packages/chalk"] {
            db.memory_file_system()
                .create_directory_all(SystemPath::new(path))
                .unwrap();
        }
        db.write_file(
            SystemPath::new("/site-packages/chalk/__init__.py"),
            "def online(function): ...\n",
        )
        .unwrap();
        db.write_file(
            SystemPath::new("/src/main.py"),
            "from chalk import online\n\n@online\ndef root(value: object):\n    len(1)\n    bool(value)\n",
        )
        .unwrap();
        let search_paths = SearchPathSettings {
            extra_paths: Vec::new(),
            src_roots: vec![SystemPathBuf::from("/src")],
            custom_typeshed: None,
            site_packages_paths: vec![SystemPathBuf::from("/site-packages")],
            real_stdlib_path: None,
        }
        .to_search_paths(db.system(), db.vendored(), &FallibleStrategy)
        .unwrap();
        Program::from_settings(
            &db,
            ProgramSettings {
                python_version: PythonVersionWithSource {
                    version: PythonVersion::latest_ty(),
                    source: PythonVersionSource::Default,
                },
                python_platform: PythonPlatform::default(),
                search_paths,
            },
        );
        let file = system_path_to_file(&db, "/src/main.py").unwrap();
        let project = ChalkProjectInput::new(
            &db,
            SystemPathBuf::from("/src"),
            SystemPathBuf::from("/src/chalk.yml"),
            Box::new([file]),
        );

        let diagnostics = chalk_diagnostics_for_file(&db, project, file);
        assert_eq!(diagnostics.len(), 2);
        for diagnostic in diagnostics {
            let details = diagnostic.unsupported_function_details().unwrap();
            let observed = details.observed_call().unwrap();
            let source_name = if observed.starts_with("len(") {
                "len"
            } else {
                assert!(observed.starts_with("bool("), "{observed}");
                "bool"
            };
            assert!(!details.supported_signatures.is_empty());
            assert!(
                details
                    .supported_signatures
                    .iter()
                    .all(|signature| signature.starts_with(source_name))
            );
            assert!(
                details
                    .supported_signatures
                    .iter()
                    .all(|signature| !signature.contains("__len__")
                        && !signature.contains("__bool__"))
            );
        }
    }

    #[test]
    fn diagnostic_hash_includes_unsupported_details() {
        fn hash(value: &impl Hash) -> u64 {
            let mut hasher = DefaultHasher::new();
            value.hash(&mut hasher);
            hasher.finish()
        }

        let (db, file) = setup("missing()", &[]);
        let first = ChalkDiagnostic {
            file,
            range: TextRange::new(0.into(), 9.into()),
            kind: ChalkDiagnosticKind::UnsupportedFunction(details(
                &db,
                file,
                vec![target(
                    "first",
                    CallKind::Builtin,
                    "missing",
                    CallNoMatchReason::MissingRegistryEntry,
                    None,
                )],
            )),
        };
        let second = ChalkDiagnostic {
            file,
            range: first.range,
            kind: ChalkDiagnosticKind::UnsupportedFunction(details(
                &db,
                file,
                vec![target(
                    "second",
                    CallKind::Builtin,
                    "missing",
                    CallNoMatchReason::MissingRegistryEntry,
                    None,
                )],
            )),
        };

        assert_ne!(first, second);
        assert_ne!(hash(&first), hash(&second));
    }
}
