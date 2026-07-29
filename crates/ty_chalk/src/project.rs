use std::collections::HashMap;
use std::io;

use ignore::Match;
use ignore::gitignore::{Gitignore, GitignoreBuilder};
use ruff_db::system::{DirectoryEntry, System, SystemPath, SystemPathBuf};
use serde::Deserialize;
use thiserror::Error;

const PROJECT_CONFIG_FILENAMES: [&str; 2] = ["chalk.yaml", "chalk.yml"];
const BUILT_IN_EXCLUSIONS: &[&str] = &[
    "*.egg*",
    "*.iml",
    "*.ipynb_checkpoints*",
    "*.pyc",
    "*.py~",
    "*venv",
    ".DS_Store",
    ".git",
    ".github",
    ".idea",
    ".vscode",
    "__pycache__",
    "node_modules",
    "venv",
];

/// A Chalk project selected by the nearest `chalk.yml` or `chalk.yaml`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ChalkProject {
    pub root: SystemPathBuf,
    pub config_path: SystemPathBuf,
}

impl ChalkProject {
    /// Recomputes and returns the project's apply/import source membership.
    ///
    /// The result is sorted by path and contains Python and Chalk SQL sources.
    pub fn source_files(
        &self,
        system: &dyn System,
    ) -> Result<Vec<SystemPathBuf>, ChalkProjectError> {
        let matcher = ProjectIgnoreMatcher::new(system, self)?;
        let mut sources = Vec::new();
        matcher.walk_sources(system, &self.root, &mut sources)?;
        sources.sort();
        Ok(sources)
    }

    /// Recomputes whether an existing file belongs to this project's apply/import sources.
    pub fn contains_source(
        &self,
        system: &dyn System,
        path: &SystemPath,
    ) -> Result<bool, ChalkProjectError> {
        let path = SystemPath::absolute(path, system.current_directory());
        if !path.starts_with(&self.root) || !system.is_file(&path) || !is_source_candidate(&path) {
            return Ok(false);
        }

        Ok(!ProjectIgnoreMatcher::new(system, self)?.is_ignored(&path, false))
    }
}

/// Finds the nearest Chalk project for a relevant file path.
///
/// This walks only the path's ancestors. It does not search workspace roots.
pub fn discover_chalk_project(system: &dyn System, file_path: &SystemPath) -> Option<ChalkProject> {
    let file_path = SystemPath::absolute(file_path, system.current_directory());
    let start = if system.is_directory(&file_path) {
        file_path.as_path()
    } else {
        file_path.parent()?
    };

    for directory in start.ancestors() {
        for filename in PROJECT_CONFIG_FILENAMES {
            let config_path = directory.join(filename);
            if system.is_file(&config_path) {
                return Some(ChalkProject {
                    root: directory.to_path_buf(),
                    config_path,
                });
            }
        }
    }

    None
}

#[derive(Debug, Error)]
pub enum ChalkProjectError {
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error(transparent)]
    Ignore(#[from] ignore::Error),
    #[error("failed to parse Chalk project configuration `{path}`: {error}")]
    Config {
        path: SystemPathBuf,
        #[source]
        error: serde_yaml::Error,
    },
    #[error("failed to resolve Chalk source `{path}`")]
    File {
        path: SystemPathBuf,
        #[source]
        source: ruff_db::files::FileError,
    },
}

#[derive(Debug)]
struct ProjectIgnoreMatcher {
    root: SystemPathBuf,
    gitignores: Vec<GitignoreEntry>,
    chalkignore: Gitignore,
    built_in: Gitignore,
}

impl ProjectIgnoreMatcher {
    fn new(system: &dyn System, project: &ChalkProject) -> Result<Self, ChalkProjectError> {
        let root = project.root.clone();
        let gitignores = load_gitignores(system, &root)?;
        let chalkignore_path = configured_chalkignore(system, project)?;
        let chalkignore = build_ignore_file(system, &root, &chalkignore_path)?;

        let mut built_in = GitignoreBuilder::new(root.as_std_path());
        for pattern in BUILT_IN_EXCLUSIONS {
            built_in.add_line(None, pattern)?;
        }

        Ok(Self {
            root,
            gitignores,
            chalkignore,
            built_in: built_in.build()?,
        })
    }

    fn is_ignored(&self, path: &SystemPath, is_directory: bool) -> bool {
        if !path.starts_with(&self.root) {
            return false;
        }

        let mut result = match_gitignore_entries(&self.gitignores, path, is_directory);
        result = fold_match(
            result,
            &self
                .chalkignore
                .matched_path_or_any_parents(path.as_std_path(), is_directory),
        );
        result = fold_match(
            result,
            &self
                .built_in
                .matched_path_or_any_parents(path.as_std_path(), is_directory),
        );

        matches!(result, Some(MatchKind::Ignore))
    }

