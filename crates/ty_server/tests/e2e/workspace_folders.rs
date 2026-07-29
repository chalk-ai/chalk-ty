use anyhow::Result;
use insta::assert_snapshot;
use lsp_types::{
    DiagnosticSeverity, DocumentDiagnosticReport, FullDocumentDiagnosticReport, Message, Position,
    WorkspaceDiagnosticReport, WorkspaceDocumentDiagnosticReport,
};
use ruff_db::system::SystemPath;
use serde_json::{Map, json};
use ty_server::{ClientOptions, DiagnosticMode, GlobalOptions, WorkspaceOptions};

use crate::{
    TestServer, TestServerBuilder,
    pull_diagnostics::{
        assert_workspace_diagnostics_suspends_for_long_polling, send_workspace_diagnostic_request,
        shutdown_and_await_workspace_diagnostic,
    },
};

#[test]
fn document_diagnostics_isolate_sibling_chalk_projects() -> Result<()> {
    let workspace = SystemPath::new("workspace");
    let first_file = workspace.join("first/main.py");
    let second_file = workspace.join("second/main.py");
    let first_source = "\
from chalk import online

@online
def root():
    abs(\"first\")
";
    let updated_first_source = "\
from chalk import online

@online
def root():
    abs(\"third\")
";
    let second_source = "\
from chalk import online

@online
def root():
    round(\"second\")
";
    let mut server = TestServerBuilder::new()?
        .with_workspace(workspace, None)?
        .with_file(workspace.join("ty.toml"), "")?
        .with_file(workspace.join("first/chalk.yaml"), "{}")?
        .with_file(&first_file, first_source)?
        .with_file(workspace.join("second/chalk.yml"), "{}")?
        .with_file(&second_file, second_source)?
        .build()
        .wait_until_workspaces_are_initialized();

    server.open_text_document(&first_file, first_source, 1);
    server.open_text_document(&second_file, second_source, 1);

    let DocumentDiagnosticReport::RelatedFullDocumentDiagnosticReport(first) =
        server.document_diagnostic_request(&first_file, None)
    else {
        panic!("the first sibling must return a full document report");
    };
    let first_id = first
        .full_document_diagnostic_report
        .result_id
        .clone()
        .expect("the first sibling's Chalk diagnostic must have a result ID");
    let first_chalk = first
        .full_document_diagnostic_report
        .items
        .iter()
        .filter(|diagnostic| diagnostic.source.as_deref() == Some("chalk"))
        .collect::<Vec<_>>();
    assert_eq!(first_chalk.len(), 1);
    let Message::String(first_message) = &first_chalk[0].message else {
        panic!("Chalk diagnostics must use string messages");
    };
    assert!(first_message.contains("Target `builtins.abs`"));
    assert!(first_message.contains("Observed call: abs(\"first\")"));
    assert!(!first_message.contains("round"));

    let DocumentDiagnosticReport::RelatedFullDocumentDiagnosticReport(second) =
        server.document_diagnostic_request(&second_file, None)
    else {
        panic!("the second sibling must return a full document report");
    };
    let second_id = second
        .full_document_diagnostic_report
        .result_id
        .clone()
        .expect("the second sibling's Chalk diagnostic must have a result ID");
    let second_chalk = second
        .full_document_diagnostic_report
        .items
        .iter()
        .filter(|diagnostic| diagnostic.source.as_deref() == Some("chalk"))
        .collect::<Vec<_>>();
    assert_eq!(second_chalk.len(), 1);
    let Message::String(second_message) = &second_chalk[0].message else {
        panic!("Chalk diagnostics must use string messages");
    };
    assert!(second_message.contains("Target `builtins.round`"));
    assert!(second_message.contains("Observed call: round(\"second\")"));
    assert!(!second_message.contains("abs"));

    server.change_text_document(
        &first_file,
        vec![
            lsp_types::TextDocumentContentChangeEvent::TextDocumentContentChangeWholeDocument(
                lsp_types::TextDocumentContentChangeWholeDocument {
                    text: updated_first_source.to_owned(),
                },
            ),
        ],
        2,
    );
    let DocumentDiagnosticReport::RelatedFullDocumentDiagnosticReport(updated_first) =
        server.document_diagnostic_request(&first_file, Some(first_id.clone()))
    else {
        panic!("the first sibling's edit must invalidate its document report");
    };
    assert_ne!(
        updated_first
            .full_document_diagnostic_report
            .result_id
            .as_ref(),
        Some(&first_id)
    );
    let updated_first_chalk = updated_first
        .full_document_diagnostic_report
        .items
        .iter()
        .filter(|diagnostic| diagnostic.source.as_deref() == Some("chalk"))
        .collect::<Vec<_>>();
    assert_eq!(updated_first_chalk.len(), 1);
    let Message::String(updated_first_message) = &updated_first_chalk[0].message else {
        panic!("Chalk diagnostics must use string messages");
    };
    assert!(updated_first_message.contains("Observed call: abs(\"third\")"));
    assert!(!updated_first_message.contains("round"));

    assert!(matches!(
        server.document_diagnostic_request(&second_file, Some(second_id)),
        DocumentDiagnosticReport::RelatedUnchangedDocumentDiagnosticReport(_)
    ));

    Ok(())
}

#[test]
fn workspace_diagnostics_partition_nested_chalk_projects() -> Result<()> {
    let workspace = SystemPath::new("workspace");
    let parent_file = workspace.join("parent.py");
    let first_file = workspace.join("first/main.py");
    let second_file = workspace.join("second/main.py");
    let source = "does_not_exist()";
    let mut server = TestServerBuilder::new()?
        .with_initialization_options(
            ClientOptions::default().with_diagnostic_mode(DiagnosticMode::Workspace),
        )
        .with_workspace(workspace, None)?
        .with_file(workspace.join("ty.toml"), "")?
        .with_file(&parent_file, source)?
        .with_file(workspace.join("first/chalk.yaml"), "{}")?
        .with_file(&first_file, source)?
        .with_file(workspace.join("second/chalk.yml"), "{}")?
        .with_file(&second_file, source)?
        .build()
        .wait_until_workspaces_are_initialized();

    // Opening one file in each Chalk project creates the persistent sibling routes. Both lazy
    // databases discover the workspace-level `ty.toml`, so their internal ty roots overlap.
    server.open_text_document(&first_file, source, 1);
    server.open_text_document(&second_file, source, 1);

    let diagnostics =
        condensed_workspace_diagnostic_snapshot(server.workspace_diagnostic_request(None, None));
    for path in [
        "workspace/parent.py",
        "workspace/first/main.py",
        "workspace/second/main.py",
    ] {
        assert_eq!(
            diagnostics.matches(path).count(),
            1,
            "`{path}` should be published by exactly one routed project:\n{diagnostics}"
        );
    }
    assert_eq!(diagnostics.matches("[ERROR]").count(), 3);

    Ok(())
}

#[test]
fn workspace_diagnostics_report_unsuppressible_cycles_in_closed_files() -> Result<()> {
    let workspace = SystemPath::new("workspace");
    let main = workspace.join("project/main.py");
    let helper = workspace.join("project/helper.py");
    let main_source = "\
from chalk import online
from project import helper

@online
def root():
    helper.first()
";
    let helper_source = "\
from project import main

# chalk: ignore[unsupported-function, resolver-cycle, future-code]
def first():
    main.root()
";
    let mut server = TestServerBuilder::new()?
        .with_initialization_options(
            ClientOptions::default().with_diagnostic_mode(DiagnosticMode::Workspace),
        )
        .with_workspace(workspace, None)?
        .with_file(workspace.join("ty.toml"), "")?
        .with_file(workspace.join("project/chalk.yml"), "")?
        .with_file(workspace.join("project/__init__.py"), "")?
        .with_file(&main, main_source)?
        .with_file(&helper, helper_source)?
        .build()
        .wait_until_workspaces_are_initialized();

    // Opening `main.py` discovers the Chalk project; `helper.py`, which closes the cycle, stays
    // closed.
    server.open_text_document(&main, main_source, 1);

    let helper_uri = server.file_uri(&helper);
    let report = server.workspace_diagnostic_request(None, None);
    let helper_report =
        report
            .items
            .iter()
            .find_map(|item| match item {
                WorkspaceDocumentDiagnosticReport::WorkspaceFullDocumentDiagnosticReport(
                    report,
                ) if report.uri == helper_uri => Some(&report.full_document_diagnostic_report),
                _ => None,
            })
            .expect("the closed helper must have a workspace diagnostic report");
    let chalk_diagnostics = helper_report
        .items
        .iter()
        .filter(|diagnostic| diagnostic.source.as_deref() == Some("chalk"))
        .collect::<Vec<_>>();

    assert_eq!(chalk_diagnostics.len(), 3);
    let invalid_suppression = chalk_diagnostics
        .iter()
        .find(|diagnostic| {
            diagnostic.code.as_ref()
                == Some(&lsp_types::Code::String(
                    "invalid-chalk-suppression".to_owned(),
                ))
        })
        .expect("resolver-cycle must be rejected as an unsuppressible diagnostic code");
    assert_eq!(
        invalid_suppression.range,
        lsp_types::Range::new(Position::new(2, 38), Position::new(2, 52))
    );
    assert_eq!(
        invalid_suppression.severity,
        Some(DiagnosticSeverity::Warning)
    );
    assert_eq!(
        invalid_suppression.message,
        Message::String(
            "Invalid Chalk suppression directive: this diagnostic code cannot be suppressed"
                .to_owned()
        )
    );

    let unknown_suppression = chalk_diagnostics
        .iter()
        .find(|diagnostic| {
            diagnostic.code.as_ref()
                == Some(&lsp_types::Code::String(
                    "unknown-chalk-suppression".to_owned(),
                ))
        })
        .expect("future-code must remain an unknown suppression code");
    assert_eq!(
        unknown_suppression.range,
        lsp_types::Range::new(Position::new(2, 54), Position::new(2, 65))
    );
    assert_eq!(
        unknown_suppression.severity,
        Some(DiagnosticSeverity::Warning)
    );
    assert_eq!(
        unknown_suppression.message,
        Message::String("Unknown Chalk suppression code: future-code".to_owned())
    );

    let cycle = chalk_diagnostics
        .iter()
        .find(|diagnostic| {
            diagnostic.code.as_ref() == Some(&lsp_types::Code::String("resolver-cycle".to_owned()))
        })
        .expect("the invalid suppression must not suppress the cycle");
    assert_eq!(
        cycle.range,
        lsp_types::Range::new(Position::new(4, 4), Position::new(4, 15))
    );
    assert_eq!(cycle.severity, Some(DiagnosticSeverity::Error));
    assert_eq!(
        cycle.code,
        Some(lsp_types::Code::String("resolver-cycle".to_owned()))
    );
    assert_eq!(
        cycle.message,
        Message::String("Resolver call graph contains a cycle".to_owned())
    );

    Ok(())
}

#[test]
fn workspace_diagnostics_report_shared_project_diagnostics_once() -> Result<()> {
    let workspace = SystemPath::new("workspace");
    let config = workspace.join("ty.toml");
    let first_file = workspace.join("first/main.py");
    let second_file = workspace.join("second/main.py");
    let source = "";
    let mut server = TestServerBuilder::new()?
        .with_initialization_options(
            ClientOptions::default().with_diagnostic_mode(DiagnosticMode::Workspace),
        )
        .with_workspace(workspace, None)?
        .with_file(
            &config,
            r#"
            [rules]
            not-a-rule = "error"
            "#,
        )?
        .with_file(workspace.join("first/chalk.yaml"), "{}")?
        .with_file(&first_file, source)?
        .with_file(workspace.join("second/chalk.yml"), "{}")?
        .with_file(&second_file, source)?
        .build()
        .wait_until_workspaces_are_initialized();

    server.await_notification::<lsp_types::PublishDiagnosticsNotification>();

    // Both Chalk databases inherit the workspace-level `ty.toml`.
    server.open_text_document(&first_file, source, 1);
    server.open_text_document(&second_file, source, 1);

    let diagnostics =
        condensed_workspace_diagnostic_snapshot(server.workspace_diagnostic_request(None, None));
    assert_eq!(
        diagnostics.matches("workspace/ty.toml").count(),
        1,
        "the shared configuration diagnostic should be published once:\n{diagnostics}"
    );
    assert_eq!(diagnostics.matches("[WARNING]").count(), 1);

    Ok(())
}

#[test]
fn workspace_diagnostics_keep_shared_project_roots_separate_across_workspaces() -> Result<()> {
    let repository = SystemPath::new("repository");
    let first = repository.join("first");
    let second = repository.join("second");
    let first_options = ClientOptions {
        workspace: WorkspaceOptions {
            configuration: Some(
                Map::from_iter([("rules".to_string(), json!({"unresolved-reference": "warn"}))])
                    .into(),
            ),
            ..WorkspaceOptions::default()
        },
        ..ClientOptions::default()
    };
    let second_options = ClientOptions {
        workspace: WorkspaceOptions {
            configuration: Some(
                Map::from_iter([(
                    "rules".to_string(),
                    json!({"unresolved-reference": "error"}),
                )])
                .into(),
            ),
            ..WorkspaceOptions::default()
        },
        ..ClientOptions::default()
    };
    let mut server = TestServerBuilder::new()?
        .with_initialization_options(
            ClientOptions::default().with_diagnostic_mode(DiagnosticMode::Workspace),
        )
        .with_file(
            repository.join("ty.toml"),
            r#"
            [rules]
            not-a-rule = "error"
            "#,
        )?
        .with_file(first.join("main.py"), "")?
        .with_file(second.join("main.py"), "")?
        .with_workspace(&first, Some(first_options))?
        .with_workspace(&second, Some(second_options))?
        .build()
        .wait_until_workspaces_are_initialized();

    server.await_notification::<lsp_types::PublishDiagnosticsNotification>();
    server.await_notification::<lsp_types::PublishDiagnosticsNotification>();

    let diagnostics =
        condensed_workspace_diagnostic_snapshot(server.workspace_diagnostic_request(None, None));
    assert_eq!(
        diagnostics.matches("repository/ty.toml").count(),
        2,
        "each workspace owns its independently configured project diagnostics:\n{diagnostics}"
    );
    assert_eq!(diagnostics.matches("[WARNING]").count(), 2);

    Ok(())
}

/// Test that we can initialize multiple workspace folders.
#[test]
fn initialize_multiple_workspace_folders() -> Result<()> {
    let root1 = SystemPath::new("root1");
    let root2 = SystemPath::new("root2");
    let mut server = TestServerBuilder::new()?
        .with_initialization_options(
            ClientOptions::default().with_diagnostic_mode(DiagnosticMode::Workspace),
        )
        .with_file(root1.join("main.py"), "does_not_exist()")?
        .with_file(root2.join("main.py"), "does_not_exist()")?
        .with_workspace(root1, None)?
        .with_workspace(root2, None)?
        .build()
        .wait_until_workspaces_are_initialized();

    let workspace_diagnostics = server.workspace_diagnostic_request(None, None);
    assert_snapshot!(
        condensed_workspace_diagnostic_snapshot(workspace_diagnostics), @"
    file://<temp_dir>/root1/main.py
    	0:0..0:14[ERROR]: Name `does_not_exist` used when not defined
    file://<temp_dir>/root2/main.py
    	0:0..0:14[ERROR]: Name `does_not_exist` used when not defined
    "
    );

    Ok(())
}

/// Tests that we can add a workspace folder after the server
/// is initialized.
#[test]
fn add_workspace_folder_after_init() -> Result<()> {
    let root1 = SystemPath::new("root1");
    let root2 = SystemPath::new("root2");
    let mut server = TestServerBuilder::new()?
        .with_initialization_options(
            ClientOptions::default().with_diagnostic_mode(DiagnosticMode::Workspace),
        )
        .with_file(root1.join("main.py"), "does_not_exist()")?
        .with_file(root2.join("main.py"), "does_not_exist()")?
        .with_workspace(root1, None)?
        .build()
        .wait_until_workspaces_are_initialized();

    // Take an initial snapshot of diagnostics to confirm that we
    // don't see `root2/main.py`.
    let workspace_diagnostics = server.workspace_diagnostic_request(None, None);
    assert_snapshot!(
        condensed_workspace_diagnostic_snapshot(workspace_diagnostics), @"
    file://<temp_dir>/root1/main.py
    	0:0..0:14[ERROR]: Name `does_not_exist` used when not defined
    "
    );

    server.add_workspace_folder(root2, None)?;
    server.change_workspace_folders([root2], []);
    server = server.wait_until_workspaces_are_initialized();

    let workspace_diagnostics = server.workspace_diagnostic_request(None, None);
    assert_snapshot!(
        condensed_workspace_diagnostic_snapshot(workspace_diagnostics), @"
    file://<temp_dir>/root1/main.py
    	0:0..0:14[ERROR]: Name `does_not_exist` used when not defined
    file://<temp_dir>/root2/main.py
    	0:0..0:14[ERROR]: Name `does_not_exist` used when not defined
    "
    );

    Ok(())
}

/// Tests that we can add multiple workspace folders simultaneously.
#[test]
fn add_multiple_workspace_folders() -> Result<()> {
    let root1 = SystemPath::new("root1");
    let root2 = SystemPath::new("root2");
    let root3 = SystemPath::new("root3");
    let mut server = TestServerBuilder::new()?
        .with_initialization_options(
            ClientOptions::default().with_diagnostic_mode(DiagnosticMode::Workspace),
        )
        .with_file(root1.join("main.py"), "does_not_exist()")?
        .with_file(root2.join("main.py"), "does_not_exist()")?
        .with_file(root3.join("main.py"), "does_not_exist()")?
        .with_workspace(root1, None)?
        .build()
        .wait_until_workspaces_are_initialized();

    // Take an initial snapshot of diagnostics to confirm that we
    // don't see `root2/main.py` or `root3/main.py`.
    let workspace_diagnostics = server.workspace_diagnostic_request(None, None);
    assert_snapshot!(
        condensed_workspace_diagnostic_snapshot(workspace_diagnostics), @"
    file://<temp_dir>/root1/main.py
    	0:0..0:14[ERROR]: Name `does_not_exist` used when not defined
    "
    );

    server.add_workspace_folder(root2, None)?;
    server.add_workspace_folder(root3, None)?;
    server.change_workspace_folders([root2, root3], []);
    server = server.wait_until_workspaces_are_initialized();

    let workspace_diagnostics = server.workspace_diagnostic_request(None, None);
    assert_snapshot!(
        condensed_workspace_diagnostic_snapshot(workspace_diagnostics), @"
    file://<temp_dir>/root1/main.py
    	0:0..0:14[ERROR]: Name `does_not_exist` used when not defined
    file://<temp_dir>/root2/main.py
    	0:0..0:14[ERROR]: Name `does_not_exist` used when not defined
    file://<temp_dir>/root3/main.py
    	0:0..0:14[ERROR]: Name `does_not_exist` used when not defined
    "
    );

    Ok(())
}

/// Tests that we can remove a workspace folder after the server
/// is initialized.
#[test]
fn remove_workspace_folder_after_init() -> Result<()> {
    let root1 = SystemPath::new("root1");
    let root2 = SystemPath::new("root2");
    let mut server = TestServerBuilder::new()?
        .with_initialization_options(
            ClientOptions::default().with_diagnostic_mode(DiagnosticMode::Workspace),
        )
        .with_file(root1.join("main.py"), "does_not_exist()")?
        .with_file(root2.join("main.py"), "does_not_exist()")?
        .with_workspace(root1, None)?
        .with_workspace(root2, None)?
        .build()
        .wait_until_workspaces_are_initialized();

    // Assert that we get diagnostics across both workspaces
    // initially.
    let workspace_diagnostics = server.workspace_diagnostic_request(None, None);
    assert_snapshot!(
        condensed_workspace_diagnostic_snapshot(workspace_diagnostics), @"
    file://<temp_dir>/root1/main.py
    	0:0..0:14[ERROR]: Name `does_not_exist` used when not defined
    file://<temp_dir>/root2/main.py
    	0:0..0:14[ERROR]: Name `does_not_exist` used when not defined
    "
    );

    // Now remove one of the workspaces and assert that the
    // diagnostics are now limited only to `root1`.
    server.change_workspace_folders([], [root2]);
    // We don't need to wait for workspace initialization
    // since we are only removing a workspace. That is, the
    // server is not expected to send a `workspace/configuration`
    // request.

    let workspace_diagnostics = server.workspace_diagnostic_request(None, None);
    assert_snapshot!(
        condensed_workspace_diagnostic_snapshot(workspace_diagnostics), @"
    file://<temp_dir>/root1/main.py
    	0:0..0:14[ERROR]: Name `does_not_exist` used when not defined
    "
    );

    Ok(())
}

/// Tests that we can remove multiple workspace folders simultaneously.
#[test]
fn remove_multiple_workspace_folders() -> Result<()> {
    let root1 = SystemPath::new("root1");
    let root2 = SystemPath::new("root2");
    let root3 = SystemPath::new("root3");
    let mut server = TestServerBuilder::new()?
        .with_initialization_options(
            ClientOptions::default().with_diagnostic_mode(DiagnosticMode::Workspace),
        )
        .with_file(root1.join("main.py"), "does_not_exist()")?
        .with_file(root2.join("main.py"), "does_not_exist()")?
        .with_file(root3.join("main.py"), "does_not_exist()")?
        .with_workspace(root1, None)?
        .with_workspace(root2, None)?
        .with_workspace(root3, None)?
        .build()
        .wait_until_workspaces_are_initialized();

    // Assert that we get diagnostics across all workspaces
    // initially.
    let workspace_diagnostics = server.workspace_diagnostic_request(None, None);
    assert_snapshot!(
        condensed_workspace_diagnostic_snapshot(workspace_diagnostics), @"
    file://<temp_dir>/root1/main.py
    	0:0..0:14[ERROR]: Name `does_not_exist` used when not defined
    file://<temp_dir>/root2/main.py
    	0:0..0:14[ERROR]: Name `does_not_exist` used when not defined
    file://<temp_dir>/root3/main.py
    	0:0..0:14[ERROR]: Name `does_not_exist` used when not defined
    "
    );

    // Now remove two of the workspaces and assert that the
    // diagnostics are now limited only to `root1`.
    server.change_workspace_folders([], [root2, root3]);
    // We don't need to wait for workspace initialization
    // since we are only removing a workspace. That is, the
    // server is not expected to send a `workspace/configuration`
    // request.

    let workspace_diagnostics = server.workspace_diagnostic_request(None, None);
    assert_snapshot!(
        condensed_workspace_diagnostic_snapshot(workspace_diagnostics), @"
    file://<temp_dir>/root1/main.py
    	0:0..0:14[ERROR]: Name `does_not_exist` used when not defined
    "
    );

    Ok(())
}

/// Tests that we can remove a workspace folder even while there
/// is an open document from that folder.
#[test]
fn remove_workspace_folder_with_open_document() -> Result<()> {
    let root1 = SystemPath::new("root1");
    let root2 = SystemPath::new("root2");
    let main1 = root1.join("main.py");
    let main2 = root2.join("main.py");
    let main1_content = "does_not_exist1()";
    let main2_content = "does_not_exist2()";

    let mut server = TestServerBuilder::new()?
        .with_initialization_options(ClientOptions::default())
        .with_file(&main1, main1_content)?
        .with_file(&main2, main1_content)?
        .with_workspace(root1, None)?
        .with_workspace(root2, None)?
        .build()
        .wait_until_workspaces_are_initialized();

    server.open_text_document(&main1, main1_content, 1);
    let document_diagnostics = server.document_diagnostic_request(&main1, None);
    assert_snapshot!(
        condensed_document_diagnostic_snapshot(document_diagnostics),
        @"0:0..0:15[ERROR]: Name `does_not_exist1` used when not defined",
    );

    server.open_text_document(&main2, main2_content, 1);
    let document_diagnostics = server.document_diagnostic_request(&main2, None);
    assert_snapshot!(
        condensed_document_diagnostic_snapshot(document_diagnostics),
        @"0:0..0:15[ERROR]: Name `does_not_exist2` used when not defined",
    );

    server.change_workspace_folders([], [root2]);

    let document_diagnostics = server.document_diagnostic_request(&main1, None);
    assert_snapshot!(
        condensed_document_diagnostic_snapshot(document_diagnostics),
        @"0:0..0:15[ERROR]: Name `does_not_exist1` used when not defined",
    );

    let document_diagnostics = server.document_diagnostic_request(&main2, None);
    assert_snapshot!(
        condensed_document_diagnostic_snapshot(document_diagnostics),
        @"",
    );

    Ok(())
}

/// Tests that we can add and remove workspace folders at the same time.
#[test]
fn add_and_remove_workspace_folders() -> Result<()> {
    let root1 = SystemPath::new("root1");
    let root2 = SystemPath::new("root2");
    let root3 = SystemPath::new("root3");
    let mut server = TestServerBuilder::new()?
        .with_initialization_options(
            ClientOptions::default().with_diagnostic_mode(DiagnosticMode::Workspace),
        )
        .with_file(root1.join("main.py"), "does_not_exist()")?
        .with_file(root2.join("main.py"), "does_not_exist()")?
        .with_file(root3.join("main.py"), "does_not_exist()")?
        .with_workspace(root1, None)?
        .with_workspace(root2, None)?
        .build()
        .wait_until_workspaces_are_initialized();

    // Take an initial snapshot of diagnostics to confirm that we
    // don't see `root3/main.py`.
    let workspace_diagnostics = server.workspace_diagnostic_request(None, None);
    assert_snapshot!(
        condensed_workspace_diagnostic_snapshot(workspace_diagnostics), @"
    file://<temp_dir>/root1/main.py
    	0:0..0:14[ERROR]: Name `does_not_exist` used when not defined
    file://<temp_dir>/root2/main.py
    	0:0..0:14[ERROR]: Name `does_not_exist` used when not defined
    "
    );

    server.add_workspace_folder(root3, None)?;
    server.change_workspace_folders([root3], [root2]);
    // root3 needs to be initialized, so we expect the server
    // to send a `workspace/configuration` request.
    server = server.wait_until_workspaces_are_initialized();

    let workspace_diagnostics = server.workspace_diagnostic_request(None, None);
    assert_snapshot!(
        condensed_workspace_diagnostic_snapshot(workspace_diagnostics), @"
    file://<temp_dir>/root1/main.py
    	0:0..0:14[ERROR]: Name `does_not_exist` used when not defined
    file://<temp_dir>/root3/main.py
    	0:0..0:14[ERROR]: Name `does_not_exist` used when not defined
    "
    );

    Ok(())
}

/// Tests that if we add a workspace folder that has already been
/// added, then it's a no-op and things still work.
#[test]
fn add_existing_workspace_folder_is_no_op() -> Result<()> {
    let root1 = SystemPath::new("root1");
    let mut server = TestServerBuilder::new()?
        .with_initialization_options(
            ClientOptions::default().with_diagnostic_mode(DiagnosticMode::Workspace),
        )
        .with_file(root1.join("main.py"), "does_not_exist()")?
        .with_workspace(root1, None)?
        .build()
        .wait_until_workspaces_are_initialized();

    server.change_workspace_folders([root1], []);
    // Since `root1` is already a workspace folder, the
    // server won't attempt to re-request configuration.
    // Thus, we don't need to wait for that request here.
    // Arguably, this is debatable. Re-adding a workspace
    // folder that already exists is perhaps a signal that
    // we *should* re-request configuration. But this test
    // is merely asserting current behavior and that we are
    // thoughtful about changing it.

    let workspace_diagnostics = server.workspace_diagnostic_request(None, None);
    assert_snapshot!(
        condensed_workspace_diagnostic_snapshot(workspace_diagnostics), @"
    file://<temp_dir>/root1/main.py
    	0:0..0:14[ERROR]: Name `does_not_exist` used when not defined
    "
    );

    Ok(())
}

/// Tests that if we add a workspace folder that has already been
/// added, then it's a no-op and things still work.
#[test]
fn remove_only_workspace() -> Result<()> {
    let root1 = SystemPath::new("root1");
    let mut server = TestServerBuilder::new()?
        .with_initialization_options(
            ClientOptions::default().with_diagnostic_mode(DiagnosticMode::Workspace),
        )
        .with_file(root1.join("main.py"), "does_not_exist()")?
        .with_workspace(root1, None)?
        .build()
        .wait_until_workspaces_are_initialized();

    server.change_workspace_folders([], [root1]);

    let workspace_diagnostics = get_expected_empty_workspace_diagnostics_and_shutdown(server);
    assert_snapshot!(
        condensed_workspace_diagnostic_snapshot(workspace_diagnostics), @""
    );

    Ok(())
}

/// Test that we can have different settings for each workspace folder.
#[test]
fn different_settings() -> Result<()> {
    let root1 = SystemPath::new("root1");
    let root2 = SystemPath::new("root2");
    let main1 = root1.join("main.py");
    let main2 = root2.join("main.py");
    let main_content = "ZQZQZQ = None\nZQZQ";

    let mut server = TestServerBuilder::new()?
        .with_initialization_options(
            ClientOptions::default().with_diagnostic_mode(DiagnosticMode::Workspace),
        )
        .with_file(&main1, main_content)?
        .with_file(&main2, main_content)?
        .with_workspace(root1, None)?
        // We disable language services in the second workspace
        // folder. Below, we assert that completions work in `root1`
        // but not `root2`.
        .with_workspace(
            root2,
            Some(ClientOptions {
                workspace: WorkspaceOptions {
                    disable_language_services: Some(true),
                    ..WorkspaceOptions::default()
                },
                ..ClientOptions::default()
            }),
        )?
        .build()
        .wait_until_workspaces_are_initialized();

    server.open_text_document(&main1, main_content, 1);
    let completions = server.completion_request(&server.file_uri(&main1), Position::new(1, 4));
    insta::assert_json_snapshot!(completions, @r#"
    [
      {
        "label": "ZQZQZQ",
        "kind": 22,
        "detail": "None",
        "documentation": {
          "kind": "plaintext",
          "value": "The type of the None singleton.\n"
        },
        "sortText": "0"
      }
    ]
    "#);

    server.open_text_document(&main2, main_content, 1);
    let completions = server.completion_request(&server.file_uri(&main2), Position::new(1, 4));
    insta::assert_json_snapshot!(completions, @"[]");

    Ok(())
}

/// Test that a file uses the settings from its containing workspace folder, not from a
/// lexicographically later sibling workspace that merely sorts before the file path.
///
/// This exercises the setting lookup through language services: hover stays enabled for
/// `systemtests`, while `external` keeps its own `disable_language_services = true`.
#[test]
fn nested_sibling_workspace_uses_correct_settings() -> Result<()> {
    let systemtests_file = SystemPath::new("systemtests/foo.py");
    let external_file = SystemPath::new("external/Y/foo.py");
    let file_content = "\
def foo() -> str:
    return 42
";

    let mut server = TestServerBuilder::new()?
        .with_workspace(SystemPath::new("."), None)?
        .with_file(systemtests_file, file_content)?
        .with_workspace(
            SystemPath::new("external/Y"),
            Some(ClientOptions::default().with_disable_language_services(true)),
        )?
        .with_file(external_file, file_content)?
        .build()
        .wait_until_workspaces_are_initialized();

    server.open_text_document(systemtests_file, file_content, 1);
    let systemtests_hover = server.hover_request(systemtests_file, Position::new(0, 5));
    assert!(
        systemtests_hover.is_some(),
        "expected hover information for {systemtests_file}, got: {systemtests_hover:?}",
    );

    server.open_text_document(external_file, file_content, 1);
    let external_hover = server.hover_request(external_file, Position::new(0, 5));
    assert!(
        external_hover.is_none(),
        "expected no hover information for {external_file}, got: {external_hover:?}",
    );

    Ok(())
}

/// Test that a document resolves to the correct project in a multi-root workspace, rather than
/// to a lexicographically later sibling workspace that merely sorts before the file path.
#[test]
fn nested_sibling_workspace_uses_correct_project() -> Result<()> {
    let systemtests_file = SystemPath::new("systemtests/included.py");
    let external_file = SystemPath::new("external/Y/only_external.py");
    let file_content = "\
def foo() -> str:
    return a
";

    let mut server = TestServerBuilder::new()?
        .with_workspace(SystemPath::new("."), None)?
        .with_file(
            SystemPath::new("pyproject.toml"),
            r#"
[tool.ty.src]
include = ["systemtests/included.py"]
"#,
        )?
        .with_file(systemtests_file, file_content)?
        .with_workspace(SystemPath::new("external/Y"), None)?
        .with_file(
            SystemPath::new("external/Y/pyproject.toml"),
            r#"
[tool.ty.src]
include = ["only_external.py"]
"#,
        )?
        .with_file(external_file, file_content)?
        .build()
        .wait_until_workspaces_are_initialized();

    server.open_text_document(systemtests_file, file_content, 1);
    let systemtests_diagnostics = server.document_diagnostic_request(systemtests_file, None);
    assert_snapshot!(
        condensed_document_diagnostic_snapshot(systemtests_diagnostics),
        @"1:11..1:12[ERROR]: Name `a` used when not defined",
    );

    server.open_text_document(external_file, file_content, 1);
    let external_diagnostics = server.document_diagnostic_request(external_file, None);
    assert_snapshot!(
        condensed_document_diagnostic_snapshot(external_diagnostics),
        @"1:11..1:12[ERROR]: Name `a` used when not defined",
    );

    Ok(())
}

/// Test that workspace folders cannot realistically have different
/// global settings.
///
/// Note that this scenario isn't currently possible in VS Code as of
/// 2026-01-30 because the global settings are marked as scoped to the
/// "window," which will prevent VS Code from sending different values
/// to each workspace folder. But, other non-VS Code clients might
/// do something different, including allowing workspace folders to
/// purportedly have different global setting values. (Where "global"
/// here is referring the ty server's idea of what ought to be global
/// and not necessarily according to the LSP protocol or the LSP
/// clients.)
#[test]
fn global_settings_precedence() -> Result<()> {
    let root1 = SystemPath::new("root1");
    let root2 = SystemPath::new("root2");
    let main1 = root1.join("main.py");
    let main2 = root2.join("main.py");
    let main_content = "(";

    // This puts the setting change (no syntax errors) on root2,
    // which causes it to take precedence and apply even to root1.

    let mut server = TestServerBuilder::new()?
        .with_initialization_options(ClientOptions::default())
        .with_file(&main1, main_content)?
        .with_file(&main2, main_content)?
        .with_workspace(root1, None)?
        .with_workspace(
            root2,
            Some(ClientOptions {
                global: GlobalOptions {
                    show_syntax_errors: Some(false),
                    ..GlobalOptions::default()
                },
                ..ClientOptions::default()
            }),
        )?
        .build()
        .wait_until_workspaces_are_initialized();

    server.open_text_document(&main1, main_content, 1);
    let document_diagnostics = server.document_diagnostic_request(&main1, None);
    assert_snapshot!(
        condensed_document_diagnostic_snapshot(document_diagnostics),
        @"",
    );

    server.open_text_document(&main2, main_content, 1);
    let document_diagnostics = server.document_diagnostic_request(&main2, None);
    assert_snapshot!(
        condensed_document_diagnostic_snapshot(document_diagnostics),
        @"",
    );

    // Now we do it again, but apply the settings to root1 which
    // comes before root2. The default settings on root2 end up
    // winning out, and we get syntax error diagnostics.

    let mut server = TestServerBuilder::new()?
        .with_initialization_options(ClientOptions::default())
        .with_file(&main1, main_content)?
        .with_file(&main2, main_content)?
        .with_workspace(
            root1,
            Some(ClientOptions {
                global: GlobalOptions {
                    show_syntax_errors: Some(false),
                    ..GlobalOptions::default()
                },
                ..ClientOptions::default()
            }),
        )?
        .with_workspace(root2, None)?
        .build()
        .wait_until_workspaces_are_initialized();

    server.open_text_document(&main1, main_content, 1);
    let document_diagnostics = server.document_diagnostic_request(&main1, None);
    assert_snapshot!(
        condensed_document_diagnostic_snapshot(document_diagnostics),
        @"0:1..0:1[ERROR]: unexpected EOF while parsing",
    );

    server.open_text_document(&main2, main_content, 1);
    let document_diagnostics = server.document_diagnostic_request(&main2, None);
    assert_snapshot!(
        condensed_document_diagnostic_snapshot(document_diagnostics),
        @"0:1..0:1[ERROR]: unexpected EOF while parsing",
    );

    Ok(())
}

/// Test that workspace folders can be a vehicle for a change
/// to global settings. And that when global settings are
/// changed, it applies to all workspace folders.
#[test]
fn global_settings_change() -> Result<()> {
    let root1 = SystemPath::new("root1");
    let root2 = SystemPath::new("root2");
    let main1 = root1.join("main.py");
    let main2 = root2.join("main.py");
    let main_content = "(";

    // We initialize with default settings, which means
    // we get syntax error diagnostics.

    let mut server = TestServerBuilder::new()?
        .with_initialization_options(ClientOptions::default())
        .with_file(&main1, main_content)?
        .with_file(&main2, main_content)?
        .with_workspace(root1, None)?
        .build()
        .wait_until_workspaces_are_initialized();

    server.open_text_document(&main1, main_content, 1);
    let document_diagnostics = server.document_diagnostic_request(&main1, None);
    assert_snapshot!(
        condensed_document_diagnostic_snapshot(document_diagnostics),
        @"0:1..0:1[ERROR]: unexpected EOF while parsing",
    );

    // Now we'll add a new workspace folder with syntax error
    // diagnostics disabled. This will apply not just to the
    // new folder, but to the existing folders.
    server.add_workspace_folder(
        root2,
        Some(ClientOptions {
            global: GlobalOptions {
                show_syntax_errors: Some(false),
                ..GlobalOptions::default()
            },
            ..ClientOptions::default()
        }),
    )?;
    server.change_workspace_folders([root2], []);
    server = server.wait_until_workspaces_are_initialized();

    let document_diagnostics = server.document_diagnostic_request(&main1, None);
    assert_snapshot!(
        condensed_document_diagnostic_snapshot(document_diagnostics),
        @"",
    );

    server.open_text_document(&main2, main_content, 1);
    let document_diagnostics = server.document_diagnostic_request(&main2, None);
    assert_snapshot!(
        condensed_document_diagnostic_snapshot(document_diagnostics),
        @"",
    );

    Ok(())
}

/// A helper routine for creating a snapshot for a collection of
/// workspace diagnostics.
///
/// We mostly use this in our workspace folder tests to check that the
/// LSP is correctly recognizing and reporting diagnostics for each
/// workspace folder. This isn't really meant to test the diagnostics
/// themselves, hence the condensed output.
fn condensed_workspace_diagnostic_snapshot(report: WorkspaceDiagnosticReport) -> String {
    let items = report.items;
    items
        .into_iter()
        .map(|item| match item {
            WorkspaceDocumentDiagnosticReport::WorkspaceFullDocumentDiagnosticReport(
                doc_report,
            ) => {
                let diagnostics = condensed_full_document_diagnostic_report(
                    doc_report.full_document_diagnostic_report,
                )
                .join("\n\t");
                format!("{}\n\t{diagnostics}", doc_report.uri)
            }
            WorkspaceDocumentDiagnosticReport::WorkspaceUnchangedDocumentDiagnosticReport(
                doc_report,
            ) => {
                format!("{}\n\tUNCHANGED", doc_report.uri)
            }
        })
        .collect::<Vec<String>>()
        .join("\n")
}

pub(crate) fn condensed_document_diagnostic_snapshot(report: DocumentDiagnosticReport) -> String {
    match report {
        DocumentDiagnosticReport::RelatedFullDocumentDiagnosticReport(full) => {
            condensed_full_document_diagnostic_report(full.full_document_diagnostic_report)
                .join("\n")
        }
        // NOTE: It might be worth providing more details for these
        // cases, but I don't think there's currently a use case for
        // it.
        DocumentDiagnosticReport::RelatedUnchangedDocumentDiagnosticReport(_) => {
            "UNCHANGED".to_string()
        }
    }
}

fn condensed_full_document_diagnostic_report(report: FullDocumentDiagnosticReport) -> Vec<String> {
    report
        .items
        .into_iter()
        .map(|d| {
            let range = format!(
                "{start_line}:{start_char}..{end_line}:{end_char}",
                start_line = d.range.start.line,
                start_char = d.range.start.character,
                end_line = d.range.end.line,
                end_char = d.range.end.character,
            );
            let severity = match d.severity {
                Some(DiagnosticSeverity::Error) => "ERROR",
                Some(DiagnosticSeverity::Warning) => "WARNING",
                Some(DiagnosticSeverity::Information) => "INFORMATION",
                Some(DiagnosticSeverity::Hint) => "HINT",
                None => "unknown",
            };
            let Message::String(message) = d.message else {
                panic!(
                    "Only string-type diagnostic messages supported, got: {:?}",
                    d.message
                );
            };
            format!("{range}[{severity}]: {message}")
        })
        .collect()
}

/// Asks for workspace diagnostics in a way that anticipates "long polling."
///
/// This specifically occurs when there aren't any workspace diagnostics
/// to report. We use this technique in some (weird) tests where there aren't
/// any workspaces remaining, and thus expect to not receive any diagnostics.
///
/// This also initiates a shutdown of the server, which ultimately cancels
/// the long polling and returns (an expected empty) workspace diagnostic
/// response.
///
/// NOTE: Using this sparingly, since the way this asserts that long polling
/// is occurring is by sending a request with a 2-second timeout that we
/// expect to never have a response for.
fn get_expected_empty_workspace_diagnostics_and_shutdown(
    mut server: TestServer,
) -> WorkspaceDiagnosticReport {
    let request_id = send_workspace_diagnostic_request(&mut server);
    assert_workspace_diagnostics_suspends_for_long_polling(&mut server, &request_id);
    shutdown_and_await_workspace_diagnostic(server, &request_id)
}
