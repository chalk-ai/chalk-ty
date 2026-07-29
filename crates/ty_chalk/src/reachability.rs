use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};

use ruff_db::files::File;
use ruff_text_size::TextRange;
use ty_python_semantic::chalk::{CallTargetKind, Definition};

use crate::CallNoMatchReason;
use crate::active_project::ChalkProjectInput;
use crate::call_matcher::{CallMatchIdentity, CallMatchTarget};
use crate::facts::{CallFact, file_facts};
use crate::suppression::SuppressionProblemKind;

type UnsupportedReason = CallNoMatchReason;

/// One unsupported statically possible target of a source call.
#[derive(Clone, Debug, Eq, PartialEq, salsa::Update, get_size2::GetSize)]
pub(crate) struct UnsupportedTarget<'db> {
    pub(crate) target: CallMatchTarget<'db>,
    pub(crate) reason: UnsupportedReason,
}

/// One reachable call with at least one unsupported external target.
#[derive(Debug, Eq, PartialEq, salsa::Update, get_size2::GetSize)]
pub(crate) struct UnsupportedCallCandidate<'db> {
    pub(crate) file: File,
    pub(crate) range: TextRange,
    pub(crate) targets: Box<[UnsupportedTarget<'db>]>,
}

/// A reachable call edge that closes a direct or multi-function cycle.
#[derive(Clone, Copy, Debug, Eq, PartialEq, salsa::Update, get_size2::GetSize)]
pub(crate) struct CycleCandidate {
    pub(crate) file: File,
    pub(crate) range: TextRange,
}

/// An invalid or unknown Chalk suppression, independent of resolver reachability.
#[derive(Clone, Copy, Debug, Eq, PartialEq, salsa::Update, get_size2::GetSize)]
pub(crate) struct ProjectSuppressionProblem {
    pub(crate) file: File,
    pub(crate) range: TextRange,
    pub(crate) kind: SuppressionProblemKind,
}

/// Cached, project-wide candidates grouped by exact source file through lookup methods.
#[derive(Debug, Eq, PartialEq, salsa::Update, get_size2::GetSize)]
pub(crate) struct ProjectCandidates<'db> {
    unsupported: Box<[UnsupportedCallCandidate<'db>]>,
    cycles: Box<[CycleCandidate]>,
    suppression_problems: Box<[ProjectSuppressionProblem]>,
}

impl<'db> ProjectCandidates<'db> {
    #[cfg(test)]
    fn unsupported(&self) -> &[UnsupportedCallCandidate<'db>] {
        &self.unsupported
    }

    #[cfg(test)]
    fn cycles(&self) -> &[CycleCandidate] {
        &self.cycles
    }

    pub(crate) fn unsupported_for_file(
        &self,
        file: File,
    ) -> impl Iterator<Item = &UnsupportedCallCandidate<'db>> {
        self.unsupported
            .iter()
            .filter(move |candidate| candidate.file == file)
    }

    pub(crate) fn cycles_for_file(&self, file: File) -> impl Iterator<Item = &CycleCandidate> {
        self.cycles
            .iter()
            .filter(move |candidate| candidate.file == file)
    }

    pub(crate) fn suppression_problems_for_file(
        &self,
        file: File,
    ) -> impl Iterator<Item = &ProjectSuppressionProblem> {
        self.suppression_problems
            .iter()
            .filter(move |problem| problem.file == file)
    }
}

/// Computes the Chalk accelerator closure once for an exact active source-set input.
#[salsa::tracked(returns(ref), heap_size=ruff_memory_usage::heap_size)]
pub(crate) fn project_candidates<'db>(
    db: &'db dyn ty_project::Db,
    project: ChalkProjectInput,
) -> ProjectCandidates<'db> {
    let mut python_files = project.python_files(db).collect::<Vec<_>>();
    python_files.sort_by(|left, right| left.path(db).as_str().cmp(right.path(db).as_str()));
    let first_party = python_files.iter().copied().collect::<HashSet<_>>();
    let mut calls = HashMap::<Definition<'db>, Vec<CallSite<'db>>>::new();
    let mut cycle_roots = Vec::new();
    let mut unsupported_roots = Vec::new();
    let mut seen_roots = HashSet::new();
    let mut suppression_problems = Vec::new();

    for file in python_files {
        let Some(facts) = file_facts(db, file) else {
            continue;
        };

        for call in &facts.calls {
            calls
                .entry(call.caller)
                .or_default()
                .push(CallSite { file, call });
        }
        for root in &facts.resolver_roots {
            if seen_roots.insert(root.definition) {
                cycle_roots.push(root.definition);
                if root.unsupported_function_suppression.is_none() {
                    unsupported_roots.push(root.definition);
                }
            }
        }
        suppression_problems.extend(facts.suppression_problems.iter().map(|problem| {
            ProjectSuppressionProblem {
                file,
                range: problem.range,
                kind: problem.kind,
            }
        }));
    }

    let mut analyzer = ReachabilityAnalyzer {
        db,
        first_party,
        calls,
        states: HashMap::new(),
        unsupported: Vec::new(),
        cycles: Vec::new(),
        cycle_sites: HashSet::new(),
    };
    for root in unsupported_roots {
        analyzer.visit(root, Analysis::Unsupported);
    }
    analyzer.states.clear();
    for root in cycle_roots {
        analyzer.visit(root, Analysis::Cycles);
    }
    let cycle_sites = &analyzer.cycle_sites;
    analyzer
        .unsupported
        .retain(|candidate| !cycle_sites.contains(&(candidate.file, candidate.range)));

    analyzer.unsupported.sort_by(|left, right| {
        compare_file_range(db, left.file, left.range, right.file, right.range)
    });
    analyzer.cycles.sort_by(|left, right| {
        compare_file_range(db, left.file, left.range, right.file, right.range)
    });
    suppression_problems.sort_by(|left, right| {
        compare_file_range(db, left.file, left.range, right.file, right.range)
    });

    ProjectCandidates {
        unsupported: analyzer.unsupported.into_boxed_slice(),
        cycles: analyzer.cycles.into_boxed_slice(),
        suppression_problems: suppression_problems.into_boxed_slice(),
    }
}

