use ruff_db::Db as _;
use ruff_db::files::system_path_to_file;
use ruff_db::source::source_text;
use ruff_db::system::{
    DbWithTestSystem as _, DbWithWritableSystem as _, SystemPath, SystemPathBuf,
};
use ruff_python_ast::PythonVersion;
use ty_chalk::{
    ActiveChalkProject, CallNoMatchReason, ChalkProjectInput, chalk_diagnostics_for_file,
    discover_chalk_project,
};
use ty_module_resolver::SearchPathSettings;
use ty_project::{ProjectMetadata, TestDb};
use ty_python_core::platform::PythonPlatform;
use ty_python_core::program::{FallibleStrategy, Program, ProgramSettings};
use ty_python_semantic::{PythonVersionSource, PythonVersionWithSource};

fn fixture_db(files: &[(&str, &str)]) -> TestDb {
    let project_root = SystemPath::new("/project");
    let config_path = SystemPath::new("/project/chalk.yml");
    let chalk_path = SystemPath::new("/site-packages/chalk/__init__.py");
    let mut db = TestDb::new(ProjectMetadata::new("accel", project_root.to_path_buf()));

    for directory in [project_root, SystemPath::new("/site-packages/chalk")] {
        db.memory_file_system()
            .create_directory_all(directory)
            .unwrap();
    }
    db.write_file(config_path, include_str!("fixtures/accel/chalk.yml"))
        .unwrap();
    for (name, source) in files {
        db.write_file(project_root.join(name), source).unwrap();
    }
    db.write_file(chalk_path, "def online(function): ...\n")
        .unwrap();

    let search_paths = SearchPathSettings {
        extra_paths: Vec::new(),
        src_roots: vec![project_root.to_path_buf()],
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

    db
}

#[test]
fn accel_behavioral_matrix() {
    let db = fixture_db(&[("features.py", include_str!("fixtures/accel/features.py"))]);
    let source_path = SystemPath::new("/project/features.py");
    let file = system_path_to_file(&db, source_path).unwrap();
    let project = discover_chalk_project(db.system(), source_path).unwrap();
    let active_project = ActiveChalkProject::new(&db, project).unwrap();
    let diagnostics = chalk_diagnostics_for_file(&db, active_project.input(), file);
    let source = source_text(&db, file);
    let calls = diagnostics
        .iter()
        .map(|diagnostic| source[diagnostic.range].to_string())
        .collect::<Vec<_>>();

    assert_eq!(
        calls,
        [
            "math.gcd(4, 2)",
            "json.load(1)",
            "re.finditer(\"x\", \"xyz\")",
            "urlparse(\"https://chalk.ai\")",
            "mapping.values()",
            "counts.values()",
            "os.environ.get(1)",
            "math.sqrt(\"x\")",
            "json.loads(1)",
            "re.search(1, \"abc\")",
            "timedelta(days=3)",
            "datetime.timedelta(days=3)",
        ]
    );

    for (diagnostic, (reason, has_suggestions)) in diagnostics.iter().zip([
        (CallNoMatchReason::MissingRegistryEntry, false),
        (CallNoMatchReason::MissingRegistryEntry, false),
        (CallNoMatchReason::MissingRegistryEntry, false),
        (CallNoMatchReason::MissingRegistryEntry, false),
        (CallNoMatchReason::MissingRegistryEntry, false),
        (CallNoMatchReason::MissingRegistryEntry, false),
        (CallNoMatchReason::SignatureMismatch, true),
        (CallNoMatchReason::SignatureMismatch, true),
        (CallNoMatchReason::SignatureMismatch, true),
        (CallNoMatchReason::SignatureMismatch, true),
        (CallNoMatchReason::MissingRegistryEntry, false),
        (CallNoMatchReason::MissingRegistryEntry, false),
    ]) {
        let details = diagnostic
            .unsupported_function_details()
            .expect("the fixture should emit only unsupported-function diagnostics");
        assert_eq!(details.targets.len(), 1);
        assert_eq!(details.targets[0].reason, reason);
        assert_eq!(!details.supported_signatures.is_empty(), has_suggestions);
    }
}

const DEFAULT_HELPERS: &str = include_str!("fixtures/alignment/defaults_helpers.py");
const DEFAULT_RESOLVERS: &str = include_str!("fixtures/alignment/defaults_resolvers.py");

fn alignment_input(db: &TestDb) -> ChalkProjectInput {
    let path = SystemPath::new("/project/features.py");
    let project = discover_chalk_project(db.system(), path).unwrap();
    let active = ActiveChalkProject::new(db, project).unwrap();
    active.input()
}

fn assert_helper_default_warnings(
    db: &TestDb,
    input: ChalkProjectInput,
    expected: &[(&str, &str)],
) {
    let mut actual = Vec::new();
    for file in input.python_files(db) {
        let source = source_text(db, file);
        for diagnostic in chalk_diagnostics_for_file(db, input, file) {
            assert_eq!(
                diagnostic.kind,
                ty_chalk::ChalkDiagnosticKind::HelperParameterDefault
            );
            assert_eq!(diagnostic.code(), "unsupported-function");
            assert_eq!(
                diagnostic.severity(),
                ty_chalk::ChalkDiagnosticSeverity::Warning
            );
            assert_eq!(
                diagnostic.message(),
                "Helper parameter defaults must be scalar literals for the static accelerator"
            );
            actual.push((
                file.path(db)
                    .as_system_path()
                    .unwrap()
                    .file_name()
                    .unwrap()
                    .to_string(),
                source[diagnostic.range].to_string(),
            ));
        }
    }
    assert_eq!(
        actual,
        expected
            .iter()
            .map(|(file, expression)| (file.to_string(), expression.to_string()))
            .collect::<Vec<_>>()
    );
}

#[test]
fn alignment_named_defaults_cross_file() {
    let db = fixture_db(&[
        ("defaults_helpers.py", DEFAULT_HELPERS),
        ("features.py", DEFAULT_RESOLVERS),
    ]);
    assert_helper_default_warnings(
        &db,
        alignment_input(&db),
        &[
            ("defaults_helpers.py", "BUDGET"),
            ("defaults_helpers.py", "BUDGET"),
            ("defaults_helpers.py", "BUDGET"),
            ("defaults_helpers.py", "2 * 4"),
        ],
    );
}

#[test]
fn alignment_named_defaults_same_file() {
    let (_, resolvers) = DEFAULT_RESOLVERS
        .split_once("\n)\n")
        .expect("the fixture starts with a multiline helper import");
    let source = format!("{DEFAULT_HELPERS}\nfrom chalk import online\n{resolvers}");
    let db = fixture_db(&[("features.py", &source)]);
    assert_helper_default_warnings(
        &db,
        alignment_input(&db),
        &[
            ("features.py", "BUDGET"),
            ("features.py", "BUDGET"),
            ("features.py", "BUDGET"),
            ("features.py", "2 * 4"),
        ],
    );
}

#[test]
fn alignment_helper_default_edits_update_warnings() {
    let resolver = "from chalk import online\nfrom defaults_helpers import named_budget\n@online\ndef resolver(text: str) -> int:\n    return named_budget(text)\n";
    let mut db = fixture_db(&[
        ("features.py", resolver),
        ("defaults_helpers.py", DEFAULT_HELPERS),
    ]);
    let input = alignment_input(&db);
    assert_helper_default_warnings(&db, input, &[("defaults_helpers.py", "BUDGET")]);
    db.write_file(
        "/project/defaults_helpers.py",
        DEFAULT_HELPERS.replace("budget: int = BUDGET", "budget: int = 8"),
    )
    .unwrap();
    assert_helper_default_warnings(&db, input, &[]);
    db.write_file("/project/defaults_helpers.py", DEFAULT_HELPERS)
        .unwrap();
    assert_helper_default_warnings(&db, input, &[("defaults_helpers.py", "BUDGET")]);
    db.write_file(
        "/project/features.py",
        resolver.replace("return named_budget(text)", "return len(text)"),
    )
    .unwrap();
    assert_helper_default_warnings(&db, input, &[]);
}

#[test]
fn alignment_outside_resolvers_and_call_site_suppression() {
    for source in [DEFAULT_RESOLVERS] {
        let source = source.replace("@online\n", "");
        let db = fixture_db(&[
            ("features.py", &source),
            ("defaults_helpers.py", DEFAULT_HELPERS),
        ]);
        assert_helper_default_warnings(&db, alignment_input(&db), &[]);
    }
    let resolver = "from chalk import online\nfrom defaults_helpers import named_budget\n@online\ndef resolver(text: str) -> int:\n    return named_budget(text)  # chalk: ignore[unsupported-function]\n";
    let db = fixture_db(&[
        ("features.py", resolver),
        ("defaults_helpers.py", DEFAULT_HELPERS),
    ]);
    // Call-site suppression does not suppress diagnostics inside a reachable helper.
    assert_helper_default_warnings(
        &db,
        alignment_input(&db),
        &[("defaults_helpers.py", "BUDGET")],
    );
}

fn assert_resolver_default_warnings(db: &TestDb, input: ChalkProjectInput, expected: &[&str]) {
    let file = system_path_to_file(db, "/project/features.py").unwrap();
    let source = source_text(db, file);
    let diagnostics = chalk_diagnostics_for_file(db, input, file);
    let actual = diagnostics
        .iter()
        .map(|diagnostic| {
            assert_eq!(diagnostic.file, file);
            assert_eq!(
                diagnostic.kind,
                ty_chalk::ChalkDiagnosticKind::ResolverParameterDefault
            );
            assert_eq!(
                diagnostic.severity(),
                ty_chalk::ChalkDiagnosticSeverity::Warning
            );
            assert_eq!(diagnostic.code(), "unsupported-function");
            assert_eq!(
                diagnostic.message(),
                "Resolver parameter defaults are not supported by the static accelerator"
            );
            assert!(diagnostic.unsupported_function_details().is_none());
            source[diagnostic.range].to_string()
        })
        .collect::<Vec<_>>();
    assert_eq!(actual, expected);
}

#[test]
fn resolver_defaults_warn_for_literals_names_and_expressions() {
    for default in [
        "8", "-8", "+8", "1.5", "\"text\"", "True", "None", "BUDGET", "2 * 4", "[]",
    ] {
        let source = format!(
            "from chalk import online\nBUDGET = 8\n@online\ndef resolver(value={default}):\n    return value\n"
        );
        let db = fixture_db(&[("features.py", &source)]);
        assert_resolver_default_warnings(&db, alignment_input(&db), &[default]);
    }
}

#[test]
fn resolver_required_parameters_do_not_warn() {
    for parameters in [
        "",
        "value",
        "value: int",
        "value: int | None",
        "text: str, budget: int",
        "values: list[int], mapping: dict[str, int]",
        "default: int",
    ] {
        for prefix in ["def", "async def"] {
            let source = format!(
                "from chalk import online\nBUDGET = 8\n@online\n{prefix} required({parameters}) -> int:\n    return 1\n@online\n{prefix} defaulted(budget: int = BUDGET) -> int:\n    return budget\n"
            );
            let db = fixture_db(&[("features.py", &source)]);
            // The neighboring default must still warn, so this cannot pass merely
            // because resolver discovery stopped working. Nullable is not defaulted.
            assert_resolver_default_warnings(&db, alignment_input(&db), &["BUDGET"]);
        }
    }
}

#[test]
fn resolver_required_arguments_and_keyword_calls_do_not_warn() {
    let source = r"
from chalk import online

BUDGET = 8

def helper(text: str, budget: int) -> int:
    return min(len(text), budget)

@online
def positional_call(text: str, budget: int) -> int:
    return helper(text, budget)

@online
def keyword_call(text: str, budget: int) -> int:
    return helper(text=text, budget=budget)

@online
def named_constant_in_call(text: str) -> int:
    return helper(text, budget=BUDGET)

@online
def mixed_parameters(text: str, budget: int = BUDGET) -> int:
    return helper(text, budget)
";
    let db = fixture_db(&[("features.py", source)]);
    assert_resolver_default_warnings(&db, alignment_input(&db), &["BUDGET"]);
}

#[test]
fn resolver_defaults_cover_parameter_kinds_and_async_roots() {
    let source = r"
from chalk import online as resolver

@resolver
async def root(positional=1, /, ordinary=2, *, keyword=3):
    return ordinary

# Ordinary helpers are not subject to the resolver-entry restriction.
def helper(value=4):
    return value

@resolver
def required(value):
    return helper(value)

# A local decorator with the same spelling is not a Chalk resolver.
def online(function):
    return function

@online
def ordinary_function(value=5):
    return value
";
    let db = fixture_db(&[("features.py", source)]);
    assert_resolver_default_warnings(&db, alignment_input(&db), &["1", "2", "3"]);
}

#[test]
fn resolver_default_suppressions_do_not_hide_body_warnings() {
    let source = r"
from chalk import online
import math

# chalk: ignore[unsupported-function]
@online
def function_suppressed(value=1):
    return math.gcd(value, 2)

@online  # chalk: ignore[unsupported-function]
def statement_suppressed(value=3):
    return math.gcd(value, 4)

@online
def body_suppressed(value=5):
    return math.gcd(value, 6)  # chalk: ignore[unsupported-function]
";
    let db = fixture_db(&[("features.py", source)]);
    let file = system_path_to_file(&db, "/project/features.py").unwrap();
    let diagnostics = chalk_diagnostics_for_file(&db, alignment_input(&db), file);
    assert_eq!(diagnostics.len(), 2);
    assert_eq!(&source[diagnostics[0].range], "math.gcd(value, 4)");
    assert_eq!(
        diagnostics[0]
            .unsupported_function_details()
            .unwrap()
            .targets[0]
            .reason,
        CallNoMatchReason::MissingRegistryEntry
    );
    assert_eq!(&source[diagnostics[1].range], "5");
    assert_eq!(
        diagnostics[1].kind,
        ty_chalk::ChalkDiagnosticKind::ResolverParameterDefault
    );
}

#[test]
fn resolver_default_warning_updates_with_signature_and_decorator_edits() {
    let source = "from chalk import online\nBUDGET = 8\n@online\ndef resolver(value=BUDGET):\n    return value\n";
    let mut db = fixture_db(&[("features.py", source)]);
    let input = alignment_input(&db);
    assert_resolver_default_warnings(&db, input, &["BUDGET"]);
    db.write_file(
        "/project/features.py",
        source.replace("value=BUDGET", "value=8"),
    )
    .unwrap();
    assert_resolver_default_warnings(&db, input, &["8"]);
    db.write_file(
        "/project/features.py",
        source.replace("value=BUDGET", "value"),
    )
    .unwrap();
    assert_resolver_default_warnings(&db, input, &[]);
    db.write_file("/project/features.py", source).unwrap();
    assert_resolver_default_warnings(&db, input, &["BUDGET"]);
    db.write_file("/project/features.py", source.replace("@online\n", ""))
        .unwrap();
    assert_resolver_default_warnings(&db, input, &[]);
}

#[test]
fn helper_default_literal_matrix() {
    let mut accepted = [
        "0", "8", "0xff", "0b101", "1_000", "1.5", "0.0", "1e3", "''", "'text'", "'a' 'b'", "True",
        "False", "None", "(8)", "((None))",
    ]
    .map(str::to_string)
    .to_vec();
    let mut rejected = [
        "BUDGET",
        "2 * 4",
        "make()",
        "[]",
        "{}",
        "()",
        "{1}",
        "b'text'",
        "1j",
        "...",
        "f'text'",
        "f'{BUDGET}'",
        "BUDGET.real",
        "[1][0]",
        "lambda: 8",
        "8 if True else 4",
        "8 or 4",
        "8 == 4",
        "(value := 8)",
        "[x for x in []]",
        "{x for x in []}",
        "{x: x for x in []}",
        "(x for x in [])",
    ]
    .map(str::to_string)
    .to_vec();
    // Exercise every unary operator against numeric literals, other constants,
    // names, calls, containers, and expressions. Only direct +/- numbers pass.
    for operator in ["+", "-", "~", "not "] {
        for operand in [
            "0", "8", "1.5", "0.0", "True", "False", "None", "'text'", "b'text'", "1j", "...",
            "BUDGET", "make()", "[]", "(2 * 4)",
        ] {
            let expression = format!("{operator}{operand}");
            if matches!(operator, "+" | "-") && matches!(operand, "0" | "8" | "1.5" | "0.0") {
                accepted.push(expression);
            } else {
                rejected.push(expression);
            }
        }
    }
    for outer in ["+", "-"] {
        for inner in ["+", "-"] {
            for number in ["8", "1.5"] {
                rejected.push(format!("{outer}{inner}{number}"));
            }
        }
    }
    // Parentheses do not introduce AST operators, so these remain direct literals.
    accepted.extend(["+(8)", "-((1.5))"].map(str::to_string));
    for (defaults, warns) in [(&accepted[..], false), (&rejected[..], true)] {
        for default in defaults {
            for (parameters, call) in [
                (format!("value={default}"), "helper()"),
                (format!("value={default}"), "helper(3)"),
                (format!("value={default}"), "helper(value=3)"),
                (format!("value={default}, /"), "helper()"),
                (format!("value={default}, /"), "helper(3)"),
                (format!("*, value={default}"), "helper()"),
                (format!("*, value={default}"), "helper(value=3)"),
            ] {
                let helpers = format!(
                    "BUDGET = 8\ndef make():\n    return 8\ndef helper({parameters}):\n    return 1\n"
                );
                let resolver = format!(
                    "from chalk import online\nfrom helpers import helper\n@online\ndef resolver():\n    return {call}\n"
                );
                let db = fixture_db(&[("features.py", &resolver), ("helpers.py", &helpers)]);
                let expected = if warns {
                    vec![(
                        "helpers.py",
                        match default.as_str() {
                            "(value := 8)" => "value := 8",
                            expression => expression,
                        },
                    )]
                } else {
                    vec![]
                };
                assert_helper_default_warnings(&db, alignment_input(&db), &expected);
            }
        }
    }
}

#[test]
fn helper_default_suppression_preserves_traversal_and_updates() {
    let helpers = "BUDGET = 8\ndef outer(value=BUDGET):\n    return inner()\ndef inner(value=2 * 4):\n    return 1\n";
    let resolver = "from chalk import online\nfrom helpers import outer\n@online\ndef resolver():\n    return outer()\n";
    let mut db = fixture_db(&[("features.py", resolver), ("helpers.py", helpers)]);
    let input = alignment_input(&db);
    assert_helper_default_warnings(
        &db,
        input,
        &[("helpers.py", "BUDGET"), ("helpers.py", "2 * 4")],
    );
    for suppressed in [
        helpers.replace(
            "def outer",
            "# chalk: ignore[unsupported-function]\ndef outer",
        ),
        helpers.replace(
            "def outer(value=BUDGET):",
            "def outer(value=BUDGET):  # chalk: ignore[unsupported-function]",
        ),
    ] {
        db.write_file("/project/helpers.py", suppressed).unwrap();
        assert_helper_default_warnings(&db, input, &[("helpers.py", "2 * 4")]);
    }
    db.write_file("/project/helpers.py", helpers).unwrap();
    assert_helper_default_warnings(
        &db,
        input,
        &[("helpers.py", "BUDGET"), ("helpers.py", "2 * 4")],
    );
    db.write_file(
        "/project/features.py",
        resolver.replace("@online", "# chalk: ignore[unsupported-function]\n@online"),
    )
    .unwrap();
    assert_helper_default_warnings(&db, input, &[]);
}

#[test]
fn helper_defaults_deduplicate_shared_paths() {
    let source = r"
from chalk import online
BUDGET = 8

def helper(value=BUDGET, *, other=2 * 4):
    return 1

def intermediate():
    return helper()

@online
def first():
    alias = helper
    return alias() + helper(value=3, other=4)

@online
def second():
    return intermediate()

def unused(value=make()):
    return value
";
    let db = fixture_db(&[("features.py", source)]);
    assert_helper_default_warnings(
        &db,
        alignment_input(&db),
        &[("features.py", "BUDGET"), ("features.py", "2 * 4")],
    );
}

#[test]
fn helper_defaults_in_nested_functions_and_cycles() {
    let source = r"
from chalk import online
BUDGET = 8

@online
def resolver():
    def nested(value=BUDGET):
        return nested(value)
    return nested()
";
    let db = fixture_db(&[("features.py", source)]);
    let file = system_path_to_file(&db, "/project/features.py").unwrap();
    let diagnostics = chalk_diagnostics_for_file(&db, alignment_input(&db), file);
    assert_eq!(diagnostics.len(), 2);
    assert_eq!(
        diagnostics[0].kind,
        ty_chalk::ChalkDiagnosticKind::HelperParameterDefault
    );
    assert_eq!(&source[diagnostics[0].range], "BUDGET");
    assert_eq!(
        diagnostics[1].kind,
        ty_chalk::ChalkDiagnosticKind::ResolverCycle
    );
    assert_eq!(&source[diagnostics[1].range], "nested(value)");
}

#[test]
fn helper_multiline_default_ranges_and_suppression() {
    let helpers = "BUDGET = 8\ndef helper(\n    required,\n    accepted=-1.5,\n    named=BUDGET,\n    *,\n    expression=(\n        2 *\n        4\n    ),\n):\n    return required\n";
    let resolver = "from chalk import online\nfrom helpers import helper\n@online\ndef resolver():\n    return helper(1)\n";
    let mut db = fixture_db(&[("features.py", resolver), ("helpers.py", helpers)]);
    let input = alignment_input(&db);
    let expected = [("helpers.py", "BUDGET"), ("helpers.py", "2 *\n        4")];
    assert_helper_default_warnings(&db, input, &expected);
    for suppressed in [
        helpers.replace(
            "def helper(",
            "# chalk: ignore[unsupported-function]\ndef helper(",
        ),
        helpers.replace(
            "def helper(",
            "def helper(  # chalk: ignore[unsupported-function]",
        ),
    ] {
        db.write_file("/project/helpers.py", suppressed).unwrap();
        assert_helper_default_warnings(&db, input, &[]);
    }
    // Interior signature lines do not anchor statement-level suppression.
    for unsuppressed in [
        helpers.replace(
            "named=BUDGET,",
            "named=BUDGET,  # chalk: ignore[unsupported-function]",
        ),
        helpers.replace("):\n", "):  # chalk: ignore[unsupported-function]\n"),
    ] {
        db.write_file("/project/helpers.py", unsuppressed).unwrap();
        assert_helper_default_warnings(&db, input, &expected);
    }
    db.write_file("/project/helpers.py", helpers).unwrap();
    assert_helper_default_warnings(&db, input, &expected);
}
