use ruff_db::files::{File, system_path_to_file};
use ruff_db::system::{SystemPath, SystemPathBuf};
use salsa::Setter;
use ty_project::Db;
use ty_project::watch::ChangeEvent;

use crate::project::{configured_chalkignore, is_source_candidate};
use crate::{ChalkProject, ChalkProjectError};

/// The active Chalk project visible to Salsa queries in a project database.
///
/// The source files are exact [`File`] identities from the owning project
/// database. Their order is deterministic because [`ChalkProject::source_files`]
/// sorts paths before they are converted.
#[salsa::input(debug, heap_size=ruff_memory_usage::heap_size)]
pub struct ChalkProjectInput {
    #[returns(deref)]
    pub root: SystemPathBuf,
    #[returns(deref)]
    pub config_path: SystemPathBuf,
    #[returns(deref)]
    pub source_files: Box<[File]>,
}

impl ChalkProjectInput {
    pub fn python_files(self, db: &dyn Db) -> impl Iterator<Item = File> + use<'_> {
        self.source_files(db)
            .iter()
            .copied()
            .filter(move |file| file.path(db).extension() == Some("py"))
    }

    pub fn chalk_sql_files(self, db: &dyn Db) -> impl Iterator<Item = File> + use<'_> {
        self.source_files(db).iter().copied().filter(move |file| {
            file.path(db)
                .as_system_path()
                .and_then(SystemPath::file_name)
                .is_some_and(|name| name.ends_with(".chalk.sql"))
        })
    }
}

/// Owns the mutable active-project input for one project database.
#[derive(Debug)]
pub struct ActiveChalkProject {
    project: ChalkProject,
    input: ChalkProjectInput,
    ignore_path: SystemPathBuf,
    open_sources: Vec<File>,
}

impl ActiveChalkProject {
    pub fn new(db: &dyn Db, project: ChalkProject) -> Result<Self, ChalkProjectError> {
        let (source_files, ignore_path) = source_files(db, &project, &[])?;
        let input = ChalkProjectInput::new(
            db,
            project.root.clone(),
            project.config_path.clone(),
            source_files,
        );

        Ok(Self {
            project,
            input,
            ignore_path,
            open_sources: Vec::new(),
        })
    }

    pub fn project(&self) -> &ChalkProject {
        &self.project
    }

    pub fn input(&self) -> ChalkProjectInput {
        self.input
    }

    /// Retains an open source so refreshes can include it even when it is absent
    /// from the native filesystem.
    pub fn open_source(&mut self, db: &dyn Db, file: File) -> bool {
        let Some(path) = file.path(db).as_system_path() else {
            return false;
        };
        if !path.starts_with(&self.project.root)
            || !is_source_candidate(path)
            || self.open_sources.contains(&file)
        {
            return false;
        }

        self.open_sources.push(file);
        true
    }

    /// Stops retaining an open source. A following refresh still includes it
    /// when it remains a native, non-ignored source.
    pub fn close_source(&mut self, file: File) -> bool {
        let previous_len = self.open_sources.len();
        self.open_sources.retain(|open_file| *open_file != file);
        self.open_sources.len() != previous_len
    }

    /// Recomputes the source set and updates Salsa only if membership changed.
    ///
    /// Errors leave both the input and the last successfully resolved ignore
    /// path unchanged.
    pub fn refresh(&mut self, db: &mut dyn Db) -> Result<bool, ChalkProjectError> {
        let (source_files, ignore_path) = source_files(db, &self.project, &self.open_sources)?;
        let changed = self.input.source_files(db) != source_files.as_ref();

        if changed {
            self.input.set_source_files(db).to(source_files);
        }
        self.ignore_path = ignore_path;

        Ok(changed)
    }

    /// Returns whether a filesystem change can affect source-set membership.
    pub fn should_refresh(&self, change: &ChangeEvent) -> bool {
        if matches!(change, ChangeEvent::Rescan) {
            return true;
        }

        let Some(path) = change.system_path() else {
            return false;
        };

        if matches!(
            change,
            ChangeEvent::Created { .. } | ChangeEvent::Deleted { .. }
        ) && matches!(path.file_name(), Some("chalk.yml" | "chalk.yaml"))
        {
            return false;
        }

        if path == self.ignore_path.as_path() {
            return true;
        }

        if !path.starts_with(&self.project.root) {
            return false;
        }

        match change {
            ChangeEvent::Opened(_) | ChangeEvent::Created { .. } | ChangeEvent::Deleted { .. } => {
                true
            }
            ChangeEvent::Changed { .. } => {
                path == self.project.config_path.as_path()
                    || matches!(path.file_name(), Some(".gitignore" | ".chalkignore"))
            }
            ChangeEvent::CreatedVirtual(_)
            | ChangeEvent::ChangedVirtual(_)
            | ChangeEvent::DeletedVirtual(_)
            | ChangeEvent::Rescan => false,
        }
    }
}

