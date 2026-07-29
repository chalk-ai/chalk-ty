use lsp_types::WorkspaceSymbolRequest;
use lsp_types::{WorkspaceSymbolParams, WorkspaceSymbolResponse};
use ty_ide::{WorkspaceSymbolInfo, workspace_symbols_for_files};
use ty_project::Db as _;

use crate::server::api::symbols::convert_to_lsp_symbol_information;
use crate::server::api::traits::{
    BackgroundRequestHandler, RequestHandler, RetriableRequestHandler,
};
use crate::session::SessionSnapshot;
use crate::session::client::Client;

pub(crate) struct WorkspaceSymbolRequestHandler;

impl RequestHandler for WorkspaceSymbolRequestHandler {
    type RequestType = WorkspaceSymbolRequest;
}

impl BackgroundRequestHandler for WorkspaceSymbolRequestHandler {
    fn run(
        snapshot: &SessionSnapshot,
        _client: &Client,
        params: WorkspaceSymbolParams,
    ) -> crate::server::Result<Option<WorkspaceSymbolResponse>> {
        let query = &params.query;
        if query.is_empty() {
            return Ok(None);
        }

        let mut all_symbols = Vec::new();

        for project in snapshot.routed_projects() {
            let db = project.db();
            let indexed_files = db.project().files(db);
            let files = indexed_files
                .iter()
                .copied()
                .filter(|file| snapshot.project_owns_file(project, *file));

            // Get workspace symbols matching the query
            let start = std::time::Instant::now();
            let workspace_symbol_infos = workspace_symbols_for_files(db, query, files);
            tracing::debug!(
                "Found {len} workspace symbols in {elapsed:?}",
                len = workspace_symbol_infos.len(),
                elapsed = std::time::Instant::now().duration_since(start)
            );

            // Convert to LSP SymbolInformation
            for workspace_symbol_info in workspace_symbol_infos {
                let WorkspaceSymbolInfo { symbol, file } = workspace_symbol_info;

                // Get position encoding from session
                let encoding = snapshot.position_encoding();

                let Some(symbol) = convert_to_lsp_symbol_information(db, file, symbol, encoding)
                else {
                    tracing::debug!(
                        "Failed to convert symbol '{}' to LSP symbol information",
                        file.path(db)
                    );
                    continue;
                };

                all_symbols.push(symbol);
            }
        }

        if all_symbols.is_empty() {
            Ok(None)
        } else {
            Ok(Some(all_symbols.into()))
        }
    }
}

impl RetriableRequestHandler for WorkspaceSymbolRequestHandler {}
