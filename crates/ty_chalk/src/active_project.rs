use ruff_db::files::{File, system_path_to_file};
use ruff_db::system::{SystemPath, SystemPathBuf};
use salsa::Setter;
use ty_project::Db;
use ty_project::watch::ChangeEvent;

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
            project.root().to_path_buf(),
            project.config_path().to_path_buf(),
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
        if !path.starts_with(self.project.root())
            || !is_source(path)
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

        if !path.starts_with(self.project.root()) {
            return false;
        }

        match change {
            ChangeEvent::Opened(_) | ChangeEvent::Created { .. } | ChangeEvent::Deleted { .. } => {
                true
            }
            ChangeEvent::Changed { .. } => {
                path == self.project.config_path()
                    || matches!(path.file_name(), Some(".gitignore" | ".chalkignore"))
                    || !is_source(path)
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
    let ignore_path = project.configured_ignore_path(db.system())?;
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

fn is_source(path: &SystemPath) -> bool {
    path.extension() == Some("py")
        || path
            .file_name()
            .is_some_and(|name| name.ends_with(".chalk.sql"))
}
