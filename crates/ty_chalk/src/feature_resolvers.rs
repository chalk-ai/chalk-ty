//! On-demand navigation from feature declarations to Python resolver outputs.

use ruff_db::files::{File, FileRange};
use ruff_db::parsed::parsed_module;
use ruff_db::source::source_text;
use ruff_python_ast::visitor::source_order::{SourceOrderVisitor, walk_expr, walk_stmt};
use ruff_python_ast::{self as ast, AnyNodeRef};
use ruff_text_size::{Ranged, TextRange};
use ty_project::Db;
use ty_python_semantic::{HasDefinition, SemanticModel, definitions_for_attribute};

use crate::resolvers::resolver_function_ranges;
use crate::{ChalkProjectInput, discover_chalk_project};

/// Finds resolvers whose explicit return annotation refers to a selected feature declaration.
///
/// The search runs only when navigation is requested and retains no project-wide output index.
pub fn feature_resolvers(
    db: &dyn Db,
    project: ChalkProjectInput,
    name: &str,
    declarations: &[FileRange],
) -> Vec<FileRange> {
    if name.is_empty() || declarations.is_empty() {
        return Vec::new();
    }

    let mut results = Vec::new();
    for file in project.python_files(db) {
        if let Some(path) = file.path(db).as_system_path()
            && let Some(owner) = discover_chalk_project(db.system(), path)
            && owner.config_path.as_path() != project.config_path(db)
        {
            continue;
        }
        if !source_text(db, file).as_str().contains(name) {
            continue;
        }

        let resolver_ranges = resolver_function_ranges(db, file);
        if resolver_ranges.is_empty() {
            continue;
        }

        let parsed = parsed_module(db, file).load(db);
        let model = SemanticModel::new(db, file);
        let mut visitor = ResolverVisitor {
            db,
            file,
            model: &model,
            resolver_ranges,
            declarations,
            results: &mut results,
        };
        AnyNodeRef::from(parsed.syntax()).visit_source_order(&mut visitor);
    }
    results.sort_by_key(|range| (range.file().path(db).to_string(), range.range().start()));
    results.dedup();
    results
}

struct ResolverVisitor<'a, 'db> {
    db: &'db dyn Db,
    file: File,
    model: &'a SemanticModel<'db>,
    resolver_ranges: &'a [TextRange],
    declarations: &'a [FileRange],
    results: &'a mut Vec<FileRange>,
}

impl SourceOrderVisitor<'_> for ResolverVisitor<'_, '_> {
    fn visit_stmt(&mut self, statement: &ast::Stmt) {
        if let ast::Stmt::FunctionDef(function) = statement
            && self
                .resolver_ranges
                .binary_search_by_key(&function.start(), Ranged::start)
                .is_ok()
            && let Some(annotation) = function.returns.as_deref()
        {
            let mut output = OutputVisitor {
                model: self.model,
                declarations: self.declarations,
                matched: false,
            };
            output.visit_expr(annotation);
            if output.matched {
                let parsed = parsed_module(self.db, self.file).load(self.db);
                self.results.push(
                    function
                        .definition(self.model)
                        .focus_range(self.db, &parsed),
                );
            }
        }
        walk_stmt(self, statement);
    }
}

struct OutputVisitor<'a, 'db> {
    model: &'a SemanticModel<'db>,
    declarations: &'a [FileRange],
    matched: bool,
}

impl SourceOrderVisitor<'_> for OutputVisitor<'_, '_> {
    fn visit_expr(&mut self, expression: &ast::Expr) {
        if let ast::Expr::Attribute(attribute) = expression
            && definitions_for_attribute(self.model, attribute)
                .iter()
                .any(|definition| {
                    self.declarations
                        .contains(&definition.focus_range(self.model.db()))
                })
        {
            self.matched = true;
        }
        if !self.matched {
            walk_expr(self, expression);
        }
    }
}

