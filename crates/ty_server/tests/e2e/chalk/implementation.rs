//! End-to-end tests for Chalk feature-to-resolver navigation.

use lsp_types::{
    ImplementationParams, ImplementationRequest, ImplementationResponse, PartialResultParams,
    Position, TextDocumentIdentifier, TextDocumentPositionParams, WorkDoneProgressParams,
};

use crate::{TestServer, TestServerBuilder};

#[test]
fn feature_declaration_navigates_to_resolver() -> anyhow::Result<()> {
    let features = "from chalk.features import features\n@features\nclass User:\n    score: int\n";
    let resolvers = "from chalk import online\nfrom features import User\n\n@online\ndef resolve_score() -> User.score:\n    return 1\n";
    let builder = TestServerBuilder::new()?;
    let python_home = builder.file_path("base/bin");
    let mut server = builder
        .with_file("chalk.yml", "{}")?
        .with_file("base/bin/python", "")?
        .with_file(".venv/bin/python", "")?
        .with_file(".venv/pyvenv.cfg", format!("home = {python_home}\n"))?
        .with_file(
            ".venv/lib/python3.14/site-packages/chalk/__init__.py",
            "from chalk.features.resolver import online as online\n",
        )?
        .with_file(
            ".venv/lib/python3.14/site-packages/chalk/features/__init__.py",
            "def features(cls): ...\n",
        )?
        .with_file(
            ".venv/lib/python3.14/site-packages/chalk/features/resolver.py",
            "def online(function): ...\n",
        )?
        .with_file("features.py", features)?
        .with_file("resolvers.py", resolvers)?
        .with_file("nested/chalk.yml", "{}")?
        .with_file("nested/other.py", resolvers)?
        .build()
        .wait_until_workspaces_are_initialized();
    server.open_text_document("features.py", features, 1);

    let result = implementation(&mut server, "features.py", Position::new(3, 6));
    let Some(ImplementationResponse::Definition(lsp_types::Definition::LocationList(locations))) =
        result.as_ref()
    else {
        panic!("expected resolver locations, got {result:?}");
    };
    assert_eq!(locations.len(), 1);
    assert_eq!(locations[0].uri, server.file_uri("resolvers.py"));
    assert_eq!(locations[0].range.start, Position::new(4, 4));

    server.open_text_document("resolvers.py", resolvers, 1);
    let reference = implementation(&mut server, "resolvers.py", Position::new(4, 29));
    assert_eq!(reference, result);

    let updated_resolvers = resolvers.replace("@online", "# @online");
    server.change_text_document(
        "resolvers.py",
        vec![
            lsp_types::TextDocumentContentChangeEvent::TextDocumentContentChangeWholeDocument(
                lsp_types::TextDocumentContentChangeWholeDocument {
                    text: updated_resolvers,
                },
            ),
        ],
        2,
    );
    assert_eq!(
        implementation(&mut server, "features.py", Position::new(3, 6)),
        None
    );

    Ok(())
}

fn implementation(
    server: &mut TestServer,
    file: &str,
    position: Position,
) -> Option<ImplementationResponse> {
    server.send_request_await::<ImplementationRequest>(ImplementationParams {
        text_document_position_params: TextDocumentPositionParams {
            text_document: TextDocumentIdentifier {
                uri: server.file_uri(file),
            },
            position,
        },
        work_done_progress_params: WorkDoneProgressParams::default(),
        partial_result_params: PartialResultParams::default(),
    })
}
