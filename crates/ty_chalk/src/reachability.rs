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

    #[cfg(test)]
    fn suppression_problems(&self) -> &[ProjectSuppressionProblem] {
        &self.suppression_problems
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

        for call in facts.calls() {
            calls
                .entry(call.caller)
                .or_default()
                .push(CallSite { file, call });
        }
        for root in facts.resolver_roots() {
            if seen_roots.insert(root.definition) {
                cycle_roots.push(root.definition);
                if root.unsupported_function_suppression.is_none() {
                    unsupported_roots.push(root.definition);
                }
            }
        }
        suppression_problems.extend(facts.suppression_problems().iter().map(|problem| {
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
