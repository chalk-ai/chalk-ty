use ruff_db::diagnostic::Diagnostic;
use ruff_db::files::File;
use rustc_hash::FxHashSet;
use ty_chalk::{ChalkProjectInput, chalk_diagnostics_for_file};
use ty_ide::hints;
use ty_project::{ProgressReporter, ProjectDatabase};

use super::WorkspaceDiagnosticsProgressReporter;
use crate::session::{RoutedProject, SessionSnapshot};

/// Adds Chalk diagnostics to one routed project's ordinary workspace-diagnostic pass.
pub(super) struct ProjectReporter<'reporter, 'snapshot> {
    reporter: &'reporter mut WorkspaceDiagnosticsProgressReporter<'snapshot>,
    db: &'snapshot ProjectDatabase,
    chalk_project: Option<ChalkProjectInput>,
    chalk_only_files: Vec<File>,
}

impl<'reporter, 'snapshot> ProjectReporter<'reporter, 'snapshot> {
    pub(super) fn new(
        reporter: &'reporter mut WorkspaceDiagnosticsProgressReporter<'snapshot>,
        snapshot: &'snapshot SessionSnapshot,
        project: &'snapshot RoutedProject,
        checked_files: &[File],
    ) -> Self {
        let db = project.db();
        let checked_files: FxHashSet<_> = checked_files.iter().copied().collect();
        let chalk_project = project.chalk_project();

        // Chalk source membership is independent of ty's indexed diagnostic files. Report the
        // remainder separately instead of asking ty to check files its configuration excludes.
        let chalk_only_files = chalk_project
            .map(|chalk_project| {
                chalk_project
                    .python_files(db)
                    .filter(|file| snapshot.project_owns_file(project, *file))
                    .filter(|file| !checked_files.contains(file))
                    .collect()
            })
            .unwrap_or_default();

        Self {
            reporter,
            db,
            chalk_project,
            chalk_only_files,
        }
    }

    pub(super) fn finish(self) {
        let Some(chalk_project) = self.chalk_project else {
            return;
        };

        for file in self.chalk_only_files {
            let chalk_diagnostics = chalk_diagnostics_for_file(self.db, chalk_project, file);
            self.reporter
                .report_file(self.db, file, &[], &chalk_diagnostics, &[]);
        }
    }
}

impl ProgressReporter for ProjectReporter<'_, '_> {
    fn set_files(&mut self, files: usize) {
        ProgressReporter::set_files(&mut *self.reporter, files + self.chalk_only_files.len());
    }

    fn report_checked_file(&self, db: &ProjectDatabase, file: File, diagnostics: &[Diagnostic]) {
        let chalk_diagnostics = self
            .chalk_project
            .map(|project| chalk_diagnostics_for_file(db, project, file))
            .unwrap_or_default();
        let unnecessary_hints = hints(db, file);
        self.reporter.report_file(
            db,
            file,
            diagnostics,
            &chalk_diagnostics,
            &unnecessary_hints,
        );
    }

    fn report_diagnostics(&mut self, db: &ProjectDatabase, diagnostics: Vec<Diagnostic>) {
        ProgressReporter::report_diagnostics(&mut *self.reporter, db, diagnostics);
    }
}