#[derive(Clone, Copy)]
struct CallSite<'db> {
    file: File,
    call: &'db CallFact<'db>,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum VisitState {
    Visiting,
    Done,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum Analysis {
    Unsupported,
    Cycles,
}

struct ReachabilityAnalyzer<'db> {
    db: &'db dyn ty_project::Db,
    first_party: HashSet<File>,
    calls: HashMap<Definition<'db>, Vec<CallSite<'db>>>,
    states: HashMap<Definition<'db>, VisitState>,
    unsupported: Vec<UnsupportedCallCandidate<'db>>,
    cycles: Vec<CycleCandidate>,
    cycle_sites: HashSet<(File, TextRange)>,
}

impl<'db> ReachabilityAnalyzer<'db> {
    fn visit(&mut self, definition: Definition<'db>, analysis: Analysis) {
        if self.states.contains_key(&definition) {
            return;
        }
        self.states.insert(definition, VisitState::Visiting);

        let calls = self.calls.get(&definition).cloned().unwrap_or_default();
        for site in calls {
            self.visit_call(site, analysis);
        }

        self.states.insert(definition, VisitState::Done);
    }

    fn visit_call(&mut self, site: CallSite<'db>, analysis: Analysis) {
        let mut first_party_targets = Vec::new();

        for target in &site.call.targets {
            if target.kind == CallTargetKind::Function
                && self.first_party.contains(&target.definition.file(self.db))
            {
                if !first_party_targets.contains(&target.definition) {
                    first_party_targets.push(target.definition);
                }
            }
        }

        if analysis == Analysis::Unsupported {
            let mut unsupported_targets = Vec::new();
            for (target, reason) in &site.call.no_matches {
                if matches!(
                    &target.identity,
                    CallMatchIdentity::Definition(definition)
                        if first_party_targets.contains(definition)
                ) {
                    continue;
                }
                let unsupported = UnsupportedTarget {
                    target: target.clone(),
                    reason: *reason,
                };
                if !unsupported_targets.contains(&unsupported) {
                    unsupported_targets.push(unsupported);
                }
            }

            if !unsupported_targets.is_empty()
                && site
                    .call
                    .unsupported_function_statement_suppression
                    .is_none()
                && site.call.unsupported_function_caller_suppression.is_none()
            {
                self.unsupported.push(UnsupportedCallCandidate {
                    file: site.file,
                    range: site.call.range,
                    targets: unsupported_targets.into_boxed_slice(),
                });
            }
        }

        for target in first_party_targets {
            match self.states.get(&target) {
                Some(VisitState::Visiting) => {
                    if analysis == Analysis::Cycles
                        && self.cycle_sites.insert((site.file, site.call.range))
                    {
                        self.cycles.push(CycleCandidate {
                            file: site.file,
                            range: site.call.range,
                        });
                    }
                }
                Some(VisitState::Done) => {}
                None => self.visit(target, analysis),
            }
        }
    }
}

