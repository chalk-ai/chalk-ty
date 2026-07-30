use ruff_db::Db as _;
use ruff_db::files::system_path_to_file;
use ruff_db::source::source_text;
use ruff_db::system::{
    DbWithTestSystem as _, DbWithWritableSystem as _, SystemPath, SystemPathBuf,
};
use ruff_python_ast::PythonVersion;
use ty_chalk::{
    ActiveChalkProject, CallNoMatchReason, chalk_diagnostics_for_file, discover_chalk_project,
};
use ty_module_resolver::SearchPathSettings;
use ty_project::{ProjectMetadata, TestDb};
use ty_python_core::platform::PythonPlatform;
use ty_python_core::program::{FallibleStrategy, Program, ProgramSettings};
use ty_python_semantic::{PythonVersionSource, PythonVersionWithSource};

#[test]
fn accel_behavioral_matrix() {
    let project_root = SystemPath::new("/project");
    let source_path = SystemPath::new("/project/features.py");
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
    db.write_file(source_path, include_str!("fixtures/accel/features.py"))
        .unwrap();
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
            "math.sqrt(\"x\")",
            "json.loads(1)",
            "re.search(1, \"abc\")",
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
    ]) {
        let details = diagnostic
            .unsupported_function_details()
            .expect("the fixture should emit only unsupported-function diagnostics");
        assert_eq!(details.targets.len(), 1);
        assert_eq!(details.targets[0].reason, reason);
        assert_eq!(!details.supported_signatures.is_empty(), has_suggestions);
    }
}
