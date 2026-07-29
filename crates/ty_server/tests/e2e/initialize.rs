use anyhow::Result;
use lsp_types::{
    BaseUri, DidChangeWatchedFilesRegistrationOptions, GlobPattern, Position, RegistrationRequest,
    ShowMessageNotification, UnregistrationRequest, Uri,
};
use ruff_db::system::SystemPath;
use serde_json::Value;
use ty_server::{ClientOptions, DiagnosticMode};

use crate::TestServerBuilder;

#[test]
fn empty_workspace_folders() -> Result<()> {
    let server = TestServerBuilder::new()?
        .build()
        .wait_until_workspaces_are_initialized();

    let initialization_result = server.initialization_result().unwrap();

    insta::assert_json_snapshot!("initialization", initialization_result);

    Ok(())
}

#[test]
fn single_workspace_folder() -> Result<()> {
    let workspace_root = SystemPath::new("foo");
    let server = TestServerBuilder::new()?
        .with_workspace(workspace_root, None)?
        .build()
        .wait_until_workspaces_are_initialized();

    let initialization_result = server.initialization_result().unwrap();

    insta::assert_json_snapshot!("initialization_with_workspace", initialization_result);

    Ok(())
}

/// Tests that the server sends a registration request for diagnostics if workspace diagnostics
/// are enabled via initialization options and dynamic registration is enabled, even if the
/// workspace configuration is not supported by the client.
#[test]
fn workspace_diagnostic_registration_without_configuration() -> Result<()> {
    let workspace_root = SystemPath::new("foo");
    let mut server = TestServerBuilder::new()?
        .with_initialization_options(
            ClientOptions::default().with_diagnostic_mode(DiagnosticMode::Workspace),
        )
        .with_workspace(workspace_root, None)?
        .enable_workspace_configuration(false)
        .enable_diagnostic_dynamic_registration(true)
        .build();

    // No need to wait for workspaces to initialize as the client does not support workspace
    // configuration.

    let (_, params) = server.await_request::<RegistrationRequest>();
    let [registration] = params.registrations.as_slice() else {
        panic!(
            "Expected a single registration, got: {:#?}",
            params.registrations
        );
    };

    insta::assert_json_snapshot!(registration, @r#"
    {
      "id": "ty/textDocument/diagnostic",
      "method": "textDocument/diagnostic",
      "registerOptions": {
        "documentSelector": null,
        "identifier": "ty",
        "interFileDependencies": true,
        "workDoneProgress": true,
        "workspaceDiagnostics": true
      }
    }
    "#);

    Ok(())
}

/// Tests that the server sends a registration request for diagnostics if open files diagnostics
/// are enabled via initialization options and dynamic registration is enabled, even if the
/// workspace configuration is not supported by the client.
#[test]
fn open_files_diagnostic_registration_without_configuration() -> Result<()> {
    let workspace_root = SystemPath::new("foo");
    let mut server = TestServerBuilder::new()?
        .with_initialization_options(
            ClientOptions::default().with_diagnostic_mode(DiagnosticMode::OpenFilesOnly),
        )
        .with_workspace(workspace_root, None)?
        .enable_workspace_configuration(false)
        .enable_diagnostic_dynamic_registration(true)
        .build();

    // No need to wait for workspaces to initialize as the client does not support workspace
    // configuration.

    let (_, params) = server.await_request::<RegistrationRequest>();
    let [registration] = params.registrations.as_slice() else {
        panic!(
            "Expected a single registration, got: {:#?}",
            params.registrations
        );
    };

    insta::assert_json_snapshot!(registration, @r#"
    {
      "id": "ty/textDocument/diagnostic",
      "method": "textDocument/diagnostic",
      "registerOptions": {
        "documentSelector": null,
        "identifier": "ty",
        "interFileDependencies": true,
        "workDoneProgress": false,
        "workspaceDiagnostics": false
      }
    }
    "#);

    Ok(())
}

/// Tests that the server sends a registration request for diagnostics if workspace diagnostics
/// are enabled via initialization options and dynamic registration is enabled.
#[test]
fn workspace_diagnostic_registration_via_initialization() -> Result<()> {
    let workspace_root = SystemPath::new("foo");
    let mut server = TestServerBuilder::new()?
        .with_initialization_options(
            ClientOptions::default().with_diagnostic_mode(DiagnosticMode::Workspace),
        )
        .with_workspace(workspace_root, None)?
        .enable_diagnostic_dynamic_registration(true)
        .build()
        .wait_until_workspaces_are_initialized();

    let (_, params) = server.await_request::<RegistrationRequest>();
    let [registration] = params.registrations.as_slice() else {
        panic!(
            "Expected a single registration, got: {:#?}",
            params.registrations
        );
    };

    insta::assert_json_snapshot!(registration, @r#"
    {
      "id": "ty/textDocument/diagnostic",
      "method": "textDocument/diagnostic",
      "registerOptions": {
        "documentSelector": null,
        "identifier": "ty",
        "interFileDependencies": true,
        "workDoneProgress": true,
        "workspaceDiagnostics": true
      }
    }
    "#);

    Ok(())
}

/// Tests that the server sends a registration request for diagnostics if open files diagnostics
/// are enabled via initialization options and dynamic registration is enabled.
#[test]
fn open_files_diagnostic_registration_via_initialization() -> Result<()> {
    let workspace_root = SystemPath::new("foo");
    let mut server = TestServerBuilder::new()?
        .with_initialization_options(
            ClientOptions::default().with_diagnostic_mode(DiagnosticMode::OpenFilesOnly),
        )
        .with_workspace(workspace_root, None)?
        .enable_diagnostic_dynamic_registration(true)
        .build()
        .wait_until_workspaces_are_initialized();

    let (_, params) = server.await_request::<RegistrationRequest>();
    let [registration] = params.registrations.as_slice() else {
        panic!(
            "Expected a single registration, got: {:#?}",
            params.registrations
        );
    };

    insta::assert_json_snapshot!(registration, @r#"
    {
      "id": "ty/textDocument/diagnostic",
      "method": "textDocument/diagnostic",
      "registerOptions": {
        "documentSelector": null,
        "identifier": "ty",
        "interFileDependencies": true,
        "workDoneProgress": false,
        "workspaceDiagnostics": false
      }
    }
    "#);

    Ok(())
}

/// Tests that the server sends a registration request for diagnostics if workspace diagnostics
/// are enabled and dynamic registration is enabled.
#[test]
fn workspace_diagnostic_registration() -> Result<()> {
    let workspace_root = SystemPath::new("foo");
    let mut server = TestServerBuilder::new()?
        .with_workspace(
            workspace_root,
            Some(ClientOptions::default().with_diagnostic_mode(DiagnosticMode::Workspace)),
        )?
        .enable_diagnostic_dynamic_registration(true)
        .build()
        .wait_until_workspaces_are_initialized();

    let (_, params) = server.await_request::<RegistrationRequest>();
    let [registration] = params.registrations.as_slice() else {
        panic!(
            "Expected a single registration, got: {:#?}",
            params.registrations
        );
    };

    insta::assert_json_snapshot!(registration, @r#"
    {
      "id": "ty/textDocument/diagnostic",
      "method": "textDocument/diagnostic",
      "registerOptions": {
        "documentSelector": null,
        "identifier": "ty",
        "interFileDependencies": true,
        "workDoneProgress": true,
        "workspaceDiagnostics": true
      }
    }
    "#);

    Ok(())
}

/// Tests that the server sends a registration request for diagnostics if workspace diagnostics are
/// disabled and dynamic registration is enabled.
#[test]
fn open_files_diagnostic_registration() -> Result<()> {
    let workspace_root = SystemPath::new("foo");
    let mut server = TestServerBuilder::new()?
        .with_workspace(
            workspace_root,
            Some(ClientOptions::default().with_diagnostic_mode(DiagnosticMode::OpenFilesOnly)),
        )?
        .enable_diagnostic_dynamic_registration(true)
        .build()
        .wait_until_workspaces_are_initialized();

    let (_, params) = server.await_request::<RegistrationRequest>();
    let [registration] = params.registrations.as_slice() else {
        panic!(
            "Expected a single registration, got: {:#?}",
            params.registrations
        );
    };

    insta::assert_json_snapshot!(registration, @r#"
    {
      "id": "ty/textDocument/diagnostic",
      "method": "textDocument/diagnostic",
      "registerOptions": {
        "documentSelector": null,
        "identifier": "ty",
        "interFileDependencies": true,
        "workDoneProgress": false,
        "workspaceDiagnostics": false
      }
    }
    "#);

    Ok(())
}

/// Tests that the server can disable language services for a workspace via initialization options.
#[test]
fn disable_language_services_set_on_initialization() -> Result<()> {
    let workspace_root = SystemPath::new("src");
    let foo = SystemPath::new("src/foo.py");
    let foo_content = "\
def foo() -> str:
    return 42
";

    let mut server = TestServerBuilder::new()?
        .with_initialization_options(ClientOptions::default().with_disable_language_services(true))
        .with_workspace(workspace_root, None)?
        .with_file(foo, foo_content)?
        .build()
        .wait_until_workspaces_are_initialized();

    server.open_text_document(foo, foo_content, 1);
    let hover = server.hover_request(foo, Position::new(0, 5));

    assert!(
        hover.is_none(),
        "Expected no hover information, got: {hover:?}"
    );

    Ok(())
}

/// Tests that the server can disable language services for a workspace via workspace configuration
/// request.
#[test]
fn disable_language_services_set_on_workspace() -> Result<()> {
    let workspace_root = SystemPath::new("src");
    let foo = SystemPath::new("src/foo.py");
    let foo_content = "\
def foo() -> str:
    return 42
";

    let mut server = TestServerBuilder::new()?
        .with_workspace(
            workspace_root,
            Some(ClientOptions::default().with_disable_language_services(true)),
        )?
        .with_file(foo, foo_content)?
        .build()
        .wait_until_workspaces_are_initialized();

    server.open_text_document(foo, foo_content, 1);
    let hover = server.hover_request(foo, Position::new(0, 5));

    assert!(
        hover.is_none(),
        "Expected no hover information, got: {hover:?}"
    );

    Ok(())
}

/// Tests that the server can disable language services for one workspace while keeping them
/// enabled for another.
#[test]
#[ignore = "Requires multiple workspace support in the server and test server"]
fn disable_language_services_for_one_workspace() -> Result<()> {
    let workspace_a = SystemPath::new("src/a");
    let workspace_b = SystemPath::new("src/b");
    let foo = SystemPath::new("src/a/foo.py");
    let bar = SystemPath::new("src/b/bar.py");
    let foo_content = "\
def foo() -> str:
    return 42
";
    let bar_content = "\
def bar() -> str:
    return 42
";

    let mut server = TestServerBuilder::new()?
        .with_workspace(
            workspace_a,
            Some(ClientOptions::default().with_disable_language_services(true)),
        )?
        .with_workspace(workspace_b, None)?
        .with_file(foo, foo_content)?
        .with_file(bar, bar_content)?
        .build()
        .wait_until_workspaces_are_initialized();

    server.open_text_document(foo, foo_content, 1);
    let hover_foo = server.hover_request(foo, Position::new(0, 5));
    assert!(
        hover_foo.is_none(),
        "Expected no hover information for workspace A, got: {hover_foo:?}"
    );

    server.open_text_document(bar, bar_content, 1);
    let hover_bar = server.hover_request(bar, Position::new(0, 5));
    assert!(
        hover_bar.is_some(),
        "Expected hover information for workspace B, got: {hover_bar:?}"
    );

    Ok(())
}

/// Tests that the server sends a warning notification if user provided unknown options during
/// initialization.
#[test]
fn unknown_initialization_options() -> Result<()> {
    let workspace_root = SystemPath::new("foo");
    let mut server = TestServerBuilder::new()?
        .with_workspace(workspace_root, None)?
        .with_initialization_options(
            ClientOptions::default().with_unknown([("bar".to_string(), Value::Null)].into()),
        )
        .build()
        .wait_until_workspaces_are_initialized();

    let show_message_params = server.await_notification::<ShowMessageNotification>();

    insta::assert_json_snapshot!(show_message_params, @r#"
    {
      "type": 2,
      "message": "Received unknown options during initialization: {\n  \"bar\": null\n}"
    }
    "#);

    Ok(())
}

/// Tests that the server sends a warning notification if user provided unknown options in the
/// workspace configuration.
#[test]
fn unknown_options_in_workspace_configuration() -> Result<()> {
    let workspace_root = SystemPath::new("foo");
    let mut server = TestServerBuilder::new()?
        .with_workspace(
            workspace_root,
            Some(ClientOptions::default().with_unknown([("bar".to_string(), Value::Null)].into())),
        )?
        .build()
        .wait_until_workspaces_are_initialized();

    let show_message_params = server.await_notification::<ShowMessageNotification>();

    insta::assert_json_snapshot!(show_message_params, @r#"
    {
      "type": 2,
      "message": "Received unknown options for workspace `file://<temp_dir>/foo`: {\n  \"bar\": null\n}"
    }
    "#);

    Ok(())
}

/// Tests that the server can register multiple capabilities at once.
///
/// This test would need to be updated when the server supports additional capabilities in the
/// future.
///
/// TODO: This test currently only verifies a single capability. It should be
/// updated with more dynamic capabilities when the server supports it.
#[test]
fn register_multiple_capabilities() -> Result<()> {
    let workspace_root = SystemPath::new("foo");
    let mut server = TestServerBuilder::new()?
        .with_workspace(workspace_root, None)?
        .with_initialization_options(
            ClientOptions::default().with_diagnostic_mode(DiagnosticMode::Workspace),
        )
        .enable_diagnostic_dynamic_registration(true)
        .build()
        .wait_until_workspaces_are_initialized();

    let (_, params) = server.await_request::<RegistrationRequest>();
    let registrations = params.registrations;

    insta::assert_json_snapshot!(registrations, @r#"
    [
      {
        "id": "ty/textDocument/diagnostic",
        "method": "textDocument/diagnostic",
        "registerOptions": {
          "documentSelector": null,
          "identifier": "ty",
          "interFileDependencies": true,
          "workDoneProgress": true,
          "workspaceDiagnostics": true
        }
      }
    ]
    "#);

    Ok(())
}

#[test]
fn lazy_chalk_project_refreshes_file_watcher_registration() -> Result<()> {
    let workspace_root = SystemPath::new("workspace");
    let chalk_root = SystemPath::new("chalk-project");
    let external_root = SystemPath::new("external");
    let main = chalk_root.join("main.py");
    let second = chalk_root.join("second.py");
    let mut server = TestServerBuilder::new()?
        .with_workspace(workspace_root, None)?
        .with_initialization_options(
            ClientOptions::default().with_diagnostic_mode(DiagnosticMode::Off),
        )
        .with_file(chalk_root.join("chalk.yml"), "{}")?
        .with_file(
            chalk_root.join("ty.toml"),
            "[environment]\nextra-paths = [\"../external\"]\n",
        )?
        .with_file(&main, "")?
        .with_file(&second, "")?
        .with_file(external_root.join("dependency.py"), "")?
        .enable_workspace_configuration(false)
        .enable_did_change_watched_files(true)
        .build();

    let has_watcher_for = |options: &DidChangeWatchedFilesRegistrationOptions, expected: &Uri| {
        options.watchers.iter().any(|watcher| {
            matches!(
                &watcher.glob_pattern,
                GlobPattern::RelativePattern(pattern)
                    if matches!(&pattern.base_uri, BaseUri::Uri(uri) if uri == expected)
            )
        })
    };

    let (_, params) = server.await_request::<RegistrationRequest>();
    let [registration] = params.registrations.as_slice() else {
        panic!(
            "Expected a single registration, got: {:#?}",
            params.registrations
        );
    };
    assert_eq!(registration.method, "workspace/didChangeWatchedFiles");
    let initial_options: DidChangeWatchedFilesRegistrationOptions =
        serde_json::from_value(registration.register_options.clone().unwrap())?;
    let workspace_uri = server.file_uri(workspace_root);
    let chalk_uri = server.file_uri(chalk_root);
    let external_uri = server.file_uri(external_root);
    assert!(has_watcher_for(&initial_options, &workspace_uri));
    assert!(!has_watcher_for(&initial_options, &chalk_uri));
    assert!(!has_watcher_for(&initial_options, &external_uri));

    server.open_text_document(&main, "", 1);

    let (_, params) = server.await_request::<UnregistrationRequest>();
    let [unregistration] = params.unregisterations.as_slice() else {
        panic!(
            "Expected a single unregistration, got: {:#?}",
            params.unregisterations
        );
    };
    assert_eq!(unregistration.id, "ty/workspace/didChangeWatchedFiles");
    assert_eq!(unregistration.method, "workspace/didChangeWatchedFiles");

    let (_, params) = server.await_request::<RegistrationRequest>();
    let [registration] = params.registrations.as_slice() else {
        panic!(
            "Expected a single registration, got: {:#?}",
            params.registrations
        );
    };
    assert_eq!(registration.method, "workspace/didChangeWatchedFiles");
    let refreshed_options: DidChangeWatchedFilesRegistrationOptions =
        serde_json::from_value(registration.register_options.clone().unwrap())?;
    assert!(has_watcher_for(&refreshed_options, &workspace_uri));
    assert!(has_watcher_for(&refreshed_options, &chalk_uri));
    assert!(has_watcher_for(&refreshed_options, &external_uri));

    server.open_text_document(&second, "", 1);
    assert!(
        server
            .try_await_request::<UnregistrationRequest>(Some(std::time::Duration::from_millis(100)))
            .is_err()
    );
    assert!(
        server
            .try_await_request::<RegistrationRequest>(Some(std::time::Duration::from_millis(100)))
            .is_err()
    );

    Ok(())
}

#[test]
fn removing_workspace_refreshes_lazy_chalk_file_watcher_registration() -> Result<()> {
    let retained_workspace = SystemPath::new("retained");
    let repository = SystemPath::new("repository");
    let workspace = repository.join("service");
    let main = workspace.join("main.py");
    let mut server = TestServerBuilder::new()?
        .with_workspace(retained_workspace, None)?
        .with_workspace(&workspace, None)?
        .with_initialization_options(
            ClientOptions::default().with_diagnostic_mode(DiagnosticMode::Off),
        )
        .with_file(repository.join("chalk.yml"), "{}")?
        .with_file(&main, "")?
        .enable_workspace_configuration(false)
        .enable_did_change_watched_files(true)
        .build();

    let has_watcher_for = |options: &DidChangeWatchedFilesRegistrationOptions, expected: &Uri| {
        options.watchers.iter().any(|watcher| {
            matches!(
                &watcher.glob_pattern,
                GlobPattern::RelativePattern(pattern)
                    if matches!(&pattern.base_uri, BaseUri::Uri(uri) if uri == expected)
            )
        })
    };
    let retained_uri = server.file_uri(retained_workspace);
    let repository_uri = server.file_uri(repository);
    let workspace_uri = server.file_uri(&workspace);

    let (_, params) = server.await_request::<RegistrationRequest>();
    let [registration] = params.registrations.as_slice() else {
        panic!(
            "Expected a single registration, got: {:#?}",
            params.registrations
        );
    };
    let initial_options: DidChangeWatchedFilesRegistrationOptions =
        serde_json::from_value(registration.register_options.clone().unwrap())?;
    assert!(has_watcher_for(&initial_options, &retained_uri));
    assert!(has_watcher_for(&initial_options, &workspace_uri));
    assert!(!has_watcher_for(&initial_options, &repository_uri));

    server.open_text_document(&main, "", 1);
    server.await_request::<UnregistrationRequest>();
    let (_, params) = server.await_request::<RegistrationRequest>();
    let [registration] = params.registrations.as_slice() else {
        panic!(
            "Expected a single registration, got: {:#?}",
            params.registrations
        );
    };
    let added_options: DidChangeWatchedFilesRegistrationOptions =
        serde_json::from_value(registration.register_options.clone().unwrap())?;
    assert!(has_watcher_for(&added_options, &retained_uri));
    assert!(has_watcher_for(&added_options, &repository_uri));

    server.change_workspace_folders([], [&workspace]);
    server.await_request::<UnregistrationRequest>();
    let (_, params) = server.await_request::<RegistrationRequest>();
    let [registration] = params.registrations.as_slice() else {
        panic!(
            "Expected a single registration, got: {:#?}",
            params.registrations
        );
    };
    let removed_options: DidChangeWatchedFilesRegistrationOptions =
        serde_json::from_value(registration.register_options.clone().unwrap())?;
    assert!(has_watcher_for(&removed_options, &retained_uri));
    assert!(!has_watcher_for(&removed_options, &repository_uri));
    assert!(!has_watcher_for(&removed_options, &workspace_uri));

    Ok(())
}

/// Tests that the server doesn't panic when `VIRTUAL_ENV` points to a non-existent directory.
///
/// See: <https://github.com/astral-sh/ty/issues/2031>
#[test]
fn missing_virtual_env_does_not_panic() -> Result<()> {
    let workspace_root = SystemPath::new("project");

    // This should not panic even though VIRTUAL_ENV points to a non-existent path
    let mut server = TestServerBuilder::new()?
        .with_workspace(workspace_root, None)?
        .with_env_var("VIRTUAL_ENV", "/nonexistent/virtual/env/path")
        .build()
        .wait_until_workspaces_are_initialized();

    let show_message_params = server.await_notification::<ShowMessageNotification>();

    insta::assert_snapshot!(show_message_params.message, @"Failed to load project for workspace file://<temp_dir>/project. Please refer to the logs for more details.");

    Ok(())
}