fn compare_file_range(
    db: &dyn ty_project::Db,
    left_file: File,
    left_range: TextRange,
    right_file: File,
    right_range: TextRange,
) -> Ordering {
    left_file
        .path(db)
        .as_str()
        .cmp(right_file.path(db).as_str())
        .then_with(|| left_range.start().cmp(&right_range.start()))
        .then_with(|| left_range.end().cmp(&right_range.end()))
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use ruff_db::Db as _;
    use ruff_db::files::{File, system_path_to_file};
    use ruff_db::parsed::parsed_module;
    use ruff_db::source::source_text;
    use ruff_db::system::{
        DbWithTestSystem as _, DbWithWritableSystem as _, SystemPath, SystemPathBuf,
    };
    use ruff_db::testing::{
        assert_function_query_was_not_run, assert_function_query_was_run,
        find_will_execute_event_by_name,
    };
    use ruff_python_ast::PythonVersion;
    use ruff_text_size::{Ranged, TextRange};
    use salsa::Setter;
    use salsa::plumbing::AsId as _;
    use ty_module_resolver::SearchPathSettings;
    use ty_project::{ProjectMetadata, TestDb};
    use ty_python_core::platform::PythonPlatform;
    use ty_python_core::program::{FallibleStrategy, Program, ProgramSettings};
    use ty_python_semantic::chalk::{
        Definition, KnownCallTarget, chalk_call_definition_origin, chalk_function_definition_origin,
    };
    use ty_python_semantic::{PythonVersionSource, PythonVersionWithSource, SemanticModel};

    use crate::active_project::ChalkProjectInput;
    use crate::call_matcher::CallMatchIdentity;
    use crate::diagnostic::{ChalkDiagnosticKind, chalk_diagnostics_for_file};
    use crate::facts::file_facts;

    use super::{UnsupportedReason, project_candidates};

    #[salsa::tracked]
    fn call_origin_label<'db>(
        db: &'db dyn ty_project::Db,
        definition: Definition<'db>,
    ) -> Box<str> {
        chalk_call_definition_origin(db, definition)
            .map_or("", |origin| origin.qualified_symbol.as_ref())
            .into()
    }

    #[salsa::tracked]
    fn function_origin_label<'db>(
        db: &'db dyn ty_project::Db,
        definition: Definition<'db>,
    ) -> Box<str> {
        chalk_function_definition_origin(db, definition)
            .map_or("", |origin| origin.symbol.as_str())
            .into()
    }

    fn setup(files: &[(&str, &str)]) -> (TestDb, HashMap<String, File>) {
        let project = ProjectMetadata::new("test", SystemPathBuf::from("/"));
        let mut db = TestDb::new(project);

        for (path, source) in files.iter().copied().chain([(
            "/site-packages/chalk/__init__.py",
            "def online(function): ...\n",
        )]) {
            if let Some(parent) = SystemPath::new(path).parent() {
                db.memory_file_system()
                    .create_directory_all(parent)
                    .unwrap();
            }
            db.write_file(SystemPath::new(path), source).unwrap();
        }
        let search_paths = SearchPathSettings {
            extra_paths: Vec::new(),
            src_roots: vec![SystemPathBuf::from("/")],
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
        let files = files
            .iter()
            .map(|(path, _)| {
                (
                    (*path).to_string(),
                    system_path_to_file(&db, *path).unwrap(),
                )
            })
            .collect();
        (db, files)
    }

    fn input(db: &TestDb, files: &HashMap<String, File>, paths: &[&str]) -> ChalkProjectInput {
        ChalkProjectInput::new(
            db,
            SystemPathBuf::from("/"),
            SystemPathBuf::from("/chalk.yml"),
            paths
                .iter()
                .map(|path| files[*path])
                .collect::<Vec<_>>()
                .into_boxed_slice(),
        )
    }

    fn text(db: &TestDb, file: File, range: TextRange) -> String {
        source_text(db, file)[range].to_string()
    }

    #[test]
    fn direct_resolver_emits_missing_registry_candidate_despite_type_errors() {
        let (db, files) = setup(&[
            (
                "/main.py",
                "\
from chalk import online
from external import unsupported

@online
def root():
    1 + \"type error\"
    unsupported()
",
            ),
            ("/external.py", "def unsupported(): pass\n"),
        ]);
        let main = files["/main.py"];
        let project = input(&db, &files, &["/main.py"]);
        let candidates = project_candidates(&db, project);

        assert_eq!(candidates.unsupported().len(), 1);
        assert_eq!(
            text(&db, main, candidates.unsupported()[0].range),
            "unsupported()"
        );
        assert_eq!(candidates.unsupported()[0].targets.len(), 1);
        assert_eq!(
            candidates.unsupported()[0].targets[0].reason,
            UnsupportedReason::MissingRegistryEntry
        );
        assert_eq!(candidates.unsupported_for_file(main).count(), 1);
    }

    #[test]
    fn distinguishes_missing_registry_entry_from_signature_mismatch() {
        let (db, files) = setup(&[
            (
                "/main.py",
                "\
from chalk import online
from external import missing

@online
def root():
    missing()
    abs(\"not numeric\")
",
            ),
            ("/external.py", "def missing(): pass\n"),
        ]);
        let main = files["/main.py"];
        let project = input(&db, &files, &["/main.py"]);
        let candidates = project_candidates(&db, project);

        assert_eq!(candidates.unsupported().len(), 2);
        assert_eq!(
            text(&db, main, candidates.unsupported()[0].range),
            "missing()"
        );
        assert_eq!(
            candidates.unsupported()[0].targets[0].reason,
            UnsupportedReason::MissingRegistryEntry
        );
        assert_eq!(
            text(&db, main, candidates.unsupported()[1].range),
            "abs(\"not numeric\")"
        );
        assert_eq!(
            candidates.unsupported()[1].targets[0].reason,
            UnsupportedReason::SignatureMismatch
        );
    }

    #[test]
    fn mixed_supported_and_unsupported_targets_report_only_the_unsupported_target() {
        let (db, files) = setup(&[
            (
                "/main.py",
                "\
from chalk import online
from external import missing
from math import sqrt

@online
def root(condition):
    selected = sqrt if condition else missing
    selected(4.0)
",
            ),
            ("/external.py", "def missing(value: float) -> float: ...\n"),
        ]);
        let main = files["/main.py"];
        let project = input(&db, &files, &["/main.py"]);
        let candidates = project_candidates(&db, project);

        assert_eq!(candidates.unsupported().len(), 1);
        let candidate = &candidates.unsupported()[0];
        assert_eq!(text(&db, main, candidate.range), "selected(4.0)");
        assert_eq!(candidate.targets.len(), 1);
        assert_eq!(candidate.targets[0].target.name.as_str(), "missing");
        assert_eq!(
            candidate.targets[0].reason,
            UnsupportedReason::MissingRegistryEntry
        );
    }

    #[test]
    fn known_mismatch_is_reported_alongside_an_unresolved_receiver_alternative() {
        let (db, files) = setup(&[(
            "/main.py",
            "\
from chalk import online
from typing import Literal

class Other:
    pass

@online
def root(value: Literal[\"known\"] | Other, ordinary: str):
    ordinary.startswith(\"supported\")
    value.startswith(1)
",
        )]);
        let main = files["/main.py"];
        let project = input(&db, &files, &["/main.py"]);
        let candidates = project_candidates(&db, project);

        assert_eq!(candidates.unsupported().len(), 1);
        let candidate = &candidates.unsupported()[0];
        assert_eq!(text(&db, main, candidate.range), "value.startswith(1)");
        assert_eq!(candidate.targets.len(), 1);
        assert!(
            matches!(
                &candidate.targets[0].target.identity,
                CallMatchIdentity::Known(KnownCallTarget::StrStartswith)
            ),
            "{:#?}",
            candidate.targets
        );
        assert_eq!(
            candidate.targets[0].reason,
            UnsupportedReason::SignatureMismatch
        );
    }

    #[test]
    fn unresolved_alternative_preserves_deduplicated_unsupported_target_order() {
        let (db, files) = setup(&[
            (
                "/main.py",
                "\
from chalk import online
from external import First, Other, Second

@online
def root(value: First | Second | Other):
    value.run()
",
            ),
            (
                "/external.py",
                "\
class First:
    def run(self): pass

class Second:
    def run(self): pass

class Other: pass
",
            ),
        ]);
        let main = files["/main.py"];
        let external = files["/external.py"];
        let project = input(&db, &files, &["/main.py"]);
        let candidates = project_candidates(&db, project);

        assert_eq!(candidates.unsupported().len(), 1);
        let candidate = &candidates.unsupported()[0];
        assert_eq!(text(&db, main, candidate.range), "value.run()");
        assert_eq!(candidate.targets.len(), 2);
        assert!(candidate.targets.iter().all(|target| {
            target.reason == UnsupportedReason::MissingRegistryEntry
                && target.target.name.as_str() == "run"
        }));

        let actual = candidate
            .targets
            .iter()
            .map(|target| match target.target.identity {
                CallMatchIdentity::Definition(definition) => definition,
                ref identity => panic!("expected definition identity, got {identity:?}"),
            })
            .collect::<Vec<_>>();
        assert_eq!(actual.len(), 2);
        assert_ne!(actual[0], actual[1]);
        assert!(
            actual
                .iter()
                .all(|definition| definition.file(&db) == external)
        );
        assert!(
            candidate.targets[0]
                .target
                .definition_range
                .unwrap()
                .range()
                .start()
                < candidate.targets[1]
                    .target
                    .definition_range
                    .unwrap()
                    .range()
                    .start()
        );
    }

    #[test]
    fn follows_same_and_cross_file_ambiguous_targets_once() {
        let (db, files) = setup(&[
            (
                "/main.py",
                "\
from chalk import online
from helper import cross

def same():
    cross()

@online
def root(condition):
    selected = same if condition else cross
    selected()
",
            ),
            (
                "/helper.py",
                "\
from external import unsupported

def cross():
    unsupported()
",
            ),
            ("/external.py", "def unsupported(): pass\n"),
        ]);
        let helper = files["/helper.py"];
        let project = input(&db, &files, &["/main.py", "/helper.py"]);
        let candidates = project_candidates(&db, project);

        assert_eq!(candidates.unsupported().len(), 1);
        assert_eq!(candidates.unsupported()[0].targets.len(), 1);
        assert_eq!(
            text(&db, helper, candidates.unsupported()[0].range),
            "unsupported()"
        );
    }

    #[test]
    fn follows_direct_aliased_module_bound_and_nested_function_edges() {
        let (db, files) = setup(&[
            (
                "/main.py",
                "\
from chalk import online
from helper import aliased as imported
import helper
from external import unsupported

class Local:
    def bound(self):
        unsupported()

def direct():
    unsupported()

@online
def root(local: Local):
    direct()
    imported()
    helper.module_qualified()
    local.bound()

    def nested():
        unsupported()

    nested()
",
            ),
            (
                "/helper.py",
                "\
from external import unsupported

def aliased():
    unsupported()

def module_qualified():
    unsupported()
",
            ),
            ("/external.py", "def unsupported(): pass\n"),
        ]);
        let main = files["/main.py"];
        let helper = files["/helper.py"];
        let project = input(&db, &files, &["/main.py", "/helper.py"]);
        let candidates = project_candidates(&db, project);

        assert_eq!(candidates.unsupported().len(), 5);
        assert_eq!(candidates.unsupported_for_file(main).count(), 3);
        assert_eq!(candidates.unsupported_for_file(helper).count(), 2);
        assert!(
            candidates
                .unsupported()
                .iter()
                .all(|candidate| { text(&db, candidate.file, candidate.range) == "unsupported()" })
        );
    }

    #[test]
    fn follows_async_and_generator_resolver_helper_edges() {
        let (db, files) = setup(&[
            (
                "/main.py",
                "\
from chalk import online
from helper import async_helper, generator_helper

@online
async def async_root():
    await async_helper()

@online
def generator_root():
    yield from generator_helper()
",
            ),
            (
                "/helper.py",
                "\
from external import async_unsupported, generator_unsupported

async def async_helper():
    await async_unsupported()

def generator_helper():
    yield generator_unsupported()
",
            ),
            (
                "/external.py",
                "\
async def async_unsupported(): pass
def generator_unsupported(): pass
",
            ),
        ]);
        let helper = files["/helper.py"];
        let project = input(&db, &files, &["/main.py", "/helper.py"]);
        let candidates = project_candidates(&db, project);

        assert_eq!(candidates.unsupported().len(), 2);
        assert!(
            candidates
                .unsupported()
                .iter()
                .all(|candidate| candidate.file == helper)
        );
        assert_eq!(
            candidates
                .unsupported()
                .iter()
                .map(|candidate| text(&db, candidate.file, candidate.range))
                .collect::<Vec<_>>(),
            ["async_unsupported()", "generator_unsupported()"]
        );
        assert!(candidates.unsupported().iter().all(|candidate| {
            candidate.targets.len() == 1
                && candidate.targets[0].reason == UnsupportedReason::MissingRegistryEntry
        }));
    }

    #[test]
    fn root_and_lexical_suppressions() {
        let (db, files) = setup(&[
            (
                "/main.py",
                "\
from chalk import online
from external import unsupported

# chalk: ignore[unsupported-function]
@online
def suppressed_root():
    unsupported()
    downstream()

@online
def active():
    suppressed_root()
    statement_helper()

def downstream():
    unsupported()

def statement_helper():
    unsupported()  # chalk: ignore[unsupported-function]
",
            ),
            ("/external.py", "def unsupported(): pass\n"),
        ]);
        let main = files["/main.py"];
        let project = input(&db, &files, &["/main.py"]);
        let candidates = project_candidates(&db, project);

        assert_eq!(candidates.unsupported().len(), 1);
        assert_eq!(
            text(&db, main, candidates.unsupported()[0].range),
            "unsupported()"
        );
    }

    #[test]
    fn unsupported_function_suppression_does_not_suppress_resolver_cycles() {
        let (db, files) = setup(&[(
            "/main.py",
            "\
from chalk import online

# chalk: ignore[unsupported-function]
@online
def root():
    root()
",
        )]);
        let main = files["/main.py"];
        let project = input(&db, &files, &["/main.py"]);
        let candidates = project_candidates(&db, project);

        assert!(candidates.unsupported().is_empty());
        assert_eq!(candidates.cycles().len(), 1);
        assert_eq!(text(&db, main, candidates.cycles()[0].range), "root()");
    }

    #[test]
    fn resolver_cycle_takes_precedence_over_unsupported_target_at_same_call() {
        let (db, files) = setup(&[
            (
                "/main.py",
                "\
from chalk import online
from external import unsupported

@online
def root():
    helper()

def helper():
    nested(True)

def nested(condition):
    selected = helper if condition else unsupported
    selected()
",
            ),
            ("/external.py", "def unsupported(): pass\n"),
        ]);
        let main = files["/main.py"];
        let project = input(&db, &files, &["/main.py"]);
        let candidates = project_candidates(&db, project);

        assert!(candidates.unsupported().is_empty(), "{candidates:#?}");
        assert_eq!(candidates.cycles().len(), 1);
        assert_eq!(text(&db, main, candidates.cycles()[0].range), "selected()");

        let diagnostics = chalk_diagnostics_for_file(&db, project, main);
        assert_eq!(diagnostics.len(), 1);
        assert!(matches!(
            &diagnostics[0].kind,
            ChalkDiagnosticKind::ResolverCycle
        ));
        assert_eq!(text(&db, main, diagnostics[0].range), "selected()");
    }

    #[test]
    fn reports_direct_and_cross_file_cycles_at_back_edges() {
        let (db, files) = setup(&[
            (
                "/main.py",
                "\
from chalk import online
import helper

@online
def direct():
    direct()

@online
def cross():
    helper.cross_back()
",
            ),
            (
                "/helper.py",
                "\
import main

def cross_back():
    main.cross()
",
            ),
        ]);
        let main = files["/main.py"];
        let helper = files["/helper.py"];
        let project = input(&db, &files, &["/main.py", "/helper.py"]);
        let candidates = project_candidates(&db, project);

        assert_eq!(candidates.cycles().len(), 2);
        let main_cycle = candidates.cycles_for_file(main).next().unwrap();
        assert_eq!(text(&db, main, main_cycle.range), "direct()");
        let helper_cycle = candidates.cycles_for_file(helper).next().unwrap();
        assert_eq!(text(&db, helper, helper_cycle.range), "main.cross()");
    }

    #[test]
    fn converging_roots_and_dag_paths_do_not_become_false_cycles() {
        let (db, files) = setup(&[(
            "/main.py",
            "\
from chalk import online

def shared():
    pass

@online
def second_root():
    shared()

def alternate():
    shared()

@online
def first_root(condition):
    second_root()
    selected = alternate if condition else shared
    selected()
",
        )]);
        let project = input(&db, &files, &["/main.py"]);
        let candidates = project_candidates(&db, project);

        assert!(candidates.cycles().is_empty());
    }

    #[test]
    fn argument_unpacking_defers_matching_but_preserves_first_party_reachability() {
        let (db, files) = setup(&[
            (
                "/main.py",
                "\
from chalk import online
from helper import helper

@online
def root():
    helper(*())
",
            ),
            (
                "/helper.py",
                "\
from external import unsupported

def helper(*args):
    helper(*args)
    unsupported()
",
            ),
            ("/external.py", "def unsupported(): pass\n"),
        ]);
        let project = input(&db, &files, &["/main.py", "/helper.py"]);
        let candidates = project_candidates(&db, project);

        assert_eq!(candidates.unsupported().len(), 1);
        assert_eq!(candidates.cycles().len(), 1);
        let helper = files["/helper.py"];
        assert_eq!(
            text(&db, helper, candidates.unsupported()[0].range),
            "unsupported()"
        );
        assert_eq!(
            text(&db, helper, candidates.cycles()[0].range),
            "helper(*args)"
        );
    }

    #[test]
    fn helper_source_edit_invalidates_its_file_facts_and_cached_project_closure() {
        let (mut db, files) = setup(&[
            (
                "/main.py",
                "\
from chalk import online
from helper import helper

@online
def root():
    helper()
",
            ),
            ("/helper.py", "def helper(): pass\n"),
            ("/external.py", "def unsupported(): pass\n"),
        ]);
        let helper = files["/helper.py"];
        let project = input(&db, &files, &["/main.py", "/helper.py"]);

        assert!(project_candidates(&db, project).unsupported().is_empty());
        db.take_salsa_events();
        db.write_file(
            SystemPath::new("/helper.py"),
            "from external import unsupported\ndef helper(): unsupported()\n",
        )
        .unwrap();

        assert_eq!(project_candidates(&db, project).unsupported().len(), 1);
        let events = db.take_salsa_events();
        assert_function_query_was_run(&db, project_candidates, project, &events);
        assert_function_query_was_run(&db, file_facts, helper, &events);
    }

    #[test]
    fn argument_type_edit_updates_call_match_outcome() {
        let (mut db, files) = setup(&[(
            "/main.py",
            "\
from chalk import online

@online
def root():
    abs(1)
",
        )]);
        let main = files["/main.py"];
        let project = input(&db, &files, &["/main.py"]);

        assert!(project_candidates(&db, project).unsupported().is_empty());
        db.take_salsa_events();
        db.write_file(
            SystemPath::new("/main.py"),
            "\
from chalk import online

@online
def root():
    abs(\"value\")
",
        )
        .unwrap();

        let candidates = project_candidates(&db, project);
        assert_eq!(candidates.unsupported().len(), 1);
        assert_eq!(
            text(&db, main, candidates.unsupported()[0].range),
            "abs(\"value\")"
        );
        assert_eq!(
            candidates.unsupported()[0].targets[0].reason,
            UnsupportedReason::SignatureMismatch
        );
        let events = db.take_salsa_events();
        assert_function_query_was_run(&db, file_facts, main, &events);
        assert_function_query_was_run(&db, project_candidates, project, &events);
    }

    #[test]
    fn receiver_type_edit_updates_call_match_outcome() {
        let (mut db, files) = setup(&[(
            "/main.py",
            "\
from chalk import online

@online
def root():
    receiver: str = \"value\"
    receiver.startswith(1)
",
        )]);
        let project = input(&db, &files, &["/main.py"]);

        assert_eq!(project_candidates(&db, project).unsupported().len(), 1);
        db.write_file(
            SystemPath::new("/main.py"),
            "\
from chalk import online

@online
def root():
    receiver: int = 1
    receiver.startswith(1)
",
        )
        .unwrap();

        assert!(project_candidates(&db, project).unsupported().is_empty());
    }

    #[test]
    fn import_alias_retarget_updates_first_party_edge() {
        let (mut db, files) = setup(&[
            (
                "/main.py",
                "\
from chalk import online
from helper import first as selected

@online
def root():
    selected()
",
            ),
            (
                "/helper.py",
                "\
from external import unsupported

def first():
    pass

def second():
    unsupported()
",
            ),
            ("/external.py", "def unsupported(): pass\n"),
        ]);
        let main = files["/main.py"];
        let helper = files["/helper.py"];
        let project = input(&db, &files, &["/main.py", "/helper.py"]);

        assert_eq!(
            file_facts(&db, main).unwrap().calls[0].targets[0]
                .definition
                .name(&db)
                .as_deref(),
            Some("first")
        );
        assert!(project_candidates(&db, project).unsupported().is_empty());
        db.write_file(
            SystemPath::new("/main.py"),
            "\
from chalk import online
from helper import second as selected

@online
def root():
    selected()
",
        )
        .unwrap();

        assert_eq!(
            file_facts(&db, main).unwrap().calls[0].targets[0]
                .definition
                .name(&db)
                .as_deref(),
            Some("second")
        );
        let candidates = project_candidates(&db, project);
        assert_eq!(candidates.unsupported().len(), 1);
        assert_eq!(candidates.unsupported()[0].file, helper);
        assert_eq!(
            text(&db, helper, candidates.unsupported()[0].range),
            "unsupported()"
        );
    }

    #[test]
    fn resolver_decorator_edit_adds_and_removes_root() {
        let (mut db, files) = setup(&[
            (
                "/main.py",
                "\
from chalk import online
from external import unsupported

def root():
    unsupported()
",
            ),
            ("/external.py", "def unsupported(): pass\n"),
        ]);
        let main = files["/main.py"];
        let project = input(&db, &files, &["/main.py"]);

        assert!(file_facts(&db, main).unwrap().resolver_roots.is_empty());
        assert!(project_candidates(&db, project).unsupported().is_empty());
        db.write_file(
            SystemPath::new("/main.py"),
            "\
from chalk import online
from external import unsupported

@online
def root():
    unsupported()
",
        )
        .unwrap();

        assert_eq!(file_facts(&db, main).unwrap().resolver_roots.len(), 1);
        assert_eq!(project_candidates(&db, project).unsupported().len(), 1);
        db.write_file(
            SystemPath::new("/main.py"),
            "\
from chalk import online
from external import unsupported

def root():
    unsupported()
",
        )
        .unwrap();

        assert!(file_facts(&db, main).unwrap().resolver_roots.is_empty());
        assert!(project_candidates(&db, project).unsupported().is_empty());
    }

    #[test]
    fn project_source_set_mutation_updates_cached_candidates() {
        let (mut db, files) = setup(&[
            (
                "/main.py",
                "\
from chalk import online
from helper import helper

@online
def root():
    helper()
",
            ),
            (
                "/helper.py",
                "\
from external import unsupported

def helper():
    unsupported()
",
            ),
            ("/external.py", "def unsupported(): pass\n"),
        ]);
        let main = files["/main.py"];
        let helper = files["/helper.py"];
        let project = input(&db, &files, &["/main.py"]);

        let candidates = project_candidates(&db, project);
        assert_eq!(candidates.unsupported().len(), 1);
        assert_eq!(candidates.unsupported()[0].file, main);
        assert_eq!(
            text(&db, main, candidates.unsupported()[0].range),
            "helper()"
        );
        db.take_salsa_events();
        project
            .set_source_files(&mut db)
            .to(vec![main, helper].into_boxed_slice());

        let candidates = project_candidates(&db, project);
        assert_eq!(candidates.unsupported().len(), 1);
        assert_eq!(candidates.unsupported()[0].file, helper);
        assert_eq!(
            text(&db, helper, candidates.unsupported()[0].range),
            "unsupported()"
        );
        let events = db.take_salsa_events();
        assert_function_query_was_run(&db, project_candidates, project, &events);
    }

    #[test]
    fn non_factual_helper_edit_backdates_cached_project_closure() {
        let (mut db, files) = setup(&[
            (
                "/main.py",
                "\
from chalk import online
from helper import helper

@online
def root():
    helper()
",
            ),
            (
                "/helper.py",
                "\
def helper():
    value = 1
",
            ),
        ]);
        let helper = files["/helper.py"];
        let project = input(&db, &files, &["/main.py", "/helper.py"]);

        assert!(project_candidates(&db, project).unsupported().is_empty());
        db.take_salsa_events();
        db.write_file(
            SystemPath::new("/helper.py"),
            "\
def helper():
    value = 2
",
        )
        .unwrap();

        assert!(project_candidates(&db, project).unsupported().is_empty());
        let events = db.take_salsa_events();
        assert_function_query_was_run(&db, file_facts, helper, &events);
        assert_function_query_was_not_run(&db, project_candidates, project, &events);
    }

    #[test]
    fn unchanged_call_origin_metadata_backdates_a_direct_dependent() {
        let (mut db, files) = setup(&[
            (
                "/main.py",
                "\
from chalk import online
from external import unsupported

@online
def root():
    unsupported()
",
            ),
            (
                "/external.py",
                "\
def unsupported():
    # first
    pass
",
            ),
        ]);
        let main = files["/main.py"];

        let call_definition = file_facts(&db, main).unwrap().calls[0].targets[0].definition;
        let call_definition_id = call_definition.as_id();
        assert_eq!(
            call_origin_label(&db, call_definition).as_ref(),
            "unsupported"
        );
        db.take_salsa_events();
        db.write_file(
            SystemPath::new("/external.py"),
            "\
def unsupported():
    # other
    pass
",
        )
        .unwrap();

        let call_definition = file_facts(&db, main).unwrap().calls[0].targets[0].definition;
        assert_eq!(
            call_origin_label(&db, call_definition).as_ref(),
            "unsupported"
        );
        let events = db.take_salsa_events();
        assert!(
            find_will_execute_event_by_name(
                &db,
                "chalk_call_definition_origin",
                Some(call_definition_id),
                &events,
            )
            .is_some(),
            "{events:#?}"
        );
        assert!(
            find_will_execute_event_by_name(
                &db,
                "call_origin_label",
                Some(call_definition_id),
                &events,
            )
            .is_none(),
            "{events:#?}"
        );
    }

    #[test]
    fn unchanged_function_origin_metadata_backdates_a_direct_dependent() {
        let (mut db, files) = setup(&[
            (
                "/main.py",
                "\
from external import resolver

@resolver
def root():
    pass
",
            ),
            (
                "/external.py",
                "\
def resolver(function):
    # first
    return function
",
            ),
        ]);
        let main = files["/main.py"];

        let definition = {
            let parsed = parsed_module(&db, main).load(&db);
            let function = parsed
                .syntax()
                .body
                .iter()
                .find_map(ruff_python_ast::Stmt::as_function_def_stmt)
                .unwrap();
            SemanticModel::new(&db, main)
                .chalk_decorator_provenance(&function.decorator_list[0])
                .definitions[0]
        };
        let definition_id = definition.as_id();
        assert_eq!(function_origin_label(&db, definition).as_ref(), "resolver");
        db.take_salsa_events();
        db.write_file(
            SystemPath::new("/external.py"),
            "\
def resolver(function):
    # other
    return function
",
        )
        .unwrap();

        let definition = {
            let parsed = parsed_module(&db, main).load(&db);
            let function = parsed
                .syntax()
                .body
                .iter()
                .find_map(ruff_python_ast::Stmt::as_function_def_stmt)
                .unwrap();
            SemanticModel::new(&db, main)
                .chalk_decorator_provenance(&function.decorator_list[0])
                .definitions[0]
        };
        assert_eq!(definition.as_id(), definition_id);
        assert_eq!(function_origin_label(&db, definition).as_ref(), "resolver");
        let events = db.take_salsa_events();
        assert!(
            find_will_execute_event_by_name(
                &db,
                "chalk_function_definition_origin",
                Some(definition_id),
                &events,
            )
            .is_some(),
            "{events:#?}"
        );
        assert!(
            find_will_execute_event_by_name(
                &db,
                "function_origin_label",
                Some(definition_id),
                &events,
            )
            .is_none(),
            "{events:#?}"
        );
    }

    #[test]
    fn cached_project_facts_do_not_retain_closed_file_ast() {
        let (mut db, files) = setup(&[
            (
                "/main.py",
                "\
from chalk import online
from helper import helper

@online
def root():
    helper()
",
            ),
            (
                "/helper.py",
                "\
from external import unsupported

def helper():
    unsupported()
",
            ),
            ("/external.py", "def unsupported(): pass\n"),
        ]);
        let helper = files["/helper.py"];
        let project = input(&db, &files, &["/main.py", "/helper.py"]);

        let candidates = project_candidates(&db, project);
        assert_eq!(candidates.unsupported().len(), 1);
        assert_eq!(
            candidates.unsupported()[0].targets[0].reason,
            UnsupportedReason::MissingRegistryEntry
        );
        let parsed = parsed_module(&db, helper);
        assert!(ruff_memory_usage::heap_size(parsed) > 0);

        parsed.clear();
        assert_eq!(ruff_memory_usage::heap_size(parsed), 0);

        db.take_salsa_events();
        let cached = project_candidates(&db, project);
        assert_eq!(cached.unsupported().len(), 1);
        assert_eq!(
            cached.unsupported()[0].targets[0].reason,
            UnsupportedReason::MissingRegistryEntry
        );
        let events = db.take_salsa_events();
        assert_function_query_was_not_run(&db, project_candidates, project, &events);
        assert_function_query_was_not_run(&db, file_facts, helper, &events);
    }

    #[test]
    fn retains_unreachable_suppression_problems_and_ignores_chalk_sql() {
        let (db, files) = setup(&[
            ("/main.py", "value = 1  # chalk: ignore[future-code]\n"),
            (
                "/features.chalk.sql",
                "@online\ndef fake():\n    unsupported()\n",
            ),
        ]);
        let main = files["/main.py"];
        let project = input(&db, &files, &["/main.py", "/features.chalk.sql"]);
        let candidates = project_candidates(&db, project);

        assert!(candidates.unsupported().is_empty());
        assert!(candidates.cycles().is_empty());
        assert_eq!(candidates.suppression_problems.len(), 1);
        assert_eq!(
            text(&db, main, candidates.suppression_problems[0].range),
            "future-code"
        );
        assert_eq!(candidates.suppression_problems_for_file(main).count(), 1);
    }

    #[test]
    fn separate_source_set_inputs_do_not_share_first_party_edges() {
        let (db, files) = setup(&[
            (
                "/root1.py",
                "\
from chalk import online
import root2

@online
def root():
    root2.helper()
",
            ),
            (
                "/root2.py",
                "\
from external import unsupported

def helper():
    unsupported()
",
            ),
            ("/external.py", "def unsupported(): pass\n"),
        ]);
        let first = input(&db, &files, &["/root1.py"]);
        let second = input(&db, &files, &["/root2.py"]);
        let combined = input(&db, &files, &["/root1.py", "/root2.py"]);

        let first_candidates = project_candidates(&db, first);
        assert_eq!(first_candidates.unsupported().len(), 1);
        assert_eq!(
            text(
                &db,
                files["/root1.py"],
                first_candidates.unsupported()[0].range
            ),
            "root2.helper()"
        );
        assert!(project_candidates(&db, second).unsupported().is_empty());
        let combined_candidates = project_candidates(&db, combined);
        assert_eq!(combined_candidates.unsupported().len(), 1);
        assert_eq!(
            combined_candidates.unsupported()[0].file,
            files["/root2.py"]
        );
        assert_eq!(
            text(
                &db,
                files["/root2.py"],
                combined_candidates.unsupported()[0].range
            ),
            "unsupported()"
        );
    }
}