#[cfg(test)]
mod tests {
    use ruff_db::Db as _;
    use ruff_db::files::{FileRange, system_path_to_file};
    use ruff_db::system::{
        DbWithTestSystem as _, DbWithWritableSystem as _, SystemPath, SystemPathBuf,
    };
    use ruff_db::testing::{
        assert_function_query_was_not_run, assert_function_query_was_not_run_by_name,
        assert_function_query_was_run,
    };
    use ruff_python_ast::PythonVersion;
    use ruff_text_size::{Ranged, TextRange, TextSize};
    use salsa::plumbing::AsId;
    use ty_module_resolver::SearchPathSettings;
    use ty_project::{ProjectMetadata, TestDb};
    use ty_python_core::platform::PythonPlatform;
    use ty_python_core::program::{FallibleStrategy, Program, ProgramSettings};
    use ty_python_core::scope::ScopeKind;
    use ty_python_core::semantic_index;
    use ty_python_semantic::{PythonVersionSource, PythonVersionWithSource};

    use crate::facts::file_facts;
    use crate::resolvers::resolver_function_ranges;

    use super::{ChalkProjectInput, feature_resolvers};

    #[test]
    fn finds_only_matching_resolver_outputs() {
        let mut db = TestDb::new(ProjectMetadata::new("test", SystemPathBuf::from("/src")));
        db.memory_file_system()
            .create_directory_all(SystemPath::new("/src"))
            .unwrap();
        db.memory_file_system()
            .create_directory_all(SystemPath::new("/site-packages/chalk/features"))
            .unwrap();
        db.write_file(
            "/site-packages/chalk/__init__.py",
            "from chalk.features.resolver import online as online\n",
        )
        .unwrap();
        db.write_file(
            "/site-packages/chalk/features/resolver.py",
            "def online(function): ...\n",
        )
        .unwrap();
        let features_source = "from chalk.features import features\n@features\nclass User:\n    score: int\n    id: int\n\n@features\nclass Other:\n    score: int\n";
        db.write_file("/src/features.py", features_source).unwrap();
        db.write_file("/src/resolvers.py", "from chalk import online\nfrom features import User as Account, Other\n\n@online\ndef resolve_score(id: Account.id) -> Account.score:\n    return 1\n\n@online\ndef resolve_other() -> Other.score:\n    return 2\n\ndef plain() -> Account.score:\n    return 3\n").unwrap();

        let search_paths = SearchPathSettings {
            extra_paths: vec![],
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

        let features = system_path_to_file(&db, "/src/features.py").unwrap();
        let resolvers = system_path_to_file(&db, "/src/resolvers.py").unwrap();
        let start = features_source.find("score: int").unwrap();
        let range = TextRange::new(
            TextSize::try_from(start).unwrap(),
            TextSize::try_from(start + 5).unwrap(),
        );
        let target = FileRange::new(features, range);
        let project = ChalkProjectInput::new(
            &db,
            SystemPathBuf::from("/src"),
            SystemPathBuf::from("/src/chalk.yml"),
            vec![features, resolvers].into_boxed_slice(),
        );

        db.take_salsa_events();
        let matches = feature_resolvers(&db, project, "score", &[target]);
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].file(), resolvers);
        let resolver_source = ruff_db::source::source_text(&db, resolvers);
        let range = matches[0].range();
        assert_eq!(
            &resolver_source.as_str()[usize::from(range.start())..usize::from(range.end())],
            "resolve_score"
        );

        let events = db.take_salsa_events();
        assert_function_query_was_run(&db, resolver_function_ranges, resolvers, &events);
        assert_function_query_was_not_run_by_name(&db, "file_facts", None, &events);
        for scope in semantic_index(&db, resolvers).scope_ids() {
            if scope.scope(&db).kind() == ScopeKind::Function {
                assert_function_query_was_not_run_by_name(
                    &db,
                    "infer_scope_types_impl",
                    Some(scope.as_id()),
                    &events,
                );
            }
        }

        assert_eq!(feature_resolvers(&db, project, "score", &[target]), matches);
        let events = db.take_salsa_events();
        assert_function_query_was_not_run(&db, resolver_function_ranges, resolvers, &events);

        // Diagnostics reuse the same discovery query after navigation.
        assert_eq!(file_facts(&db, resolvers).unwrap().resolver_roots.len(), 2);
        let events = db.take_salsa_events();
        assert_function_query_was_not_run(&db, resolver_function_ranges, resolvers, &events);
    }
}