fn source_files(
    db: &dyn Db,
    project: &ChalkProject,
    open_sources: &[File],
) -> Result<(Box<[File]>, SystemPathBuf), ChalkProjectError> {
    let ignore_path = configured_chalkignore(db.system(), project)?;
    let mut source_files = project
        .source_files(db.system())?
        .into_iter()
        .map(|path| {
            system_path_to_file(db, &path)
                .map_err(|source| ChalkProjectError::File { path, source })
        })
        .collect::<Result<Vec<_>, _>>()?;

    for file in open_sources {
        let Some(path) = file.path(db).as_system_path() else {
            continue;
        };
        if !source_files.contains(file) && project.contains_source(db.system(), path)? {
            source_files.push(*file);
        }
    }
    source_files.sort_by(|left, right| left.path(db).as_str().cmp(right.path(db).as_str()));

    Ok((source_files.into_boxed_slice(), ignore_path))
}

#[cfg(test)]
mod tests {
    use ruff_db::Db as _;
    use ruff_db::system::{
        DbWithTestSystem as _, DbWithWritableSystem as _, SystemPath, SystemPathBuf,
    };
    use ty_project::watch::{ChangedKind, CreatedKind, DeletedKind};
    use ty_project::{ProjectMetadata, TestDb};

    use super::{ActiveChalkProject, ChangeEvent};
    use crate::discover_chalk_project;

    fn test_db() -> TestDb {
        TestDb::new(ProjectMetadata::new(
            "workspace",
            SystemPathBuf::from("/workspace"),
        ))
    }

    fn active_project(db: &TestDb, root: &SystemPath) -> ActiveChalkProject {
        let project = discover_chalk_project(db.system(), &root.join("src/main.py")).unwrap();
        ActiveChalkProject::new(db, project).unwrap()
    }

    fn paths(db: &TestDb, project: &ActiveChalkProject) -> Vec<String> {
        project
            .input()
            .source_files(db)
            .iter()
            .map(|file| file.path(db).as_str().to_string())
            .collect()
    }

    #[test]
    fn deterministic_typed_source_set() {
        let mut db = test_db();
        db.write_file("/workspace/chalk.yml", "").unwrap();
        db.write_file("/workspace/z.py", "").unwrap();
        db.write_file("/workspace/a.chalk.sql", "").unwrap();
        db.write_file("/workspace/not-sql.sql", "").unwrap();

        let project = active_project(&db, SystemPath::new("/workspace"));
        let input = project.input();

        assert_eq!(input.root(&db), SystemPath::new("/workspace"));
        assert_eq!(
            input.config_path(&db),
            SystemPath::new("/workspace/chalk.yml")
        );
        assert_eq!(
            paths(&db, &project),
            ["/workspace/a.chalk.sql", "/workspace/z.py"]
        );
        assert_eq!(
            input
                .python_files(&db)
                .map(|file| file.path(&db).as_str())
                .collect::<Vec<_>>(),
            ["/workspace/z.py"]
        );
        assert_eq!(
            input
                .chalk_sql_files(&db)
                .map(|file| file.path(&db).as_str())
                .collect::<Vec<_>>(),
            ["/workspace/a.chalk.sql"]
        );
    }

    #[test]
    fn refreshes_add_delete_and_skips_source_content_changes() {
        let mut db = test_db();
        db.write_file("/workspace/chalk.yml", "").unwrap();
        db.write_file("/workspace/old.py", "").unwrap();
        let mut project = active_project(&db, SystemPath::new("/workspace"));

        let content_change =
            ChangeEvent::file_content_changed(SystemPathBuf::from("/workspace/old.py"));
        assert!(!project.should_refresh(&content_change));
        assert!(!project.refresh(&mut db).unwrap());

        let unrelated_change =
            ChangeEvent::file_content_changed(SystemPathBuf::from("/workspace/notes.txt"));
        assert!(!project.should_refresh(&unrelated_change));

        db.write_file("/workspace/new.py", "").unwrap();
        let created = ChangeEvent::Created {
            path: SystemPathBuf::from("/workspace/new.py"),
            kind: CreatedKind::File,
        };
        assert!(project.should_refresh(&created));
        assert!(project.refresh(&mut db).unwrap());
        assert_eq!(
            paths(&db, &project),
            ["/workspace/new.py", "/workspace/old.py"]
        );

        db.memory_file_system()
            .remove_file(SystemPath::new("/workspace/old.py"))
            .unwrap();
        let deleted = ChangeEvent::Deleted {
            path: SystemPathBuf::from("/workspace/old.py"),
            kind: DeletedKind::File,
        };
        assert!(project.should_refresh(&deleted));
        assert!(project.refresh(&mut db).unwrap());
        assert_eq!(paths(&db, &project), ["/workspace/new.py"]);
    }

