//! Chalk feature-to-resolver navigation through Go to Implementation.

use std::borrow::Cow;

use lsp_types::{ImplementationParams, ImplementationRequest, ImplementationResponse, Uri};
use ruff_db::files::FileRange;
use ruff_db::source::source_text;
use ruff_text_size::Ranged;
use ty_chalk::feature_resolvers;
use ty_ide::{NavigationTarget, goto_definition};
use ty_project::ProjectDatabase;

use crate::document::{PositionExt, ToLink};
use crate::server::api::traits::{
    BackgroundDocumentRequestHandler, RequestHandler, RetriableRequestHandler,
};
use crate::session::DocumentSnapshot;
use crate::session::client::Client;

pub(crate) struct ImplementationRequestHandler;

impl RequestHandler for ImplementationRequestHandler {
    type RequestType = ImplementationRequest;
}

impl BackgroundDocumentRequestHandler for ImplementationRequestHandler {
    fn document_uri(params: &ImplementationParams) -> Cow<'_, Uri> {
        Cow::Borrowed(&params.text_document_position_params.text_document.uri)
    }

    fn run_with_snapshot(
        db: &ProjectDatabase,
        snapshot: &DocumentSnapshot,
        _client: &Client,
        params: ImplementationParams,
    ) -> crate::server::Result<Option<ImplementationResponse>> {
        if snapshot
            .workspace_settings()
            .is_language_services_disabled()
        {
            return Ok(None);
        }

        let Some(project) = snapshot.chalk_project() else {
            return Ok(None);
        };
        let Some(file) = snapshot.to_notebook_or_file(db) else {
            return Ok(None);
        };
        if !project.python_files(db).any(|source| source == file) {
            return Ok(None);
        }
        let Some(offset) = params.text_document_position_params.position.to_text_size(
            db,
            file,
            snapshot.uri(),
            snapshot.encoding(),
        ) else {
            return Ok(None);
        };
        let Some(declaration) = goto_definition(db, file, offset) else {
            return Ok(None);
        };
        let targets: Vec<_> = declaration
            .into_iter()
            .map(|target| FileRange::new(target.file(), target.focus_range()))
            .collect();
        let Some(first) = targets.first() else {
            return Ok(None);
        };
        let source = source_text(db, first.file());
        let Some(name) = source
            .as_str()
            .get(first.range().start().to_usize()..first.range().end().to_usize())
        else {
            return Ok(None);
        };

        let locations = feature_resolvers(db, project, name, &targets)
            .into_iter()
            .map(NavigationTarget::from)
            .filter_map(|target| target.to_location(db, snapshot.encoding()))
            .collect::<Vec<_>>();
        if locations.is_empty() {
            Ok(None)
        } else {
            Ok(Some(ImplementationResponse::Definition(locations.into())))
        }
    }
}

impl RetriableRequestHandler for ImplementationRequestHandler {}