    fn walk_sources(
        &self,
        system: &dyn System,
        directory: &SystemPath,
        sources: &mut Vec<SystemPathBuf>,
    ) -> Result<(), ChalkProjectError> {
        for entry in sorted_directory_entries(system, directory)? {
            let path = entry.path();
            let is_directory = entry.file_type().is_directory();

            if self.is_ignored(path, is_directory) {
                continue;
            }

            if is_directory {
                self.walk_sources(system, path, sources)?;
            } else if is_source_candidate(path) {
                sources.push(path.to_path_buf());
            }
        }

        Ok(())
    }
}

#[derive(Clone, Debug)]
struct GitignoreEntry {
    directory: SystemPathBuf,
    matcher: Gitignore,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum MatchKind {
    Ignore,
    Whitelist,
}

#[derive(Default, Deserialize)]
struct ChalkConfig {
    chalkignore: Option<String>,
    #[serde(default)]
    environments: HashMap<String, EnvironmentConfig>,
}

#[derive(Default, Deserialize)]
struct EnvironmentConfig {
    chalkignore: Option<String>,
}

pub(crate) fn configured_chalkignore(
    system: &dyn System,
    project: &ChalkProject,
) -> Result<SystemPathBuf, ChalkProjectError> {
    let contents = system.read_to_string(&project.config_path)?;
    let config: ChalkConfig =
        serde_yaml::from_str(&contents).map_err(|error| ChalkProjectError::Config {
            path: project.config_path.clone(),
            error,
        })?;

    if let Some(path) = config
        .environments
        .get("default")
        .and_then(|environment| environment.chalkignore.as_deref())
        .filter(|path| !path.is_empty())
    {
        return Ok(SystemPath::absolute(path, &project.root));
    }

    if let Some(path) = config
        .chalkignore
        .as_deref()
        .filter(|path| !path.is_empty())
    {
        let path = SystemPath::absolute(path, &project.root);
        if system.path_exists(&path) {
            return Ok(path);
        }
    }

    Ok(project.root.join(".chalkignore"))
}

fn load_gitignores(
    system: &dyn System,
    root: &SystemPath,
) -> Result<Vec<GitignoreEntry>, ChalkProjectError> {
    let mut active_entries = Vec::new();
    let mut all_entries = Vec::new();
    collect_gitignores(system, root, &mut active_entries, &mut all_entries)?;
    Ok(all_entries)
}

fn collect_gitignores(
    system: &dyn System,
    directory: &SystemPath,
    active_entries: &mut Vec<GitignoreEntry>,
    all_entries: &mut Vec<GitignoreEntry>,
) -> Result<(), ChalkProjectError> {
    let mut should_pop = false;
    let ignore_path = directory.join(".gitignore");
    if system.is_file(&ignore_path) {
        let matcher = build_ignore_file(system, directory, &ignore_path)?;
        let entry = GitignoreEntry {
            directory: directory.to_path_buf(),
            matcher,
        };
        active_entries.push(entry.clone());
        all_entries.push(entry);
        should_pop = true;
    }

    for entry in sorted_directory_entries(system, directory)? {
        if !entry.file_type().is_directory() || entry.path().file_name() == Some(".git") {
            continue;
        }
        if matches!(
            match_gitignore_entries(active_entries, entry.path(), true),
            Some(MatchKind::Ignore)
        ) {
            continue;
        }
        collect_gitignores(system, entry.path(), active_entries, all_entries)?;
    }

    if should_pop {
        active_entries.pop();
    }

    Ok(())
}

fn build_ignore_file(
    system: &dyn System,
    root: &SystemPath,
    path: &SystemPath,
) -> Result<Gitignore, ChalkProjectError> {
    let mut builder = GitignoreBuilder::new(root.as_std_path());
    let contents = match system.read_to_string(path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(builder.build()?),
        Err(error) => return Err(error.into()),
    };

    for line in contents.lines() {
        builder.add_line(Some(path.as_std_path().to_path_buf()), line)?;
    }

    Ok(builder.build()?)
}

fn sorted_directory_entries(
    system: &dyn System,
    directory: &SystemPath,
) -> Result<Vec<DirectoryEntry>, ChalkProjectError> {
    let mut entries = system
        .read_directory(directory)?
        .collect::<Result<Vec<_>, io::Error>>()?;
    entries.sort_by(|left, right| left.path().cmp(right.path()));
    Ok(entries)
}

fn match_gitignore_entries(
    entries: &[GitignoreEntry],
    path: &SystemPath,
    is_directory: bool,
) -> Option<MatchKind> {
    let mut result = None;

    for entry in entries {
        if path.starts_with(&entry.directory) {
            result = fold_match(
                result,
                &entry
                    .matcher
                    .matched_path_or_any_parents(path.as_std_path(), is_directory),
            );
        }
    }

    result
}

fn fold_match(
    current: Option<MatchKind>,
    next: &Match<&ignore::gitignore::Glob>,
) -> Option<MatchKind> {
    match next {
        Match::None => current,
        Match::Ignore(_) => Some(MatchKind::Ignore),
        Match::Whitelist(_) => Some(MatchKind::Whitelist),
    }
}

pub(crate) fn is_source_candidate(path: &SystemPath) -> bool {
    path.extension() == Some("py")
        || path
            .file_name()
            .is_some_and(|name| name.ends_with(".chalk.sql"))
}

#[cfg(test)]
mod tests {
    use ruff_db::system::{InMemorySystem, System, SystemPath, SystemPathBuf};