    #[test]
    fn refreshes_configured_and_nested_ignore_changes() {
        let mut db = test_db();
        db.write_file(
            "/workspace/chalk.yml",
            "chalkignore: /global/chalk.ignore\n",
        )
        .unwrap();
        db.write_file("/global/chalk.ignore", "ignored.py\n")
            .unwrap();
        db.write_file("/workspace/ignored.py", "").unwrap();
        db.write_file("/workspace/pkg/.gitignore", "nested.py\n")
            .unwrap();
        db.write_file("/workspace/pkg/nested.py", "").unwrap();
        let mut project = active_project(&db, SystemPath::new("/workspace"));
        assert!(paths(&db, &project).is_empty());

        db.write_file("/global/chalk.ignore", "").unwrap();
        let configured_ignore = ChangeEvent::Changed {
            path: SystemPathBuf::from("/global/chalk.ignore"),
            kind: ChangedKind::FileContent,
        };
        assert!(project.should_refresh(&configured_ignore));
        assert!(project.refresh(&mut db).unwrap());
        assert_eq!(paths(&db, &project), ["/workspace/ignored.py"]);

        db.write_file("/workspace/pkg/.gitignore", "").unwrap();
        let gitignore = ChangeEvent::Changed {
            path: SystemPathBuf::from("/workspace/pkg/.gitignore"),
            kind: ChangedKind::FileContent,
        };
        assert!(project.should_refresh(&gitignore));
        assert!(project.refresh(&mut db).unwrap());
        assert_eq!(
            paths(&db, &project),
            ["/workspace/ignored.py", "/workspace/pkg/nested.py"]
        );
    }

    #[test]
    fn marker_deletion_preserves_last_valid_input() {
        let mut db = test_db();
        db.write_file("/workspace/chalk.yml", "").unwrap();
        db.write_file("/workspace/main.py", "").unwrap();
        let project = active_project(&db, SystemPath::new("/workspace"));
        let before = paths(&db, &project);

        db.memory_file_system()
            .remove_file(SystemPath::new("/workspace/chalk.yml"))
            .unwrap();
        let deleted = ChangeEvent::Deleted {
            path: SystemPathBuf::from("/workspace/chalk.yml"),
            kind: DeletedKind::File,
        };
        assert!(!project.should_refresh(&deleted));
        assert!(!project.should_refresh(&ChangeEvent::Created {
            path: SystemPathBuf::from("/workspace/chalk.yml"),
            kind: CreatedKind::File,
        }));
        assert_eq!(paths(&db, &project), before);
    }

    #[test]
    fn refresh_error_preserves_last_valid_input() {
        let mut db = test_db();
        db.write_file("/workspace/chalk.yml", "").unwrap();
        db.write_file("/workspace/main.py", "").unwrap();
        let mut project = active_project(&db, SystemPath::new("/workspace"));
        let before = paths(&db, &project);

        db.write_file("/workspace/chalk.yml", "chalkignore: [")
            .unwrap();
        let changed = ChangeEvent::Changed {
            path: SystemPathBuf::from("/workspace/chalk.yml"),
            kind: ChangedKind::FileContent,
        };
        assert!(project.should_refresh(&changed));
        assert!(project.refresh(&mut db).is_err());
        assert_eq!(paths(&db, &project), before);
    }

    #[test]
    fn sibling_projects_are_isolated() {
        let mut db = test_db();
        db.write_file("/workspace/a/chalk.yml", "").unwrap();
        db.write_file("/workspace/a/main.py", "").unwrap();
        db.write_file("/workspace/b/chalk.yml", "").unwrap();
        db.write_file("/workspace/b/main.py", "").unwrap();
        let mut a = active_project(&db, SystemPath::new("/workspace/a"));
        let b = active_project(&db, SystemPath::new("/workspace/b"));
        let b_before = paths(&db, &b);

        db.write_file("/workspace/a/new.py", "").unwrap();
        let created = ChangeEvent::Created {
            path: SystemPathBuf::from("/workspace/a/new.py"),
            kind: CreatedKind::File,
        };
        assert!(a.should_refresh(&created));
        assert!(!b.should_refresh(&created));
        assert!(a.refresh(&mut db).unwrap());
        assert_eq!(
            paths(&db, &a),
            ["/workspace/a/main.py", "/workspace/a/new.py"]
        );
        assert_eq!(paths(&db, &b), b_before);
    }
}
