use std::panic::AssertUnwindSafe;

use ruff_db::files::File;
use ruff_db::parsed::parsed_module;
use ruff_python_ast::visitor::source_order::{SourceOrderVisitor, walk_expr, walk_stmt};
use ruff_python_ast::{self as ast};
use ruff_text_size::{Ranged, TextRange};
use ty_python_semantic::chalk::{CallTarget, Definition, ModuleOrigin};
use ty_python_semantic::{Db, HasDefinition, HasType, SemanticModel};

use crate::CallNoMatchReason;
use crate::call_matcher::{
    CallMatch, CallMatchTarget, ObservedArgument, ObservedCall, ObservedCallTarget,
    match_call_target,
};
use crate::suppression::{SuppressionCode, SuppressionProblem, Suppressions, extract_suppressions};

/// A named function recognized as a baked Chalk resolver root.
#[derive(Clone, Copy, Debug, Eq, PartialEq, salsa::Update, get_size2::GetSize)]
pub(crate) struct ResolverRootFact<'db> {
    pub(crate) definition: Definition<'db>,
    pub(crate) range: TextRange,
    /// The exact function-level directive suppressing this root, if any.
    pub(crate) unsupported_function_suppression: Option<TextRange>,
}

/// A call expression physically contained in a named function body.
#[derive(Debug, Eq, PartialEq, salsa::Update, get_size2::GetSize)]
pub(crate) struct CallFact<'db> {
    pub(crate) caller: Definition<'db>,
    pub(crate) range: TextRange,
    pub(crate) targets: Box<[CallTarget<'db>]>,
    pub(crate) no_matches: Box<[(CallMatchTarget<'db>, CallNoMatchReason)]>,
    /// The exact statement-level directive suppressing this call, if any.
    pub(crate) unsupported_function_statement_suppression: Option<TextRange>,
    /// The exact function-level directive suppressing this call's caller, if any.
    pub(crate) unsupported_function_caller_suppression: Option<TextRange>,
}

/// Compact Chalk-specific semantic facts extracted from one Python file.
#[derive(Debug, Eq, PartialEq, salsa::Update, get_size2::GetSize)]
pub(crate) struct FileFacts<'db> {
    resolver_roots: Box<[ResolverRootFact<'db>]>,
    calls: Box<[CallFact<'db>]>,
    suppression_problems: Box<[SuppressionProblem]>,
}

impl<'db> FileFacts<'db> {
    pub(crate) fn resolver_roots(&self) -> &[ResolverRootFact<'db>] {
        &self.resolver_roots
    }

    pub(crate) fn calls(&self) -> &[CallFact<'db>] {
        &self.calls
    }

    pub(crate) fn suppression_problems(&self) -> &[SuppressionProblem] {
        &self.suppression_problems
    }
}

/// Extracts source-ordered Chalk facts without retaining AST nodes or general native types.
fn extract_file_facts(db: &dyn Db, file: File) -> FileFacts<'_> {
    let parsed = parsed_module(db, file).load(db);
    let model = SemanticModel::new(db, file);
    let suppressions = extract_suppressions(db, file);
    let mut visitor = FactVisitor {
        model: &model,
        suppressions: &suppressions,
        caller: None,
        statement: None,
        resolver_roots: Vec::new(),
        calls: Vec::new(),
    };
    visitor.visit_body(&parsed.syntax().body);

    FileFacts {
        resolver_roots: visitor.resolver_roots.into_boxed_slice(),
        calls: visitor.calls.into_boxed_slice(),
        suppression_problems: suppressions.problems().into(),
    }
}

/// Returns cached Chalk facts after the file completes ordinary semantic analysis.
///
/// Recoverable parse and type errors do not discard facts. A fatal analysis abort, such as a
/// source read failure, returns `None`.
#[salsa::tracked(returns(as_ref), heap_size=ruff_memory_usage::heap_size)]
pub(crate) fn file_facts(db: &dyn Db, file: File) -> Option<FileFacts<'_>> {
    let unwind_safe_db = AssertUnwindSafe(db);
    match ruff_db::panic::catch_unwind(|| {
        ty_python_semantic::check_file(*unwind_safe_db, file).ok()?;
        Some(extract_file_facts(*unwind_safe_db, file))
    }) {
        Ok(facts) => facts,
        Err(error) => {
            match error.payload.downcast_ref::<salsa::Cancelled>() {
                None => {}
                Some(salsa::Cancelled::PropagatedPanic) => {
                    db.unwind_if_revision_cancelled();
                }
                Some(_) => error.resume_unwind(),
            }

            // Panicked queries do not preserve their dependencies for Salsa.
            db.report_untracked_read();
            None
        }
    }
}

struct FactVisitor<'a, 'db> {
    model: &'a SemanticModel<'db>,
    suppressions: &'a Suppressions<'db>,
    caller: Option<Definition<'db>>,
    statement: Option<TextRange>,
    resolver_roots: Vec<ResolverRootFact<'db>>,
    calls: Vec<CallFact<'db>>,
}

impl<'a, 'db> FactVisitor<'a, 'db> {
    fn visit_function(&mut self, function: &'a ast::StmtFunctionDef) {
        let definition = function.definition(self.model);

        if function
            .decorator_list
            .iter()
            .any(|decorator| self.is_resolver_decorator(decorator))
        {
            self.resolver_roots.push(ResolverRootFact {
                definition,
                range: function.range,
                unsupported_function_suppression: self.function_suppression(definition),
            });
        }

        let enclosing_caller = self.caller.replace(definition);
        self.visit_body(&function.body);
        self.caller = enclosing_caller;
    }

    fn is_resolver_decorator(&self, decorator: &ast::Decorator) -> bool {
        let provenance = self.model.chalk_decorator_provenance(decorator);

        if !provenance.definitions.is_empty() {
            !provenance.has_unresolved
                && provenance.definitions.iter().all(|definition| {
                    self.model
                        .chalk_decorator_definition_origin(*definition)
                        .is_some_and(|origin| {
                            is_baked_resolver_origin(
                                origin.ownership_origin(),
                                origin.module_name(),
                                origin.symbol_name(),
                            )
                        })
                })
        } else {
            provenance.module_fallback_is_complete
                && !provenance.modules.is_empty()
                && provenance.modules.iter().all(|origin| {
                    is_baked_resolver_origin(
                        origin.ownership_origin(),
                        origin.module_name(),
                        origin.symbol_name(),
                    )
                })
        }
    }

    fn function_suppression(&self, definition: Definition<'db>) -> Option<TextRange> {
        self.suppressions
            .function()
            .iter()
            .find(|suppression| {
                suppression.definition == definition
                    && suppression.code == SuppressionCode::UnsupportedFunction
            })
            .map(|suppression| suppression.directive)
    }

    fn statement_suppression(&self, statement: TextRange) -> Option<TextRange> {
        self.suppressions
            .statement()
            .iter()
            .find(|suppression| {
                suppression.statement == statement
                    && suppression.code == SuppressionCode::UnsupportedFunction
            })
            .map(|suppression| suppression.directive)
    }

    fn visit_call(&mut self, call: &ast::ExprCall) {
        let (Some(caller), Some(statement)) = (self.caller, self.statement) else {
            return;
        };
        let targets = self.model.chalk_call_targets(call);
        let module_provenance = self.model.chalk_call_module_provenance(call);
        let has_argument_unpacking = call.arguments.args.iter().any(ast::Expr::is_starred_expr)
            || call
                .arguments
                .keywords
                .iter()
                .any(|keyword| keyword.arg.is_none());
        let arguments = if has_argument_unpacking {
            None
        } else {
            call.arguments
                .iter_source_order()
                .map(|argument| match argument {
                    ast::ArgOrKeyword::Arg(argument) => argument
                        .inferred_type(self.model)
                        .map(ObservedArgument::Positional),
                    ast::ArgOrKeyword::Keyword(keyword) => Some(ObservedArgument::Keyword {
                        name: keyword.arg.as_ref()?.id.as_str(),
                        ty: keyword.value.inferred_type(self.model)?,
                    }),
                })
                .collect::<Option<Vec<_>>>()
        };
        let observed_arguments = arguments.as_deref().unwrap_or_default();
        let receiver = call
            .func
            .as_attribute_expr()
            .and_then(|attribute| attribute.value.inferred_type(self.model));
        let defer_matching = arguments.is_none();
        let observed_receiver = if defer_matching { None } else { receiver };
        let mut no_matches = Vec::new();
        let mut record_match = |target| {
            let CallMatch::NoMatch { target, reason } = match_call_target(
                self.model.db(),
                ObservedCall {
                    target: if defer_matching {
                        ObservedCallTarget::Deferred
                    } else {
                        target
                    },
                    arguments: observed_arguments,
                    receiver: observed_receiver,
                },
            ) else {
                return;
            };
            let no_match = (target, reason);
            if !no_matches.contains(&no_match) {
                no_matches.push(no_match);
            }
        };
        for target in &targets.targets {
            record_match(ObservedCallTarget::Resolved(*target));
        }
        for target in &targets.known_targets {
            record_match(ObservedCallTarget::Known(*target));
        }
        for provenance in &module_provenance {
            record_match(ObservedCallTarget::ModuleProvenance(provenance));
        }
        if targets.has_unresolved
            || (targets.targets.is_empty()
                && targets.known_targets.is_empty()
                && module_provenance.is_empty())
        {
            record_match(ObservedCallTarget::Deferred);
        }
        self.calls.push(CallFact {
            caller,
            range: call.range,
            targets: targets.targets,
            no_matches: no_matches.into_boxed_slice(),
            unsupported_function_statement_suppression: self.statement_suppression(statement),
            unsupported_function_caller_suppression: self.function_suppression(caller),
        });
    }
}

impl<'a> SourceOrderVisitor<'a> for FactVisitor<'a, '_> {
    fn visit_stmt(&mut self, statement: &'a ast::Stmt) {
        let enclosing_statement = self.statement.replace(statement.range());
        match statement {
            ast::Stmt::FunctionDef(function) => self.visit_function(function),
            ast::Stmt::ClassDef(class) => {
                let enclosing_caller = self.caller.take();
                self.visit_body(&class.body);
                self.caller = enclosing_caller;
            }
            ast::Stmt::TypeAlias(_) => {}
            _ => walk_stmt(self, statement),
        }
        self.statement = enclosing_statement;
    }

    fn visit_annotation(&mut self, _annotation: &'a ast::Expr) {}

    fn visit_expr(&mut self, expression: &'a ast::Expr) {
        if matches!(
            expression,
            ast::Expr::Lambda(_)
                | ast::Expr::ListComp(_)
                | ast::Expr::SetComp(_)
                | ast::Expr::DictComp(_)
                | ast::Expr::Generator(_)
        ) {
            return;
        }

        if let ast::Expr::Call(call) = expression {
            self.visit_call(call);
        }
        walk_expr(self, expression);
    }
}

fn is_baked_resolver_origin(origin: ModuleOrigin, module: &str, symbol: &str) -> bool {
    if origin != ModuleOrigin::ThirdParty {
        return false;
    }

    match module {
        "chalk.features.resolver" | "chalk.features" => {
            matches!(symbol, "online" | "offline" | "resolver")
        }
        "chalk" => matches!(
            symbol,
            "online" | "offline" | "resolver" | "batch" | "realtime"
        ),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use ruff_db::Db as _;
    use ruff_db::files::system_path_to_file;
    use ruff_db::source::source_text;
    use ruff_db::system::{
        DbWithTestSystem as _, DbWithWritableSystem as _, SystemPath, SystemPathBuf,
    };
    use ruff_python_ast::PythonVersion;
    use ty_module_resolver::SearchPathSettings;
    use ty_project::{ProjectMetadata, TestDb};
    use ty_python_core::platform::PythonPlatform;
    use ty_python_core::program::{FallibleStrategy, Program, ProgramSettings};
    use ty_python_semantic::{PythonVersionSource, PythonVersionWithSource};

    use crate::CallNoMatchReason;

    use super::{extract_file_facts, is_baked_resolver_origin};

    fn setup(main: &str, files: &[(&str, &str)]) -> (TestDb, ruff_db::files::File) {
        setup_with_search_paths(
            main,
            files,
            &[
                (
                    "/site-packages/chalk/__init__.py",
                    "from chalk.features.resolver import online as online, resolver as resolver\n\
                     def batch(function): ...\n\
                     def realtime(function): ...\n",
                ),
                (
                    "/site-packages/chalk/features/__init__.py",
                    "from chalk.features.resolver import resolver as resolver\n",
                ),
                (
                    "/site-packages/chalk/features/resolver.py",
                    "def online(function): ...\n\
                     def offline(function): ...\n\
                     def resolver(function): ...\n",
                ),
            ],
            &[],
        )
    }

    fn setup_with_search_paths(
        main: &str,
        source_files: &[(&str, &str)],
        site_package_files: &[(&str, &str)],
        extra_files: &[(&str, &str)],
    ) -> (TestDb, ruff_db::files::File) {
        let project = ProjectMetadata::new("test", SystemPathBuf::from("/"));
        let mut db = TestDb::new(project);

        for root in ["/src", "/site-packages", "/extra"] {
            db.memory_file_system()
                .create_directory_all(SystemPath::new(root))
                .unwrap();
        }
        for (path, source) in source_files
            .iter()
            .chain(site_package_files)
            .chain(extra_files)
        {
            if let Some(parent) = SystemPath::new(path).parent() {
                db.memory_file_system()
                    .create_directory_all(parent)
                    .unwrap();
            }
            db.write_file(SystemPath::new(path), source).unwrap();
        }
        db.write_file(SystemPath::new("/src/main.py"), main)
            .unwrap();

        let search_paths = SearchPathSettings {
            extra_paths: vec![SystemPathBuf::from("/extra")],
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
        (db, file)
    }

    fn text(source: &str, range: ruff_text_size::TextRange) -> &str {
        &source[usize::from(range.start())..usize::from(range.end())]
    }

    #[test]
    fn recognizes_only_baked_resolver_provenance() {
        let source = "\
from chalk.features.resolver import online as direct
from chalk.features.resolver import offline
import chalk.features.resolver as resolver_module
import chalk
from chalk.features import resolver as feature_resolver
from chalk import batch, offline as dynamic_offline
from chalk.features.resolver import batch as wrong_batch
from reexports import feature
from opaque_reexports import online as opaque_reexport
from wrappers import wrapped

condition: bool

def local(function): return function

@direct
def direct_root(): pass

@resolver_module.offline
def module_root(): pass

@feature
def reexported_root(): pass

@feature_resolver
def features_root(): pass

@batch
def chalk_root(): pass

@dynamic_offline
def dynamic_import_root(): pass

@chalk.offline
def dynamic_attribute_root(): pass

@(direct if condition else offline)
def union_root(): pass

@wrong_batch
def wrong_module_symbol(): pass

@local
def local_root(): pass

@wrapped
def opaque_wrapper(): pass

@opaque_reexport
def opaque_module_reexport(): pass

@(direct if condition else local)
def mixed_local(): pass

@(direct if condition else unknown)
def mixed_unresolved(): pass
";
        let (db, file) = setup(
            source,
            &[
                (
                    "/src/reexports.py",
                    "from chalk.features.resolver import online as feature\n",
                ),
                ("/src/opaque_reexports.py", ""),
                (
                    "/src/wrappers.py",
                    "def wrapped(function): return function\n",
                ),
            ],
        );
        let facts = extract_file_facts(&db, file);
        let names = facts
            .resolver_roots()
            .iter()
            .map(|root| root.definition.name(&db).unwrap())
            .collect::<Vec<_>>();

        assert_eq!(
            names,
            [
                "direct_root",
                "module_root",
                "reexported_root",
                "features_root",
                "chalk_root",
                "dynamic_import_root",
                "dynamic_attribute_root",
                "union_root",
            ]
        );
    }

    #[test]
    fn accepts_editable_installed_chalk_resolvers() {
        let (db, file) = setup_with_search_paths(
            "\
from chalk import online

@online
def root(): pass
",
            &[("/editable/chalk/__init__.py", "def online(function): ...\n")],
            &[("/site-packages/chalk.pth", "/editable\n")],
            &[],
        );

        let facts = extract_file_facts(&db, file);
        assert_eq!(
            facts.resolver_roots()[0].definition.name(&db).as_deref(),
            Some("root")
        );
    }

    #[test]
    fn rejects_non_third_party_chalk_resolvers() {
        let source = "\
from chalk import online

@online
def root(): pass
";

        for (label, source_files, site_package_files, extra_files) in [
            (
                "first party",
                &[("/src/chalk/__init__.py", "def online(function): ...\n")][..],
                &[][..],
                &[][..],
            ),
            (
                "extra",
                &[][..],
                &[][..],
                &[("/extra/chalk/__init__.py", "def online(function): ...\n")][..],
            ),
            (
                "namespace",
                &[][..],
                &[("/site-packages/chalk/member.py", "")][..],
                &[][..],
            ),
        ] {
            let (db, file) =
                setup_with_search_paths(source, source_files, site_package_files, extra_files);
            assert!(
                extract_file_facts(&db, file).resolver_roots().is_empty(),
                "{label}"
            );
        }

        for origin in [
            ty_python_semantic::chalk::ModuleOrigin::StandardLibrary,
            ty_python_semantic::chalk::ModuleOrigin::FirstParty,
            ty_python_semantic::chalk::ModuleOrigin::Extra,
            ty_python_semantic::chalk::ModuleOrigin::Namespace,
            ty_python_semantic::chalk::ModuleOrigin::Unresolved,
            ty_python_semantic::chalk::ModuleOrigin::Other,
        ] {
            assert!(!is_baked_resolver_origin(origin, "chalk", "online"));
        }
    }

    #[test]
    fn accepts_vendored_stub_only_chalk_resolvers() {
        let source = "\
from chalk import online

@online
def root(): pass
";
        let (db, file) = setup_with_search_paths(source, &[], &[], &[]);
        let facts = extract_file_facts(&db, file);

        assert_eq!(
            facts.resolver_roots()[0].definition.name(&db).as_deref(),
            Some("root")
        );
    }

    #[test]
    fn attributes_calls_to_exact_named_functions_and_excludes_deferred_contexts() {
        let source = "\
def target(): pass

target()

def outer(value: target() = target()):
    target()
    deferred = lambda: target()
    values = [target() for item in value if target()]
    type Deferred[T: target()] = target()

    async def nested():
        target()

    def generator():
        yield target()

    class Local(target(), metaclass=target()):
        target()

        def method(self):
            target()

@target()
def decorated():
    target()
";
        let (db, file) = setup(source, &[]);
        let facts = extract_file_facts(&db, file);
        let calls = facts
            .calls()
            .iter()
            .map(|call| call.caller.name(&db).unwrap())
            .collect::<Vec<_>>();

        assert_eq!(
            calls,
            ["outer", "nested", "generator", "method", "decorated"]
        );
    }

    #[test]
    fn preserves_target_alternatives_and_unresolved_calls() {
        let source = "\
from targets import imported as alias
import targets

def first(): pass
def second(): pass

class Methods:
    @staticmethod
    def static(): pass

    @classmethod
    def class_(cls): pass

    def bound(self): pass

class Conditional:
    if flag:
        def possible(self): pass

def caller(condition, unknown, instance: Methods, conditional: Conditional):
    selected = first if condition else second
    selected()
    unknown()
    alias()
    targets.imported()
    Methods.static()
    Methods.class_()
    instance.bound()
    conditional.possible()
    Methods()
    first(*())
    first(**{})
    len(())
    \"literal\".startswith(\"lit\")
";
        let (db, file) = setup(source, &[("/src/targets.py", "def imported(): pass\n")]);
        let facts = extract_file_facts(&db, file);
        let source = source_text(&db, file);
        let calls = facts.calls();
        let target_names = calls[0]
            .targets
            .iter()
            .map(|target| target.definition.name(&db).unwrap())
            .collect::<Vec<_>>();

        assert_eq!(target_names, ["first", "second"]);
        assert_eq!(calls[0].no_matches.len(), 2);
        assert!(
            calls[0]
                .no_matches
                .iter()
                .all(|(_, reason)| *reason == CallNoMatchReason::MissingRegistryEntry)
        );
        assert!(calls[1].no_matches.is_empty());
        assert!(calls[1].targets.is_empty());
        assert_eq!(
            calls[2..4]
                .iter()
                .map(|call| call.targets[0].definition.name(&db).unwrap())
                .collect::<Vec<_>>(),
            ["imported", "imported"]
        );
        assert_eq!(
            calls[4..7]
                .iter()
                .map(|call| call.targets[0].definition.name(&db).unwrap())
                .collect::<Vec<_>>(),
            ["static", "class_", "bound"]
        );
        assert_eq!(
            calls[7].targets[0].definition.name(&db).as_deref(),
            Some("possible")
        );
        assert_eq!(
            calls[7].no_matches[0].1,
            CallNoMatchReason::MissingRegistryEntry
        );
        assert_eq!(
            calls[8].targets[0].kind,
            ty_python_semantic::chalk::CallTargetKind::ClassConstructor
        );
        assert!(calls[8..13].iter().all(|call| call.no_matches.is_empty()));
        assert_eq!(
            calls[9..11]
                .iter()
                .map(|call| call.targets[0].definition.name(&db).unwrap())
                .collect::<Vec<_>>(),
            ["first", "first"]
        );
        assert_eq!(text(&source, calls[11].range), "len(())");
        assert!(calls[12].targets.is_empty());
    }

    #[test]
    fn source_definition_fallback_keeps_unresolved_and_rejects_assignment_aliases() {
        let source = "\
from opaque import decorate

@decorate
def direct(): pass

class Local:
    @decorate
    def bound(self): pass

@decorate
class Constructed: pass

def outer(local: Local, dynamic):
    @decorate
    def nested(): pass

    direct()
    local.bound()
    nested()
    Constructed()
    alias = dynamic
    alias()
";
        let (db, file) = setup(
            source,
            &[("/src/opaque.py", "def decorate(value): return value\n")],
        );
        let facts = extract_file_facts(&db, file);
        let calls = facts.calls();

        assert_eq!(calls.len(), 5);
        assert_eq!(
            calls[..3]
                .iter()
                .map(|call| call.targets[0].definition.name(&db).unwrap())
                .collect::<Vec<_>>(),
            ["direct", "bound", "nested"]
        );
        assert_eq!(
            calls[3].targets[0].kind,
            ty_python_semantic::chalk::CallTargetKind::ClassConstructor
        );
        assert!(calls[..2].iter().all(|call| call.no_matches.len() == 1
            && call.no_matches[0].1 == CallNoMatchReason::MissingRegistryEntry));
        assert!(calls[2..].iter().all(|call| call.no_matches.is_empty()));
        assert!(calls[4].targets.is_empty());
    }

    #[test]
    fn links_exact_statement_and_caller_suppressions_without_leaking_to_nested_functions() {
        let source = "\
from chalk import online

# chalk: ignore[unsupported-function]
@online
def root():
    unsupported()  # chalk: ignore[unsupported-function]

    def nested():
        unsupported()

top_level = 1  # chalk: ignore[future-code]
";
        let (db, file) = setup(source, &[]);
        let facts = extract_file_facts(&db, file);
        let source = source_text(&db, file);
        let root = &facts.resolver_roots()[0];
        let root_call = &facts.calls()[0];
        let nested_call = &facts.calls()[1];

        assert_eq!(root.definition.name(&db).as_deref(), Some("root"));
        assert_eq!(
            text(&source, root.unsupported_function_suppression.unwrap()),
            "# chalk: ignore[unsupported-function]"
        );
        assert_eq!(text(&source, root_call.range), "unsupported()");
        assert_eq!(
            text(
                &source,
                root_call
                    .unsupported_function_statement_suppression
                    .unwrap()
            ),
            "# chalk: ignore[unsupported-function]"
        );
        assert!(root_call.unsupported_function_caller_suppression.is_some());
        assert!(
            nested_call
                .unsupported_function_statement_suppression
                .is_none()
        );
        assert!(
            nested_call
                .unsupported_function_caller_suppression
                .is_none()
        );
        assert_eq!(facts.suppression_problems().len(), 1);
        assert_eq!(
            text(&source, facts.suppression_problems()[0].range),
            "future-code"
        );
    }
}