    use super::{ChalkProject, discover_chalk_project};

    struct TestProject {
        system: InMemorySystem,
        root: SystemPathBuf,
        project: ChalkProject,
    }

    impl TestProject {
        fn new(config_name: &str, config: &str) -> Self {
            let system = InMemorySystem::new("/project".into());
            let root = system.current_directory().to_path_buf();
            system
                .fs()
                .write_file(root.join(config_name), config)
                .unwrap();
            let project = discover_chalk_project(&system, &root.join("src/app.py")).unwrap();
            Self {
                system,
                root,
                project,
            }
        }

        fn write_files<'a>(&self, files: impl IntoIterator<Item = (&'a str, &'a str)>) {
            self.system
                .fs()
                .write_files_all(
                    files
                        .into_iter()
                        .map(|(path, contents)| (self.root.join(path), contents)),
                )
                .unwrap();
        }

        fn relative_sources(&self) -> Vec<String> {
            self.project
                .source_files(&self.system)
                .unwrap()
                .into_iter()
                .map(|path| path.strip_prefix(&self.root).unwrap().to_string())
                .collect()
        }
    }

    #[test]
    fn discovers_nearest_yaml_or_yml_marker() {
        let system = InMemorySystem::new("/workspace".into());
        system
            .fs()
            .write_files_all([
                (SystemPathBuf::from("/workspace/chalk.yml"), ""),
                (SystemPathBuf::from("/workspace/nested/chalk.yaml"), ""),
            ])
            .unwrap();

        let yml = discover_chalk_project(&system, SystemPath::new("/workspace/file.py")).unwrap();
        assert_eq!(yml.root, SystemPath::new("/workspace"));
        assert_eq!(yml.config_path, SystemPath::new("/workspace/chalk.yml"));

        let yaml =
            discover_chalk_project(&system, SystemPath::new("/workspace/nested/src/file.py"))
                .unwrap();
        assert_eq!(yaml.root, SystemPath::new("/workspace/nested"));
        assert_eq!(
            yaml.config_path,
            SystemPath::new("/workspace/nested/chalk.yaml")
        );

        assert!(discover_chalk_project(&system, SystemPath::new("/unrelated/file.py")).is_none());
    }

    #[test]
    fn applies_built_ins_nested_gitignore_and_extensions_deterministically() {
        let test = TestProject::new("chalk.yaml", "{}");
        test.write_files([
            ("z.py", ""),
            ("a.chalk.sql", ""),
            ("nested/.gitignore", "ignored.py\n"),
            ("nested/ignored.py", ""),
            ("nested/keep.py", ""),
            ("node_modules/package.py", ""),
            ("venv/lib.py", ""),
            ("notes.sql", ""),
            ("module.pyi", ""),
        ]);

        assert_eq!(
            test.relative_sources(),
            ["a.chalk.sql", "nested/keep.py", "z.py"]
        );
        assert!(
            test.project
                .contains_source(&test.system, &test.root.join("nested/keep.py"))
                .unwrap()
        );
        assert!(
            !test
                .project
                .contains_source(&test.system, &test.root.join("nested/ignored.py"))
                .unwrap()
        );
        assert!(
            !test
                .project
                .contains_source(&test.system, SystemPath::new("/other/file.py"))
                .unwrap()
        );
    }

    #[test]
    fn applies_default_chalkignore() {
        let test = TestProject::new("chalk.yml", "{}");
        test.write_files([
            (".chalkignore", "ignored.py\n"),
            ("ignored.py", ""),
            ("keep.py", ""),
        ]);

        assert_eq!(test.relative_sources(), ["keep.py"]);
    }

    #[test]
    fn applies_configured_default_environment_chalkignore() {
        let test = TestProject::new(
            "chalk.yaml",
            "chalkignore: project.ignore\nenvironments:\n  default:\n    chalkignore: default.ignore\n",
        );
        test.write_files([
            ("project.ignore", "project-only.py\n"),
            ("default.ignore", "default-only.py\n"),
            ("project-only.py", ""),
            ("default-only.py", ""),
            ("keep.py", ""),
        ]);

        assert_eq!(test.relative_sources(), ["keep.py", "project-only.py"]);
    }

    #[test]
    fn applies_project_level_configured_chalkignore() {
        let test = TestProject::new("chalk.yaml", "chalkignore: custom.ignore\n");
        test.write_files([
            (".chalkignore", "default-only.py\n"),
            ("custom.ignore", "custom-only.py\n"),
            ("default-only.py", ""),
            ("custom-only.py", ""),
            ("keep.py", ""),
        ]);

        assert_eq!(test.relative_sources(), ["default-only.py", "keep.py"]);
    }
}
