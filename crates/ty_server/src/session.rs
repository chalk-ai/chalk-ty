//! Data model, state management, and configuration resolution.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::ops::{Deref, DerefMut};
use std::panic::RefUnwindSafe;
use std::sync::Arc;

use anyhow::{Context, anyhow};
use lsp_server::{Message, RequestId};
use lsp_types::{
    ClientInfo, DiagnosticProvider, DiagnosticRegistrationOptions,
    DidChangeWatchedFilesRegistrationOptions, FileSystemWatcher, Registration, RegistrationParams,
    TextDocumentContentChangeEvent, Unregistration, UnregistrationParams, Uri,
};
use lsp_types::{DidChangeWatchedFilesNotification, ExitNotification, Notification};
use lsp_types::{
    DocumentDiagnosticRequest, RegistrationRequest, Request, ShutdownRequest,
    UnregistrationRequest, WorkspaceDiagnosticRequest,
};
use ruff_db::Db;
use ruff_db::files::{File, system_path_to_file};
use ruff_db::system::{System, SystemPath, SystemPathBuf};
use ruff_python_ast::PySourceType;
use ty_chalk::{
    ActiveChalkProject, ChalkProject, ChalkProjectError, ChalkProjectInput, discover_chalk_project,
};
use ty_combine::Combine;
use ty_project::metadata::Options;
use ty_project::metadata::options::ProjectOptionsOverrides;
use ty_project::watch::{ChangeEvent, CreatedKind};
use ty_project::{Db as _, ProjectDatabase, ProjectMetadata};

use index::DocumentError;
use ty_python_core::program::UseDefaultStrategy;

pub(crate) use self::options::InitializationOptions;
pub use self::options::{ClientOptions, DiagnosticMode, GlobalOptions, WorkspaceOptions};
pub(crate) use self::settings::{GlobalSettings, WorkspaceSettings};
use crate::capabilities::{ResolvedClientCapabilities, server_diagnostic_options};
use crate::document::{DocumentKey, DocumentVersion, LanguageId, NotebookDocument};
use crate::server::{Action, publish_settings_diagnostics};
use crate::session::client::Client;
use crate::session::index::Document;
use crate::session::request_queue::RequestQueue;
use crate::system::{AnySystemPath, LSPSystem};
use crate::{PositionEncoding, TextDocument};
use index::Index;

pub(crate) mod client;
pub(crate) mod index;
mod options;
mod request_queue;
mod settings;

const FILE_WATCHER_REGISTRATION_ID: &str = "ty/workspace/didChangeWatchedFiles";

/// The global state for the LSP
pub(crate) struct Session {
    /// A native system to use with the [`LSPSystem`].
    native_system: Arc<dyn System + 'static + Send + Sync + RefUnwindSafe>,

    /// Used to retrieve information about open documents and settings.
    ///
    /// This will be [`None`] when a mutable reference is held to the index via [`index_mut`]
    /// to prevent the index from being accessed while it is being modified. It will be restored
    /// when the mutable reference ([`MutIndexGuard`]) is dropped.
    ///
    /// [`index_mut`]: Session::index_mut
    index: Option<Arc<Index>>,

    /// Maps workspace folders to their respective workspace.
    workspaces: Workspaces,

    /// The projects across all workspaces.
    projects: BTreeMap<SystemPathBuf, ProjectState>,

    /// Initialization options that were provided by the client during server initialization.
    initialization_options: InitializationOptions,

    /// Resolved global settings that are shared across all workspaces.
    global_settings: Arc<GlobalSettings>,

    /// The global position encoding, negotiated during LSP initialization.
    position_encoding: PositionEncoding,

    /// Tracks what LSP features the client supports and doesn't support.
    resolved_client_capabilities: ResolvedClientCapabilities,

    /// Tracks the pending requests between client and server.
    request_queue: RequestQueue,

    /// Has the client requested the server to shutdown.
    shutdown_requested: bool,

    /// Whether the server has dynamically registered the diagnostic capability with the client.
    /// Is the connected client a `TestServer` instance.
    in_test: bool,

    deferred_messages: VecDeque<Message>,

    /// A revision counter. It gets incremented on every change to `Session` that
    /// could result in different workspace diagnostics.
    revision: u64,

    /// A pending workspace diagnostics request because there were no diagnostics
    /// or no changes when when the request ran last time.
    /// We'll re-run the request after every change to `Session` (see `revision`)
    /// to see if there are now changes and, if so, respond to the client.
    suspended_workspace_diagnostics_request: Option<SuspendedWorkspaceDiagnosticRequest>,

    /// Registrations is a set of LSP methods that have been dynamically registered with the
    /// client.
    registrations: HashSet<String>,

    /// Whether adding a persistent project changed the paths covered by file watching.
    file_watcher_registration_needs_refresh: bool,

    /// The name of the client (editor) that connected to this server.
    client_name: ClientName,
}

/// LSP State for a Project
pub(crate) struct ProjectState {
    kind: ProjectKind,

    /// The workspace whose settings were used to create this project.
    ///
    /// This is separate from the routing root because a Chalk project can inherit ty
    /// configuration from above its Chalk root.
    workspace_root: Option<SystemPathBuf>,

    /// Files that we have outstanding otherwise-untracked pushed diagnostics for.
    ///
    /// In `CheckMode::OpenFiles` we still read some files that the client hasn't
    /// told us to open. Notably settings files like `pyproject.toml`. In this
    /// mode the client will never pull diagnostics for that file, and because
    /// the file isn't formally "open" we also don't have a reliable signal to
    /// refresh diagnostics for it either.
    ///
    /// However diagnostics for those files include things like "you typo'd your
    /// configuration for the LSP itself", so it's really important that we tell
    /// the user about them! So we remember which ones we have emitted diagnostics
    /// for so that we can clear the diagnostics for all of them before we go
    /// to update any of them.
    pub(crate) untracked_files_with_pushed_diagnostics: Vec<Uri>,

    chalk_project: Option<ActiveChalkProject>,

    // Note: This field should be last to ensure the `db` gets dropped last.
    // The db drop order matters because we call `Arc::into_inner` on some Arc's
    // and we use Salsa's cancellation to guarantee that there's only a single reference to the `Arc`.
    // However, this requires that the db drops last.
    // This shouldn't matter here because the db's stored in the session are the
    // only reference we want to hold on, but better be safe than sorry ;).
    pub(crate) db: ProjectDatabase,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ProjectKind {
    Workspace,
    Chalk,
}

impl ProjectState {
    pub(crate) fn kind(&self) -> ProjectKind {
        self.kind
    }

    pub(crate) fn chalk_project(&self) -> Option<ChalkProjectInput> {
        self.chalk_project.as_ref().map(ActiveChalkProject::input)
    }
}

impl Session {
    pub(crate) fn new(
        resolved_client_capabilities: ResolvedClientCapabilities,
        position_encoding: PositionEncoding,
        workspace_uris: Vec<Uri>,
        initialization_options: InitializationOptions,
        native_system: Arc<dyn System + 'static + Send + Sync + RefUnwindSafe>,
        client_name: ClientName,
        in_test: bool,
    ) -> crate::Result<Self> {
        let index = Arc::new(Index::new());

        let mut workspaces = Workspaces::default();
        // Register workspaces with default settings - they'll be initialized with real settings
        // when workspace/configuration response is received
        for uri in workspace_uris {
            workspaces.register(uri)?;
        }

        Ok(Self {
            native_system,
            position_encoding,
            workspaces,
            deferred_messages: VecDeque::new(),
            index: Some(index),
            initialization_options,
            global_settings: Arc::new(GlobalSettings::default()),
            projects: BTreeMap::new(),
            resolved_client_capabilities,
            request_queue: RequestQueue::new(),
            shutdown_requested: false,
            in_test,
            suspended_workspace_diagnostics_request: None,
            revision: 0,
            registrations: HashSet::new(),
            file_watcher_registration_needs_refresh: false,
            client_name,
        })
    }

    pub(crate) fn system(&self) -> &dyn System {
        &*self.native_system
    }

    pub(crate) fn request_queue(&self) -> &RequestQueue {
        &self.request_queue
    }

    pub(crate) fn request_queue_mut(&mut self) -> &mut RequestQueue {
        &mut self.request_queue
    }

    pub(crate) fn initialization_options(&self) -> &InitializationOptions {
        &self.initialization_options
    }

    pub(crate) fn is_shutdown_requested(&self) -> bool {
        self.shutdown_requested
    }

    pub(crate) fn set_shutdown_requested(&mut self, requested: bool) {
        self.shutdown_requested = requested;
    }

    pub(crate) fn set_suspended_workspace_diagnostics_request(
        &mut self,
        request: SuspendedWorkspaceDiagnosticRequest,
        client: &Client,
    ) {
        self.suspended_workspace_diagnostics_request = Some(request);
        // Run the suspended workspace diagnostic request immediately in case there
        // were changes since the workspace diagnostics background thread queued
        // the action to suspend the workspace diagnostic request.
        self.resume_suspended_workspace_diagnostic_request(client);
    }

    pub(crate) fn take_suspended_workspace_diagnostic_request(
        &mut self,
    ) -> Option<SuspendedWorkspaceDiagnosticRequest> {
        self.suspended_workspace_diagnostics_request.take()
    }

    /// Resumes (retries) the workspace diagnostic request if there
    /// were any changes to the [`Session`] (the revision got bumped)
    /// since the workspace diagnostic request ran last time.
    ///
    /// The workspace diagnostic requests is ignored if the request
    /// was cancelled in the meantime.
    pub(crate) fn resume_suspended_workspace_diagnostic_request(&mut self, client: &Client) {
        self.suspended_workspace_diagnostics_request = self
            .suspended_workspace_diagnostics_request
            .take()
            .and_then(|request| {
                if !self.request_queue.incoming().is_pending(&request.id) {
                    // Clear out the suspended request if the request has been cancelled.
                    tracing::debug!("Skipping suspended workspace diagnostics request `{}` because it was cancelled", request.id);
                    return None;
                }

                request.resume_if_revision_changed(self.revision, client)
            });
    }

    /// Bumps the revision.
    ///
    /// The revision is used to track when workspace diagnostics may have changed and need to be re-run.
    /// It's okay if a bump doesn't necessarily result in new workspace diagnostics.
    ///
    /// In general, any change to a project database should bump the revision and so should
    /// any change to the document states (but also when the open workspaces change etc.).
    fn bump_revision(&mut self) {
        self.revision += 1;
    }

    /// The LSP specification doesn't allow configuration requests during initialization,
    /// but we need access to the configuration to resolve the settings in turn to create the
    /// project databases. This will become more important in the future when we support
    /// persistent caching. It's then crucial that we have the correct settings to select the
    /// right cache.
    ///
    /// We work around this by queueing up all messages that arrive between the `initialized` notification
    /// and the completion of workspace initialization (which waits for the client's configuration response).
    ///
    /// This queuing is only necessary when registering *new* workspaces. Changes to configurations
    /// don't need to go through the same process because we can update the existing
    /// database in place.
    ///
    /// See <https://github.com/Microsoft/language-server-protocol/issues/567#issuecomment-2085131917>
    pub(crate) fn should_defer_message(&mut self, message: Message) -> Option<Message> {
        if self.workspaces.all_initialized() {
            Some(message)
        } else {
            match &message {
                Message::Request(request) => {
                    if request.method == ShutdownRequest::METHOD.as_str() {
                        return Some(message);
                    }
                    tracing::debug!(
                        "Deferring `{}` request until all workspaces are initialized",
                        request.method
                    );
                }
                Message::Response(_) => {
                    // We still want to get client responses even during workspace initialization.
                    return Some(message);
                }
                Message::Notification(notification) => {
                    if notification.method == ExitNotification::METHOD.as_str() {
                        return Some(message);
                    }
                    tracing::debug!(
                        "Deferring `{}` notification until all workspaces are initialized",
                        notification.method
                    );
                }
            }

            self.deferred_messages.push_back(message);
            None
        }
    }

    pub(crate) fn workspaces(&self) -> &Workspaces {
        &self.workspaces
    }

    /// Returns a reference to the project's [`ProjectDatabase`] in which the given `path` belongs.
    ///
    /// If the path is a system path, it will prefer the closest Chalk project containing the path,
    /// then the closest workspace project, or the first project if neither contains the path.
    ///
    /// If the path is a virtual path, it will return the first project database in the session.
    pub(crate) fn project_db(&self, path: &AnySystemPath) -> &ProjectDatabase {
        &self.project_state(path).db
    }

    /// Returns an iterator, in arbitrary order, over all project databases
    /// in this session.
    pub(crate) fn project_dbs(&self) -> impl Iterator<Item = &ProjectDatabase> {
        self.projects
            .values()
            .map(|project_state| &project_state.db)
    }

    /// Returns a mutable reference to the project's [`ProjectDatabase`] in which the given `path`
    /// belongs.
    ///
    /// Refer to [`project_db`] for more details on how the project is selected.
    ///
    /// [`project_db`]: Session::project_db
    pub(crate) fn project_db_mut(&mut self, path: &AnySystemPath) -> &mut ProjectDatabase {
        &mut self.project_state_mut(path).db
    }

    /// Returns a reference to the project's [`ProjectState`] in which the given `path` belongs.
    ///
    /// If the path is a system path, it will prefer the closest Chalk project containing the path,
    /// then the closest workspace project, or the first project if neither contains the path.
    ///
    /// If the path is a virtual path, it will return the first project database in the session.
    pub(crate) fn project_state(&self, path: &AnySystemPath) -> &ProjectState {
        match path {
            AnySystemPath::System(system_path) => self
                .project_state_for_path(system_path)
                .unwrap_or_else(|| self.project_state_virtual_fallback()),
            AnySystemPath::SystemVirtual(_virtual_path) => self.project_state_virtual_fallback(),
        }
    }

    /// Returns a mutable reference to the project's [`ProjectState`] in which the given `path`
    /// belongs.
    ///
    /// Refer to [`project_db`] for more details on how the project is selected.
    ///
    /// [`project_db`]: Session::project_db
    pub(crate) fn project_state_mut(&mut self, path: &AnySystemPath) -> &mut ProjectState {
        match path {
            AnySystemPath::System(system_path) => {
                if let Some(routing_root) = self
                    .project_entry_for_path(system_path)
                    .map(|(routing_root, _)| routing_root.clone())
                {
                    return self
                        .projects
                        .get_mut(&routing_root)
                        .expect("selected project must still exist");
                }

                self.project_state_virtual_fallback_mut()
            }
            AnySystemPath::SystemVirtual(_virtual_path) => {
                self.project_state_virtual_fallback_mut()
            }
        }
    }

    /// Returns a reference to the project's [`ProjectState`] corresponding to the given path, if
    /// any.
    pub(crate) fn project_state_for_path(
        &self,
        path: impl AsRef<SystemPath>,
    ) -> Option<&ProjectState> {
        self.project_entry_for_path(path)
            .map(|(_, project)| project)
    }

    fn project_entry_for_path(
        &self,
        path: impl AsRef<SystemPath>,
    ) -> Option<(&SystemPathBuf, &ProjectState)> {
        let path = path.as_ref();
        let mut closest_workspace = None;
        let mut closest_chalk = None;

        for entry @ (routing_root, state) in self.projects.range(..=path.to_path_buf()) {
            if !path.starts_with(routing_root) {
                continue;
            }
            match state.kind() {
                ProjectKind::Workspace => closest_workspace = Some(entry),
                ProjectKind::Chalk => closest_chalk = Some(entry),
            }
        }

        closest_chalk.or(closest_workspace)
    }

    // TODO: While ty supports multiple workspace folders, we still
    // need to figure out which project should this virtual path
    // belong to: https://github.com/astral-sh/ty/issues/794 (e.g.
    // look for the first project with an overlapping search path?)
    fn project_state_virtual_fallback(&self) -> &ProjectState {
        self.projects
            .values()
            .next()
            .expect("To always have at least one project")
    }

    fn project_state_virtual_fallback_mut(&mut self) -> &mut ProjectState {
        self.projects.values_mut().next().unwrap()
    }

    pub(crate) fn apply_changes(&mut self, changes: &[ChangeEvent]) {
        let projects = self.projects_with_overrides();

        self.bump_revision();

        for (routing_root, overrides) in projects {
            let Some(state) = self.projects.get_mut(&routing_root) else {
                continue;
            };
            Self::apply_changes_to_project(state, changes, overrides.as_ref());
        }
    }

    fn projects_with_overrides(&self) -> Vec<(SystemPathBuf, Option<ProjectOptionsOverrides>)> {
        self.projects
            .iter()
            .map(|(routing_root, state)| {
                let overrides = state
                    .workspace_root
                    .as_ref()
                    .and_then(|workspace_root| self.workspaces.workspaces.get(workspace_root))
                    .and_then(|workspace| workspace.settings().project_options_overrides())
                    .cloned();
                (routing_root.clone(), overrides)
            })
            .collect()
    }

    fn apply_changes_to_project(
        state: &mut ProjectState,
        changes: &[ChangeEvent],
        overrides: Option<&ProjectOptionsOverrides>,
    ) {
        state.db.apply_changes(changes, overrides);

        if state.chalk_project.as_ref().is_some_and(|chalk_project| {
            changes
                .iter()
                .any(|change| chalk_project.should_refresh(change))
        }) && let Some(chalk_project) = state.chalk_project.as_mut()
            && let Err(error) = chalk_project.refresh(&mut state.db)
        {
            tracing::error!(
                "Failed to refresh Chalk sources for project at `{}`: {error}",
                chalk_project.project().root()
            );
        }
    }

    /// Applies filesystem changes to every persistent project database.
    ///
    /// Filesystem watcher events can affect shared dependencies and ignore files, so they must not
    /// be routed back through a database's internal metadata root. A Chalk database can inherit a
    /// broader metadata root while remaining keyed by its Chalk routing root.
    pub(crate) fn apply_changes_to_all(&mut self, changes: &[ChangeEvent]) {
        let projects = self.projects_with_overrides();

        self.bump_revision();

        for (routing_root, overrides) in projects {
            if let Some(state) = self.projects.get_mut(&routing_root) {
                Self::apply_changes_to_project(state, changes, overrides.as_ref());
            }
        }
    }

    pub(crate) fn project_routing_roots(&self) -> impl Iterator<Item = &SystemPathBuf> {
        self.projects.keys()
    }

    pub(crate) fn project_db_for_routing_root(
        &self,
        routing_root: &SystemPath,
    ) -> Option<&ProjectDatabase> {
        self.projects.get(routing_root).map(|state| &state.db)
    }

    pub(crate) fn project_state_for_routing_root_mut(
        &mut self,
        routing_root: &SystemPath,
    ) -> Option<&mut ProjectState> {
        self.projects.get_mut(routing_root)
    }

    /// Returns a mutable iterator over all project databases.
    pub(crate) fn projects_mut(&mut self) -> impl Iterator<Item = &'_ mut ProjectDatabase> + '_ {
        self.project_states_mut().map(|project| &mut project.db)
    }

    /// Returns a mutable iterator over all projects.
    pub(crate) fn project_states_mut(&mut self) -> impl Iterator<Item = &'_ mut ProjectState> + '_ {
        self.projects.values_mut()
    }

    /// Initializes a sequence of workspace folders identified by URI
    /// along with its corresponding options.
    ///
    /// This is meant to be called when a response from a
    /// `workspace/configuration` request is received. (This is where
    /// the `ClientOptions` comes from.)
    ///
    /// It is legal to call this on URIs corresponding to workspace
    /// folders that are already initialized. When that occurs,
    /// they are skipped by this routine. That is, they are not
    /// re-initialized.
    ///
    /// The client provided is used to show error messages, publish
    /// diagnostics related to configuration and register capabilities.
    ///
    /// This is typically called when a response to a
    /// `workspace/configuration` request is received.
    pub(crate) fn initialize_workspace_folders(
        &mut self,
        client: &Client,
        workspace_folders: Vec<(Uri, ClientOptions)>,
    ) {
        // Every workspace folder can come with its own
        // global options. In theory, these can have different
        // values. At time of writing (2026-01-28), AG has been
        // unable to make VS Code cause this. This is because
        // the ty VS Code extension scopes its settings to the
        // "window":
        // https://github.com/astral-sh/ty-vscode/blob/e68f26549a920926d8a6bced942dfaf32313f851/package.json#L107
        //
        // So at least in theory, there is a semantic mismatch
        // between the LSP protocol and ty's LSP's understanding
        // of what settings are global and which aren't.
        //
        // We used to try and combine these global options across
        // multiple workspace folders. But when we did that, we
        // didn't actually support multiple workspace folders. We
        // always took the first workspace folder and ignored the
        // rest.
        //
        // In a world where we support multiple workspace folders,
        // we also need to support possibly _adding_ (or removing)
        // new workspace folders dynamically (that's the
        // `workspace/didChangeWorkspaceFolders` notification).
        // This in turn means that global options can change based
        // on new workspace folders being added.
        //
        // Should we try to merge the existing global options with
        // the new ones? What if a workspace folder is removed? Should
        // we try to update our global options based on that?
        //
        // It should be clear that there are many different
        // permutations of possibilities here. Perhaps the best
        // choice is to find a way to get rid of global options
        // entirely and make all settings specific to workspace
        // folders.
        //
        // In any case, our current strategy for now is to just use the
        // most recent global options received (after being combined
        // with the global options received at initialization time).
        // Doing anything more complicated seems unwarranted unless
        // real users are having problems as a result of this.
        //
        // Note that this is a divergence from previous behavior:
        // https://github.com/astral-sh/ruff/pull/19614
        let mut global_options: Option<GlobalOptions> = None;

        for (uri, options) in workspace_folders {
            // Last setting wins.
            global_options = Some(
                self.initialization_options
                    .options
                    .global
                    .clone()
                    .combine(options.global),
            );
            if !options.unknown.is_empty() {
                warn_about_unknown_options(client, Some(&uri), &options.unknown);
            }
            self.initialize_workspace_folder(client, &uri, options.workspace);
        }

        if let Some(global_options) = global_options {
            let global_settings = global_options.into_settings();
            self.global_settings = Arc::new(global_settings);
        }
        if let Some(check_mode) = self.global_settings.diagnostic_mode().to_check_mode() {
            for project in self.projects.values_mut() {
                project.db.set_check_mode(check_mode);
            }
        }

        self.register_capabilities(client);
    }

    /// Initializes a single workspace folder with the given URI
    /// and options.
    ///
    /// If this workspace folder has already been initialized, then
    /// this is a no-op.
    ///
    /// The client provided is used to show error messages and publish
    /// diagnostics related to configuration.
    pub(crate) fn initialize_workspace_folder(
        &mut self,
        client: &Client,
        uri: &Uri,
        options: WorkspaceOptions,
    ) {
        let options = self
            .initialization_options
            .options
            .workspace
            .clone()
            .combine(options);

        tracing::debug!("Initializing workspace `{uri}`: {options:#?}");

        let Ok(root) = uri.to_file_path() else {
            tracing::debug!("Ignoring workspace with non-path root: {uri}");
            return;
        };

        // Realistically I don't think this can fail because we got the path from a Uri
        let root = match SystemPathBuf::from_path_buf(root) {
            Ok(root) => root,
            Err(root) => {
                tracing::debug!(
                    "Ignoring workspace with non-UTF8 root: {root}",
                    root = root.display()
                );
                return;
            }
        };
        let workspace_root = root.clone();

        let settings = options.into_settings(&root, client, &*self.native_system);
        let Some(workspace) = self.workspaces.workspaces.get_mut(&root) else {
            tracing::debug!("Ignoring workspace `{uri}` since it was not registered");
            return;
        };
        if workspace.is_initialized() {
            tracing::debug!(
                "Ignoring workspace initialization for `{uri}` \
                 since it has already been initialized"
            );
            return;
        }
        workspace.initialize(settings);

        // For now, create one project database per workspace.
        // In the future, index the workspace directories to find all projects
        // and create a project database for each.
        let system = LSPSystem::new(
            self.index.as_ref().unwrap().clone(),
            self.native_system.clone(),
        );

        let configuration_file = workspace
            .settings
            .project_options_overrides()
            .and_then(|settings| settings.config_file_override.as_ref());

        let metadata = if let Some(configuration_file) = configuration_file {
            ProjectMetadata::from_config_file(configuration_file.clone(), &root, &system)
        } else {
            ProjectMetadata::discover(&root, &system)
        };

        let project = metadata
            .context("Failed to discover project configuration")
            .and_then(|mut metadata| {
                metadata
                    .apply_configuration_files(&system)
                    .context("Failed to apply configuration files")?;

                if let Some(overrides) = workspace.settings.project_options_overrides() {
                    metadata.apply_overrides(overrides);
                }

                ProjectDatabase::fallible(metadata, system.clone())
            });

        let (root, db) = match project {
            Ok(db) => (root, db),
            Err(err) => {
                tracing::error!(
                    "Failed to create project for workspace `{uri}`: {err:#}. \
                        Falling back to default settings"
                );

                client.show_error_message(format!(
                    "Failed to load project for workspace {uri}. {}",
                    self.client_name.log_guidance(),
                ));

                let Ok(metadata) = ProjectMetadata::from_options(
                    Options::default(),
                    root,
                    None,
                    &UseDefaultStrategy,
                );
                let db_with_default_settings = ProjectDatabase::use_defaults(metadata, system);
                let default_root = db_with_default_settings
                    .project()
                    .root(&db_with_default_settings)
                    .to_path_buf();

                (default_root, db_with_default_settings)
            }
        };

        // Carry forward diagnostic state if any exists
        let previous = self.projects.remove(&root);
        let untracked = previous
            .map(|state| state.untracked_files_with_pushed_diagnostics)
            .unwrap_or_default();
        self.projects.insert(
            root.clone(),
            ProjectState {
                kind: ProjectKind::Workspace,
                workspace_root: Some(workspace_root),
                chalk_project: None,
                db,
                untracked_files_with_pushed_diagnostics: untracked,
            },
        );

        publish_settings_diagnostics(self, client, &root);
    }

    /// Adds an uninitialized workspace to this session.
    ///
    /// This returns `true` when this workspace is added and `false`
    /// when it has already been added.
    ///
    /// If there was a problem adding the workspace folder (e.g., the
    /// path derived from the given URI is not valid UTF-8), then an
    /// error is returned and no workspace folder is registered.
    ///
    /// To initialize the workspace folder, callers must initiate
    /// a request for workspace folder configuration via
    /// `Session::request_uninitialized_workspace_folder_configuration`.
    pub(crate) fn register_workspace_folder(&mut self, uri: Uri) -> anyhow::Result<bool> {
        self.workspaces.register(uri)
    }

    /// Requests configuration for each registered but uninitialized
    /// workspace folder in this session.
    ///
    /// When all workspace folders in this session are initialized, then
    /// this is a no-op.
    ///
    /// Each uninitialized workspace folder will be fully initialized
    /// once the configuration response is received (asynchronously).
    /// When the client doesn't support requesting workspace
    /// configuration, the workspace folder is initialized immediately
    /// using the options this session was initialized with.
    ///
    /// Adding an uninitialized workspace to this session can be done
    /// with `Session::register_workspace_folder`.
    pub(crate) fn request_uninitialized_workspace_folder_configurations(
        &mut self,
        client: &Client,
    ) {
        // When all workspaces are already initialized, then
        // there's nothing to do.
        if self.workspaces().all_initialized() {
            return;
        }

        let uninit_workspace_uris: Vec<Uri> = self
            .workspaces()
            .into_iter()
            .filter_map(|(_, workspace)| {
                if workspace.is_initialized() {
                    None
                } else {
                    Some(workspace.uri().clone())
                }
            })
            .collect();

        if !self
            .client_capabilities()
            .supports_workspace_configuration()
        {
            tracing::info!(
                "Client does not support workspace configuration, initializing workspaces \
                 using the initialization options"
            );
            self.initialize_workspace_folders(
                client,
                uninit_workspace_uris
                    .into_iter()
                    .map(|uri| (uri, self.initialization_options().options.clone()))
                    .collect::<Vec<_>>(),
            );
            return;
        }

        let items: Vec<lsp_types::ConfigurationItem> = uninit_workspace_uris
            .iter()
            .map(|uri| lsp_types::ConfigurationItem {
                scope_uri: Some(uri.clone()),
                section: Some("ty".to_string()),
            })
            .collect();

        tracing::debug!("Requesting workspace configuration for workspaces");
        client.send_request::<lsp_types::ConfigurationRequest>(
            self,
            lsp_types::ConfigurationParams { items },
            move |client, result: Vec<serde_json::Value>| {
                tracing::debug!("Received workspace configurations, initializing workspaces");

                // This shouldn't fail because, as per the spec, the client needs to provide a
                // `null` value even if it cannot provide a configuration for a workspace.
                assert_eq!(
                    result.len(),
                    uninit_workspace_uris.len(),
                    "Mismatch in number of workspace URIs ({}) and configuration results ({})",
                    uninit_workspace_uris.len(),
                    result.len()
                );

                let workspaces_with_options: Vec<_> = uninit_workspace_uris
                    .into_iter()
                    .zip(result)
                    .map(|(uri, value)| {
                        if value.is_null() {
                            tracing::debug!(
                                "No workspace options provided for {uri}, using default options"
                            );
                            return (uri, ClientOptions::default());
                        }
                        let options: ClientOptions =
                            serde_json::from_value(value).unwrap_or_else(|err| {
                                tracing::error!(
                                    "Failed to deserialize workspace options for {uri}: {err}. \
                                        Using default options"
                                );
                                ClientOptions::default()
                            });
                        (uri, options)
                    })
                    .collect();

                client.queue_action(Action::InitializeWorkspaces(workspaces_with_options));
            },
        );
    }

    /// Removes a workspace folder at the given URI.
    ///
    /// This removes the workspace folder and its associated project database,
    /// and clears diagnostics for any documents that were in the workspace.
    ///
    /// # Errors
    ///
    /// This returns an error if the workspace folder has already been removed
    /// or otherwise could not be found.
    pub(crate) fn remove_workspace_folder(
        &mut self,
        client: &Client,
        uri: &Uri,
    ) -> anyhow::Result<()> {
        tracing::info!("Removing workspace folder: {uri}");

        let path = uri
            .to_file_path()
            .map_err(|()| anyhow!("Workspace URI is not a file path: {uri}"))?;
        let workspace_path = SystemPathBuf::from_path_buf(path)
            .map_err(|path| anyhow!("Workspace path is not valid UTF-8: {}", path.display()))?;

        anyhow::ensure!(
            self.workspaces.unregister(&workspace_path),
            "Workspace not found: {uri}",
        );

        // Note that it is somewhat unclear whether we actually need to
        // clear diagnostics here. It seems that, at least in the case
        // of VS Code, it will auto-clear any diagnostics not found in
        // the workspace diagnostic response. Moreover, VS Code will
        // re-request workspace diagnostics after removing a workspace
        // folder.
        //
        // For now, we keep unconditionally clearing diagnostics on
        // opened text documents for reasons of good sense, but it's
        // possible that we don't even need to do that (when workspace
        // diagnostics are enabled).
        //
        // See: https://github.com/astral-sh/ruff/pull/22953#discussion_r2745255350

        // Remove all project databases owned by this workspace, including lazily discovered Chalk
        // projects whose routing roots can differ from the workspace root.
        let owned_project_roots: Vec<_> = self
            .projects
            .iter()
            .filter(|(_, state)| state.workspace_root.as_deref() == Some(workspace_path.as_path()))
            .map(|(routing_root, _)| routing_root.clone())
            .collect();
        if !owned_project_roots.is_empty() {
            self.file_watcher_registration_needs_refresh = true;
        }
        for routing_root in owned_project_roots {
            let Some(project_state) = self.projects.remove(&routing_root) else {
                continue;
            };

            // Clear diagnostics for any files that had pushed diagnostics in this project.
            for file_uri in project_state.untracked_files_with_pushed_diagnostics {
                self.clear_diagnostics(client, &file_uri);
            }
        }

        // Collect all of the documents to clear upfront to
        // work around borrowck.
        let documents_to_clear: Vec<DocumentHandle> = self
            .text_document_handles()
            .filter_map(|doc| {
                if let AnySystemPath::System(ref path) = *doc.notebook_or_file_path()
                    && path.starts_with(&workspace_path)
                {
                    Some(doc)
                } else {
                    None
                }
            })
            .collect();
        for doc in documents_to_clear {
            self.clear_diagnostics_if_needed(&doc, client);
        }

        self.bump_revision();

        Ok(())
    }

    pub(crate) fn clear_diagnostics_if_needed(&self, document: &DocumentHandle, client: &Client) {
        if self.client_capabilities().supports_pull_diagnostics() && !document.is_cell_or_notebook()
        {
            return;
        }
        self.clear_diagnostics(client, document.uri());
    }

    /// Clears the diagnostics for the document identified by `uri`.
    ///
    /// This is done by notifying the client with an empty list of diagnostics for the document.
    /// For notebook cells, this clears diagnostics for the specific cell.
    /// For other document types, this clears diagnostics for the main document.
    pub(crate) fn clear_diagnostics(&self, client: &Client, uri: &Uri) {
        if self.global_settings().diagnostic_mode().is_off() {
            return;
        }
        client.send_notification::<lsp_types::PublishDiagnosticsNotification>(
            lsp_types::PublishDiagnosticsParams {
                uri: uri.clone(),
                diagnostics: vec![],
                version: None,
            },
        );
    }

    pub(crate) fn take_deferred_messages(&mut self) -> Option<Message> {
        if self.workspaces.all_initialized() {
            self.deferred_messages.pop_front()
        } else {
            None
        }
    }

    /// Registers the dynamic capabilities with the client as per the resolved global settings.
    ///
    /// ## Diagnostic capability
    ///
    /// This capability is used to enable / disable workspace diagnostics as per the
    /// `ty.diagnosticMode` global setting.
    ///
    /// ## Rename capability
    ///
    /// This capability is used to enable / disable rename functionality as per the
    /// `ty.experimental.rename` global setting.
    fn register_capabilities(&mut self, client: &Client) {
        static DIAGNOSTIC_REGISTRATION_ID: &str = "ty/textDocument/diagnostic";

        let mut registrations = vec![];
        let mut unregistrations = vec![];

        if self
            .resolved_client_capabilities
            .supports_diagnostic_dynamic_registration()
        {
            if self
                .registrations
                .contains(DocumentDiagnosticRequest::METHOD.as_str())
            {
                unregistrations.push(Unregistration {
                    id: DIAGNOSTIC_REGISTRATION_ID.into(),
                    method: DocumentDiagnosticRequest::METHOD.into(),
                });
            }

            let diagnostic_mode = self.global_settings.diagnostic_mode;

            match diagnostic_mode {
                DiagnosticMode::Off => {
                    tracing::debug!(
                        "Skipping registration of diagnostic capability because diagnostics are turned off"
                    );
                }
                DiagnosticMode::OpenFilesOnly | DiagnosticMode::Workspace => {
                    tracing::debug!(
                        "Registering diagnostic capability with {diagnostic_mode:?} diagnostic mode"
                    );
                    registrations.push(Registration {
                        id: DIAGNOSTIC_REGISTRATION_ID.into(),
                        method: DocumentDiagnosticRequest::METHOD.into(),
                        register_options: Some(
                            serde_json::to_value(
                                DiagnosticProvider::DiagnosticRegistrationOptions(
                                    DiagnosticRegistrationOptions {
                                        diagnostic_options: server_diagnostic_options(
                                            diagnostic_mode.is_workspace(),
                                        ),
                                        ..Default::default()
                                    },
                                ),
                            )
                            .unwrap(),
                        ),
                    });
                }
            }
        }

        self.append_file_watcher_registration_changes(&mut registrations, &mut unregistrations);
        self.file_watcher_registration_needs_refresh = false;

        // First, unregister any existing capabilities and then register or re-register them.
        self.unregister_dynamic_capability(client, unregistrations);
        self.register_dynamic_capability(client, registrations);
    }

    /// Refreshes file watching after a persistent project was added lazily.
    pub(crate) fn refresh_file_watcher_registration_if_needed(&mut self, client: &Client) {
        if !std::mem::take(&mut self.file_watcher_registration_needs_refresh) {
            return;
        }

        let mut registrations = vec![];
        let mut unregistrations = vec![];
        self.append_file_watcher_registration_changes(&mut registrations, &mut unregistrations);
        self.unregister_dynamic_capability(client, unregistrations);
        self.register_dynamic_capability(client, registrations);
    }

    fn append_file_watcher_registration_changes(
        &self,
        registrations: &mut Vec<Registration>,
        unregistrations: &mut Vec<Unregistration>,
    ) {
        let Some(register_options) = self.file_watcher_registration_options() else {
            return;
        };

        if self
            .registrations
            .contains(DidChangeWatchedFilesNotification::METHOD.as_str())
        {
            unregistrations.push(Unregistration {
                id: FILE_WATCHER_REGISTRATION_ID.into(),
                method: DidChangeWatchedFilesNotification::METHOD.into(),
            });
        }
        registrations.push(Registration {
            id: FILE_WATCHER_REGISTRATION_ID.into(),
            method: DidChangeWatchedFilesNotification::METHOD.into(),
            register_options: Some(serde_json::to_value(register_options).unwrap()),
        });
    }

    /// Registers a list of dynamic capabilities with the client.
    fn register_dynamic_capability(&mut self, client: &Client, registrations: Vec<Registration>) {
        if registrations.is_empty() {
            return;
        }

        for registration in &registrations {
            self.registrations.insert(registration.method.clone());
        }

        client.send_request::<RegistrationRequest>(
            self,
            RegistrationParams { registrations },
            |_: &Client, ()| {
                tracing::debug!("Registered dynamic capabilities");
            },
        );
    }

    /// Unregisters a list of dynamic capabilities with the client.
    fn unregister_dynamic_capability(
        &mut self,
        client: &Client,
        unregistrations: Vec<Unregistration>,
    ) {
        if unregistrations.is_empty() {
            return;
        }

        for unregistration in &unregistrations {
            if !self.registrations.remove(&unregistration.method) {
                tracing::debug!(
                    "Unregistration for `{}` was requested, but it was not registered",
                    unregistration.method
                );
            }
        }

        client.send_request::<UnregistrationRequest>(
            self,
            UnregistrationParams {
                unregisterations: unregistrations,
            },
            |_: &Client, ()| {
                tracing::debug!("Unregistered dynamic capabilities");
            },
        );
    }

    /// Try to register the file watcher provided by the client if the client supports it.
    ///
    /// Note that this should be called *after* workspaces/projects have been initialized.
    /// This is required because the globs we use for registering file watching take
    /// project search paths into account.
    fn file_watcher_registration_options(
        &self,
    ) -> Option<DidChangeWatchedFilesRegistrationOptions> {
        fn make_watcher(glob: &str) -> FileSystemWatcher {
            FileSystemWatcher {
                glob_pattern: lsp_types::GlobPattern::Pattern(glob.into()),
                // When `kind` is omitted, it defaults to `WatchKind.Create | WatchKind.Change | WatchKind.Delete`.
                // https://microsoft.github.io/language-server-protocol/specifications/lsp/3.18/specification/#fileSystemWatcher
                kind: None,
            }
        }

        fn make_relative_watcher(relative_to: &SystemPath, glob: &str) -> FileSystemWatcher {
            let base_uri = Uri::from_file_path(relative_to.as_std_path())
                .expect("system path must be a valid URI");
            let glob_pattern =
                lsp_types::GlobPattern::RelativePattern(lsp_types::RelativePattern {
                    base_uri: base_uri.into(),
                    pattern: glob.to_string(),
                });
            FileSystemWatcher {
                glob_pattern,
                kind: Some(
                    lsp_types::WatchKind::Change
                        | lsp_types::WatchKind::Delete
                        | lsp_types::WatchKind::Create,
                ),
            }
        }

        if !self.client_capabilities().supports_file_watcher() {
            tracing::warn!(
                "Your LSP client doesn't support file watching: \
                 You may see stale results when files change outside the editor"
            );
            return None;
        }

        // We also want to watch everything in the search paths as
        // well. But this seems to require "relative" watcher support.
        // I had trouble getting this working without using a base uri.
        //
        // Specifically, I tried this for each search path:
        //
        //     make_watcher(&format!("{path}/**"))
        //
        // But while this seemed to work for the project root, it
        // simply wouldn't result in any file notifications for changes
        // to files outside of the project root.
        let watchers = if !self.client_capabilities().supports_relative_file_watcher() {
            tracing::warn!(
                "Your LSP client doesn't support file watching outside of project: \
                 You may see stale results when dependencies change"
            );
            // Initialize our list of watchers with the standard globs relative
            // to the project root if we can't use relative globs.
            vec![make_watcher("**")]
        } else {
            // Gather up all of our project roots and all of the corresponding
            // project root system paths, then deduplicate them relative to
            // one another. Then listen to everything.
            let roots = self.project_dbs().map(|db| db.project().root(db));
            let paths = self
                .project_dbs()
                .flat_map(|db| {
                    ty_module_resolver::system_module_search_paths(db).map(move |path| (db, path))
                })
                .filter(|(db, path)| !path.starts_with(db.project().root(*db)))
                .map(|(_, path)| path)
                .chain(roots);
            ruff_db::system::deduplicate_nested_paths(paths)
                .map(|path| make_relative_watcher(path, "**"))
                .collect()
        };
        Some(DidChangeWatchedFilesRegistrationOptions { watchers })
    }

    /// Creates a document snapshot with the URI referencing the document to snapshot.
    pub(crate) fn snapshot_document(&self, uri: &Uri) -> Result<DocumentSnapshot, DocumentError> {
        let index = self.index();
        let document_handle = index.document_handle(uri)?;

        Ok(DocumentSnapshot {
            resolved_client_capabilities: self.resolved_client_capabilities,
            global_settings: self.global_settings.clone(),
            workspace_settings: self
                .workspace_settings_for_document(document_handle.notebook_or_file_path())
                .unwrap_or_else(|| Arc::new(WorkspaceSettings::default())),
            position_encoding: self.position_encoding,
            chalk_project: self
                .project_state(document_handle.notebook_or_file_path())
                .chalk_project
                .as_ref()
                .map(ActiveChalkProject::input),
            document: document_handle,
            client_name: self.client_name,
        })
    }

    fn workspace_settings_for_document(
        &self,
        path: &AnySystemPath,
    ) -> Option<Arc<WorkspaceSettings>> {
        // Virtual documents use the same "owner" heuristic as `project_state`.
        match path {
            AnySystemPath::System(system_path) => self.workspaces.settings_for_path(system_path),
            AnySystemPath::SystemVirtual(_) => {
                let project = self.project_state(path);
                self.workspaces
                    .settings_for_path(project.db.project().root(&project.db))
                    .or_else(|| self.workspaces.settings_virtual_fallback())
            }
        }
    }

    /// Creates a snapshot of the current state of the [`Session`].
    pub(crate) fn snapshot_session(&self) -> SessionSnapshot {
        SessionSnapshot {
            projects: self
                .projects
                .iter()
                .map(|(routing_root, project)| RoutedProject {
                    routing_root: routing_root.clone(),
                    workspace_root: project.workspace_root.clone(),
                    kind: project.kind(),
                    db: project.db.clone(),
                })
                .collect(),
            index: self.index.clone().unwrap(),
            global_settings: self.global_settings.clone(),
            position_encoding: self.position_encoding,
            in_test: self.in_test,
            resolved_client_capabilities: self.resolved_client_capabilities,
            revision: self.revision,
            client_name: self.client_name,
        }
    }

    /// Iterates over the document keys for all open text documents.
    pub(super) fn text_document_handles(&self) -> impl Iterator<Item = DocumentHandle> + '_ {
        self.index()
            .text_documents()
            .map(|(_, document)| DocumentHandle::from_text_document(document))
    }

    /// Iterates over all open file-level documents.
    ///
    /// Notebook cells are excluded because their file-level representation is the containing
    /// notebook.
    pub(super) fn file_document_handles(&self) -> impl Iterator<Item = DocumentHandle> + '_ {
        self.index()
            .file_documents()
            .map(DocumentHandle::from_document)
    }

    /// Returns a handle to the document specified by its URI.
    ///
    /// # Errors
    ///
    /// If the document is not found.
    pub(crate) fn document_handle(
        &self,
        uri: &lsp_types::Uri,
    ) -> Result<DocumentHandle, DocumentError> {
        self.index().document_handle(uri)
    }

    /// Registers a notebook document at the provided `path`.
    /// If a document is already open here, it will be overwritten.
    ///
    /// Returns a handle to the opened document.
    pub(crate) fn open_notebook_document(&mut self, document: NotebookDocument) -> DocumentHandle {
        let handle = self.index_mut().open_notebook_document(document);
        self.ensure_chalk_project_for_document(&handle, None);
        self.open_document_in_db(&handle, None);
        handle
    }

    /// Registers a text document at the provided `path`.
    /// If a document is already open here, it will be overwritten.
    ///
    /// Returns a handle to the opened document.
    pub(crate) fn open_text_document(&mut self, document: TextDocument) -> DocumentHandle {
        let language_id = document.language_id();
        let handle = self.index_mut().open_text_document(document);
        self.ensure_chalk_project_for_document(&handle, Some(language_id));
        self.open_document_in_db(&handle, Some(language_id));
        handle
    }

    fn ensure_chalk_project_for_document(
        &mut self,
        document: &DocumentHandle,
        language_id: Option<LanguageId>,
    ) {
        let is_chalk_sql = document
            .notebook_or_file_path()
            .as_system()
            .is_some_and(|path| is_chalk_sql_path(path));
        if matches!(language_id, Some(LanguageId::Other)) && !is_chalk_sql {
            return;
        }

        let AnySystemPath::System(system_path) = document.notebook_or_file_path() else {
            return;
        };

        // Once a file is actively routed to a Chalk project, marker changes do not affect that
        // routing for the remainder of the session.
        if self
            .project_state_for_path(system_path)
            .is_some_and(|state| state.kind() == ProjectKind::Chalk)
        {
            return;
        }

        let Some(chalk_project) = discover_chalk_project(
            self.project_db(document.notebook_or_file_path()).system(),
            system_path,
        ) else {
            return;
        };
        let routing_root = chalk_project.root().to_path_buf();

        if self
            .projects
            .get(&routing_root)
            .is_some_and(|state| state.kind() == ProjectKind::Chalk)
        {
            return;
        }

        let migrated_documents = self.open_documents_migrating_to(&routing_root);
        if let Some(state) = self.projects.get_mut(&routing_root)
            && state.kind() == ProjectKind::Workspace
        {
            let active_project = match Self::activate_chalk_project(
                &mut state.db,
                chalk_project,
                &migrated_documents,
            ) {
                Ok(active_project) => active_project,
                Err(error) => {
                    tracing::error!(
                        "Failed to collect Chalk sources for project at `{routing_root}`: {error}"
                    );
                    return;
                }
            };
            state.kind = ProjectKind::Chalk;
            state.chalk_project = Some(active_project);
            self.file_watcher_registration_needs_refresh = true;
            return;
        }

        let workspace_root = self.workspaces.root_for_path(system_path).cloned();
        let Some(mut db) =
            self.create_chalk_project_database(&chalk_project, workspace_root.as_deref())
        else {
            return;
        };

        for (_, migrated_document, is_python) in &migrated_documents {
            if *is_python && migrated_document.key() != document.key() {
                Self::open_document_in_project(&mut db, migrated_document);
            }
        }
        let chalk_project =
            match Self::activate_chalk_project(&mut db, chalk_project, &migrated_documents) {
                Ok(chalk_project) => chalk_project,
                Err(error) => {
                    tracing::error!(
                        "Failed to collect Chalk sources for project at `{routing_root}`: {error}"
                    );
                    return;
                }
            };

        for (old_routing_root, migrated_document, is_python) in &migrated_documents {
            if *is_python && let Some(state) = self.projects.get_mut(old_routing_root) {
                Self::close_document_in_project(&mut state.db, migrated_document);
            }
        }

        let previous = self.projects.remove(&routing_root);
        let untracked_files_with_pushed_diagnostics = previous
            .map(|state| state.untracked_files_with_pushed_diagnostics)
            .unwrap_or_default();

        self.projects.insert(
            routing_root,
            ProjectState {
                kind: ProjectKind::Chalk,
                workspace_root,
                untracked_files_with_pushed_diagnostics,
                chalk_project: Some(chalk_project),
                db,
            },
        );
        self.file_watcher_registration_needs_refresh = true;
    }

    fn activate_chalk_project(
        db: &mut ProjectDatabase,
        chalk_project: ChalkProject,
        documents: &[(SystemPathBuf, DocumentHandle, bool)],
    ) -> Result<ActiveChalkProject, ChalkProjectError> {
        let mut chalk_project = ActiveChalkProject::new(db, chalk_project)?;
        for (_, document, _) in documents {
            if let AnySystemPath::System(path) = document.notebook_or_file_path()
                && let Ok(file) = system_path_to_file(db, path)
            {
                chalk_project.open_source(db, file);
            }
        }
        chalk_project.refresh(db)?;
        Ok(chalk_project)
    }

    fn create_chalk_project_database(
        &self,
        chalk_project: &ChalkProject,
        workspace_root: Option<&SystemPath>,
    ) -> Option<ProjectDatabase> {
        let root = chalk_project.root();
        let index = self.index.as_ref()?.clone();
        let system = LSPSystem::new(index, self.native_system.clone());
        let workspace_settings = workspace_root
            .and_then(|workspace_root| self.workspaces.workspaces.get(workspace_root))
            .map(Workspace::settings);
        let configuration_file = workspace_settings
            .and_then(WorkspaceSettings::project_options_overrides)
            .and_then(|overrides| overrides.config_file_override.as_ref());

        let project = if let Some(configuration_file) = configuration_file {
            ProjectMetadata::from_config_file(configuration_file.clone(), root, &system)
        } else {
            ProjectMetadata::discover(root, &system)
        }
        .context("Failed to discover Chalk project configuration")
        .and_then(|mut metadata| {
            metadata
                .apply_configuration_files(&system)
                .context("Failed to apply configuration files")?;

            if let Some(overrides) =
                workspace_settings.and_then(WorkspaceSettings::project_options_overrides)
            {
                metadata.apply_overrides(overrides);
            }

            ProjectDatabase::fallible(metadata, system.clone())
        });

        let mut db = match project {
            Ok(db) => db,
            Err(error) => {
                tracing::error!(
                    "Failed to create Chalk project at `{root}`: {error:#}. \
                     Falling back to default settings"
                );

                let Ok(mut metadata) = ProjectMetadata::from_options(
                    Options::default(),
                    root.to_path_buf(),
                    None,
                    &UseDefaultStrategy,
                );
                if let Some(overrides) =
                    workspace_settings.and_then(WorkspaceSettings::project_options_overrides)
                {
                    metadata.apply_overrides(overrides);
                }
                ProjectDatabase::use_defaults(metadata, system)
            }
        };

        if let Some(check_mode) = self.global_settings.diagnostic_mode().to_check_mode() {
            db.set_check_mode(check_mode);
        }

        Some(db)
    }

    fn open_documents_migrating_to(
        &self,
        chalk_root: &SystemPath,
    ) -> Vec<(SystemPathBuf, DocumentHandle, bool)> {
        self.index()
            .file_documents()
            .filter_map(|document| {
                let is_python = document.language_id() != Some(LanguageId::Other);
                let handle = DocumentHandle::from_document(document);
                if !is_python
                    && !handle
                        .notebook_or_file_path()
                        .as_system()
                        .is_some_and(|path| is_chalk_sql_path(path))
                {
                    return None;
                }
                Some((handle, is_python))
            })
            .filter_map(|(document, is_python)| {
                let AnySystemPath::System(path) = document.notebook_or_file_path() else {
                    return None;
                };
                if !path.starts_with(chalk_root) {
                    return None;
                }

                let (old_routing_root, state) = self.project_entry_for_path(path)?;
                if state.kind() == ProjectKind::Chalk {
                    return None;
                }

                Some((old_routing_root.clone(), document, is_python))
            })
            .collect()
    }

    fn close_document_in_project(db: &mut ProjectDatabase, document: &DocumentHandle) {
        let AnySystemPath::System(path) = document.notebook_or_file_path() else {
            return;
        };
        if let Some(file) = db.files().try_system(db, path) {
            db.project().close_file(db, file);
        }
    }

    fn open_document_in_project(db: &mut ProjectDatabase, document: &DocumentHandle) {
        let AnySystemPath::System(path) = document.notebook_or_file_path() else {
            return;
        };
        let Ok(file) = system_path_to_file(db, path) else {
            tracing::warn!("Failed to migrate open file {path} into Chalk project");
            return;
        };
        let project = db.project();
        if project.is_file_included(db, path).is_included() {
            project.open_file(db, file);
        }
    }

    fn open_document_in_db(&mut self, document: &DocumentHandle, language_id: Option<LanguageId>) {
        let path = document.notebook_or_file_path();

        // This is a "maybe" because the `File` might've not been interned yet i.e., the
        // `try_system` call will return `None` which doesn't mean that the file is new, it's just
        // that the server didn't need the file yet.
        let is_maybe_new_system_file = path.as_system().is_some_and(|system_path| {
            let db = self.project_db(path);
            db.files().try_system(db, system_path).map_or_else(
                || !self.native_system.is_file(system_path),
                |file| !file.exists(db),
            )
        });

        // When we know the document isn't a Python source file
        // then we'll avoid adding it to the project. (But we
        // still track it as part of the index.)
        let is_not_python = matches!(language_id, Some(LanguageId::Other));

        match path {
            AnySystemPath::System(system_path) => {
                let state = self.project_state_mut(path);
                if let Ok(file) = system_path_to_file(&state.db, system_path)
                    && let Some(chalk_project) = state.chalk_project.as_mut()
                {
                    chalk_project.open_source(&state.db, file);
                }

                let event = if is_maybe_new_system_file {
                    ChangeEvent::Created {
                        path: system_path.clone(),
                        kind: CreatedKind::File,
                    }
                } else {
                    ChangeEvent::Opened(system_path.clone())
                };
                self.apply_changes(&[event]);

                if is_not_python {
                    return;
                }

                let db = self.project_db_mut(path);
                match system_path_to_file(db, system_path) {
                    Ok(file) => {
                        let project = db.project();

                        // Only mark this file as open if it's part of the project.
                        // This ensures that we don't show diagnostics for files outside the project.
                        if project.is_file_included(db, system_path).is_included() {
                            project.open_file(db, file);
                        }
                    }
                    Err(err) => tracing::warn!("Failed to open file {system_path}: {err}"),
                }
            }
            AnySystemPath::SystemVirtual(virtual_path) => {
                if is_not_python {
                    return;
                }

                let db = self.project_db_mut(path);
                let virtual_file = db.files().virtual_file(db, virtual_path);
                db.project().open_file(db, virtual_file.file());
            }
        }

        self.bump_revision();
    }

    /// Returns a reference to the index.
    ///
    /// # Panics
    ///
    /// Panics if there's a mutable reference to the index via [`index_mut`].
    ///
    /// [`index_mut`]: Session::index_mut
    fn index(&self) -> &Index {
        self.index.as_ref().unwrap()
    }

    /// Returns a mutable reference to the index.
    ///
    /// This method drops all references to the index and returns a guard that will restore the
    /// references when dropped. This guard holds the only reference to the index and allows
    /// modifying it.
    fn index_mut(&mut self) -> MutIndexGuard<'_> {
        let index = self.index.take().unwrap();

        for db in self.projects_mut() {
            // Remove the `index` from each database. This drops the count of `Arc<Index>` down to 1
            db.system_mut()
                .as_any_mut()
                .downcast_mut::<LSPSystem>()
                .unwrap()
                .take_index();
        }

        // There should now be exactly one reference to index which is self.index.
        let index = Arc::into_inner(index).unwrap();

        MutIndexGuard {
            session: self,
            index: Some(index),
        }
    }

    pub(crate) fn client_capabilities(&self) -> ResolvedClientCapabilities {
        self.resolved_client_capabilities
    }

    pub(crate) fn global_settings(&self) -> &GlobalSettings {
        &self.global_settings
    }

    pub(crate) fn position_encoding(&self) -> PositionEncoding {
        self.position_encoding
    }

    pub(crate) fn client_name(&self) -> ClientName {
        self.client_name
    }
}

/// A guard that holds the only reference to the index and allows modifying it.
///
/// When dropped, this guard restores all references to the index.
struct MutIndexGuard<'a> {
    session: &'a mut Session,
    index: Option<Index>,
}

impl Deref for MutIndexGuard<'_> {
    type Target = Index;

    fn deref(&self) -> &Self::Target {
        self.index.as_ref().unwrap()
    }
}

impl DerefMut for MutIndexGuard<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.index.as_mut().unwrap()
    }
}

impl Drop for MutIndexGuard<'_> {
    fn drop(&mut self) {
        if let Some(index) = self.index.take() {
            let index = Arc::new(index);
            for db in self.session.projects_mut() {
                db.system_mut()
                    .as_any_mut()
                    .downcast_mut::<LSPSystem>()
                    .unwrap()
                    .set_index(index.clone());
            }

            self.session.index = Some(index);
        }
    }
}

/// An immutable snapshot of [`Session`] that references a specific document.
#[derive(Debug)]
pub(crate) struct DocumentSnapshot {
    resolved_client_capabilities: ResolvedClientCapabilities,
    global_settings: Arc<GlobalSettings>,
    workspace_settings: Arc<WorkspaceSettings>,
    position_encoding: PositionEncoding,
    chalk_project: Option<ChalkProjectInput>,
    document: DocumentHandle,
    client_name: ClientName,
}

impl DocumentSnapshot {
    /// Returns the resolved client capabilities that were captured during initialization.
    pub(crate) fn resolved_client_capabilities(&self) -> ResolvedClientCapabilities {
        self.resolved_client_capabilities
    }

    /// Returns the position encoding that was negotiated during initialization.
    pub(crate) fn encoding(&self) -> PositionEncoding {
        self.position_encoding
    }

    /// Returns the client settings for all workspaces.
    pub(crate) fn global_settings(&self) -> &GlobalSettings {
        &self.global_settings
    }

    /// Returns the client settings for the workspace that this document belongs to.
    pub(crate) fn workspace_settings(&self) -> &WorkspaceSettings {
        &self.workspace_settings
    }

    pub(crate) fn chalk_project(&self) -> Option<ChalkProjectInput> {
        self.chalk_project
    }

    /// Returns the result of the document query for this snapshot.
    pub(crate) fn document(&self) -> &DocumentHandle {
        &self.document
    }

    pub(crate) fn uri(&self) -> &lsp_types::Uri {
        self.document.uri()
    }

    pub(crate) fn to_notebook_or_file(&self, db: &dyn Db) -> Option<File> {
        let file = self.document.notebook_or_file(db);
        if file.is_none() {
            tracing::debug!(
                "Failed to resolve file: file not found for `{}`",
                self.document.uri()
            );
        }
        file
    }

    pub(crate) fn notebook_or_file_path(&self) -> &AnySystemPath {
        self.document.notebook_or_file_path()
    }

    pub(crate) fn client_name(&self) -> ClientName {
        self.client_name
    }
}

pub(crate) struct RoutedProject {
    routing_root: SystemPathBuf,
    workspace_root: Option<SystemPathBuf>,
    kind: ProjectKind,

    // Keep the database last for the same drop-order reason as `SessionSnapshot::projects`.
    db: ProjectDatabase,
}

impl RoutedProject {
    pub(crate) fn routing_root(&self) -> &SystemPath {
        &self.routing_root
    }

    pub(crate) fn workspace_root(&self) -> Option<&SystemPath> {
        self.workspace_root.as_deref()
    }

    pub(crate) fn kind(&self) -> ProjectKind {
        self.kind
    }

    pub(crate) fn db(&self) -> &ProjectDatabase {
        &self.db
    }
}

/// An immutable snapshot of the current state of [`Session`].
pub(crate) struct SessionSnapshot {
    index: Arc<Index>,
    global_settings: Arc<GlobalSettings>,
    position_encoding: PositionEncoding,
    resolved_client_capabilities: ResolvedClientCapabilities,
    in_test: bool,
    revision: u64,
    client_name: ClientName,

    /// IMPORTANT: It's important that the databases come last, or at least,
    /// after any `Arc` that we try to extract or mutate in-place using `Arc::into_inner`
    /// and that relies on Salsa's cancellation to guarantee that there's now only a
    /// single reference to it (e.g. see [`Session::index_mut`]).
    ///
    /// Making this field come last guarantees that the db's `Drop` handler is
    /// dropped after all other fields, which ensures that
    /// Salsa's cancellation blocks until all fields are dropped (and not only
    /// waits for the db to be dropped while we still hold on to the `Index`).
    projects: Vec<RoutedProject>,
}

impl SessionSnapshot {
    pub(crate) fn routed_projects(&self) -> &[RoutedProject] {
        &self.projects
    }

    pub(crate) fn project_owns_file(&self, project: &RoutedProject, file: File) -> bool {
        let path = file.path(project.db());
        let Some(path) = path.as_system_path() else {
            return true;
        };

        match self
            .projects
            .iter()
            .filter(|candidate| path.starts_with(candidate.routing_root()))
            .max_by_key(|candidate| {
                (
                    candidate.kind() == ProjectKind::Chalk,
                    candidate.routing_root().as_str().len(),
                )
            }) {
            Some(owner) => owner.routing_root() == project.routing_root(),
            None => project.kind() == ProjectKind::Workspace,
        }
    }

    pub(crate) fn index(&self) -> &Index {
        &self.index
    }

    pub(crate) fn global_settings(&self) -> &GlobalSettings {
        &self.global_settings
    }

    pub(crate) fn position_encoding(&self) -> PositionEncoding {
        self.position_encoding
    }

    pub(crate) fn resolved_client_capabilities(&self) -> ResolvedClientCapabilities {
        self.resolved_client_capabilities
    }

    pub(crate) const fn in_test(&self) -> bool {
        self.in_test
    }

    pub(crate) fn revision(&self) -> u64 {
        self.revision
    }

    pub(crate) fn client_name(&self) -> ClientName {
        self.client_name
    }
}

/// Represents the client (editor) that's connected to the language server.
#[derive(Debug, Clone, Copy)]
pub(crate) enum ClientName {
    Zed,
    Other,
}

impl From<Option<ClientInfo>> for ClientName {
    fn from(info: Option<ClientInfo>) -> Self {
        match info {
            Some(info) if matches!(info.name.as_str(), "Zed") => ClientName::Zed,
            _ => ClientName::Other,
        }
    }
}

impl ClientName {
    /// Returns editor-specific guidance for finding logs.
    ///
    /// Different editors have different ways to access language server logs, so we provide tailored
    /// instructions based on the connected client.
    pub(crate) fn log_guidance(self) -> &'static str {
        match self {
            ClientName::Zed => {
                "Please refer to the logs for more details \
                    (command palette: `dev: open language server logs`)."
            }
            ClientName::Other => "Please refer to the logs for more details.",
        }
    }
}

#[derive(Debug, Default)]
pub(crate) struct Workspaces {
    workspaces: BTreeMap<SystemPathBuf, Workspace>,
}

impl Workspaces {
    /// Registers a new workspace with the given URI and default settings for the workspace.
    ///
    /// This returns `true` when this workspace is added and `false`
    /// when it has already been added.
    ///
    /// It's the caller's responsibility to later call
    /// [`Session::request_uninitialized_workspace_folder_configurations`] with
    /// the resolved settings for this workspace. Registering and initializing
    /// a workspace is a two-step process because the workspace are announced
    /// to the server during the `initialize` request, but the resolved
    /// settings are only available after the client has responded to the
    /// `workspace/configuration` request.
    fn register(&mut self, uri: Uri) -> anyhow::Result<bool> {
        let path = uri
            .to_file_path()
            .map_err(|()| anyhow!("Workspace URI is not a file or directory: {uri:?}"))?;

        // Realistically I don't think this can fail because we got the path from a Uri
        let system_path = SystemPathBuf::from_path_buf(path)
            .map_err(|_| anyhow!("Workspace URI is not valid UTF8"))?;

        if self.workspaces.contains_key(&system_path) {
            return Ok(false);
        }

        self.workspaces.insert(
            system_path,
            Workspace {
                uri,
                settings: Arc::new(WorkspaceSettings::default()),
                initialized: false,
            },
        );
        Ok(true)
    }

    /// Unregisters a workspace folder at the given path.
    ///
    /// Returns `true` if the workspace was removed, `false` if it wasn't found.
    fn unregister(&mut self, path: &SystemPath) -> bool {
        self.workspaces.remove(path).is_some()
    }

    /// Returns a reference to the workspace for the given path, [`None`] if there's no workspace
    /// registered for the path.
    fn for_path(&self, path: impl AsRef<SystemPath>) -> Option<&Workspace> {
        let path = path.as_ref();
        self.workspaces
            .range(..=path.to_path_buf())
            .rfind(|(workspace_root, _)| path.starts_with(workspace_root))
            .map(|(_, db)| db)
    }

    fn root_for_path(&self, path: impl AsRef<SystemPath>) -> Option<&SystemPathBuf> {
        let path = path.as_ref();
        self.workspaces
            .range(..=path.to_path_buf())
            .rfind(|(workspace_root, _)| path.starts_with(workspace_root))
            .map(|(workspace_root, _)| workspace_root)
    }

    /// Returns the client settings for the workspace at the given path, [`None`] if there's no
    /// workspace registered for the path.
    fn settings_for_path(&self, path: impl AsRef<SystemPath>) -> Option<Arc<WorkspaceSettings>> {
        self.for_path(path).map(Workspace::settings_arc)
    }

    fn settings_virtual_fallback(&self) -> Option<Arc<WorkspaceSettings>> {
        self.workspaces.values().next().map(Workspace::settings_arc)
    }

    /// Returns `true` if all workspaces have been [initialized].
    ///
    /// [initialized]: Workspaces::initialize
    fn all_initialized(&self) -> bool {
        self.workspaces.values().all(Workspace::is_initialized)
    }
}

impl<'a> IntoIterator for &'a Workspaces {
    type Item = (&'a SystemPathBuf, &'a Workspace);
    type IntoIter = std::collections::btree_map::Iter<'a, SystemPathBuf, Workspace>;

    fn into_iter(self) -> Self::IntoIter {
        self.workspaces.iter()
    }
}

#[derive(Debug)]
pub(crate) struct Workspace {
    /// The workspace root URI as sent by the client during initialization.
    uri: Uri,
    /// The settings for this workspace.
    ///
    /// The settings here have already been "combined" with the initialization
    /// settings for the LSP.
    settings: Arc<WorkspaceSettings>,
    /// Whether this workspace has been initialized or not.
    ///
    /// If a workspace hasn't been initialized, then it is still
    /// generally usable. It just means that its settings may not
    /// be correct. That is, a workspace is considered initialized
    /// when the configuration from a `workspace/configuration`
    /// request has been received and set on this workspace.
    initialized: bool,
}

impl Workspace {
    pub(crate) fn uri(&self) -> &Uri {
        &self.uri
    }

    pub(crate) fn settings(&self) -> &WorkspaceSettings {
        &self.settings
    }

    pub(crate) fn settings_arc(&self) -> Arc<WorkspaceSettings> {
        self.settings.clone()
    }

    pub(crate) fn is_initialized(&self) -> bool {
        self.initialized
    }

    pub(crate) fn initialize(&mut self, settings: WorkspaceSettings) {
        self.settings = Arc::new(settings);
        self.initialized = true;
    }
}

/// A workspace diagnostic request that didn't yield any changes or diagnostic
/// when it ran the last time.
#[derive(Debug)]
pub(crate) struct SuspendedWorkspaceDiagnosticRequest {
    /// The LSP request id
    pub(crate) id: RequestId,

    /// The params passed to the `workspace/diagnostic` request.
    pub(crate) params: serde_json::Value,

    /// The session's revision when the request ran the last time.
    ///
    /// This is to prevent races between:
    /// * The background thread completes
    /// * A did change notification coming in
    /// * storing this struct on `Session`
    ///
    /// The revision helps us detect that a did change notification
    /// happened in the meantime, so that we can reschedule the
    /// workspace diagnostic request immediately.
    pub(crate) revision: u64,
}

impl SuspendedWorkspaceDiagnosticRequest {
    fn resume_if_revision_changed(self, current_revision: u64, client: &Client) -> Option<Self> {
        if self.revision == current_revision {
            return Some(self);
        }

        tracing::debug!("Resuming workspace diagnostics request after revision bump");
        client.queue_action(Action::RetryRequest(lsp_server::Request {
            id: self.id,
            method: WorkspaceDiagnosticRequest::METHOD.to_string(),
            params: self.params,
        }));

        None
    }
}

/// A handle to a document stored within [`Index`].
///
/// Allows identifying the document within the index but it also carries the URI used by the
/// client to reference the document as well as the version of the document.
///
/// It also exposes methods to get the file-path of the corresponding ty-file.
#[derive(Clone, Debug)]
pub(crate) enum DocumentHandle {
    Text {
        uri: lsp_types::Uri,
        path: AnySystemPath,
        version: DocumentVersion,
    },
    Notebook {
        uri: lsp_types::Uri,
        path: AnySystemPath,
        version: DocumentVersion,
    },
    Cell {
        uri: lsp_types::Uri,
        version: DocumentVersion,
        notebook_path: AnySystemPath,
    },
}

impl DocumentHandle {
    fn from_text_document(document: &TextDocument) -> Self {
        match document.notebook() {
            None => Self::Text {
                version: document.version(),
                uri: document.uri().clone(),
                path: DocumentKey::from_uri(document.uri()).into_file_path(),
            },
            Some(notebook) => Self::Cell {
                notebook_path: notebook.clone(),
                version: document.version(),
                uri: document.uri().clone(),
            },
        }
    }

    fn from_notebook_document(document: &NotebookDocument) -> Self {
        Self::Notebook {
            path: DocumentKey::from_uri(document.uri()).into_file_path(),
            uri: document.uri().clone(),
            version: document.version(),
        }
    }

    fn from_document(document: &Document) -> Self {
        match document {
            Document::Text(text) => Self::from_text_document(text),
            Document::Notebook(notebook) => Self::from_notebook_document(notebook),
        }
    }

    fn key(&self) -> DocumentKey {
        DocumentKey::from_uri(self.uri())
    }

    pub(crate) const fn version(&self) -> DocumentVersion {
        match self {
            Self::Text { version, .. }
            | Self::Notebook { version, .. }
            | Self::Cell { version, .. } => *version,
        }
    }

    /// The URI as used by the client to reference this document.
    pub(crate) fn uri(&self) -> &lsp_types::Uri {
        match self {
            Self::Text { uri, .. } | Self::Notebook { uri, .. } | Self::Cell { uri, .. } => uri,
        }
    }

    /// The path to the enclosing file for this document.
    ///
    /// This is the path corresponding to the URI, except for notebook cells where the
    /// path corresponds to the notebook file.
    pub(crate) fn notebook_or_file_path(&self) -> &AnySystemPath {
        match self {
            Self::Text { path, .. } | Self::Notebook { path, .. } => path,
            Self::Cell { notebook_path, .. } => notebook_path,
        }
    }

    #[expect(unused)]
    pub(crate) fn file_path(&self) -> Option<&AnySystemPath> {
        match self {
            Self::Text { path, .. } | Self::Notebook { path, .. } => Some(path),
            Self::Cell { .. } => None,
        }
    }

    #[expect(unused)]
    pub(crate) fn notebook_path(&self) -> Option<&AnySystemPath> {
        match self {
            DocumentHandle::Notebook { path, .. } => Some(path),
            DocumentHandle::Cell { notebook_path, .. } => Some(notebook_path),
            DocumentHandle::Text { .. } => None,
        }
    }

    /// Returns the salsa interned [`File`] for the document selected by this query.
    ///
    /// It returns [`None`] for the following cases:
    /// - For virtual file, if it's not yet opened
    /// - For regular file, if it does not exists or is a directory
    pub(crate) fn notebook_or_file(&self, db: &dyn Db) -> Option<File> {
        match &self.notebook_or_file_path() {
            AnySystemPath::System(path) => system_path_to_file(db, path).ok(),
            AnySystemPath::SystemVirtual(virtual_path) => db
                .files()
                .try_virtual_file(virtual_path)
                .map(|virtual_file| virtual_file.file()),
        }
    }

    pub(crate) fn is_cell(&self) -> bool {
        matches!(self, Self::Cell { .. })
    }

    pub(crate) fn is_cell_or_notebook(&self) -> bool {
        matches!(self, Self::Cell { .. } | Self::Notebook { .. })
    }

    pub(crate) fn update_text_document(
        &mut self,
        session: &mut Session,
        content_changes: Vec<TextDocumentContentChangeEvent>,
        new_version: DocumentVersion,
    ) -> crate::Result<()> {
        let position_encoding = session.position_encoding();
        {
            let mut index = session.index_mut();

            let document_mut = index.document_mut(&self.key())?;

            let Some(document) = document_mut.as_text_mut() else {
                anyhow::bail!("Text document path does not point to a text document");
            };

            if content_changes.is_empty() {
                document.update_version(new_version);
            } else {
                document.apply_changes(content_changes, new_version, position_encoding);
            }

            self.set_version(document.version());
        }

        self.update_in_db(session);

        Ok(())
    }

    pub(crate) fn update_notebook_document(
        &mut self,
        session: &mut Session,
        cells: Option<lsp_types::NotebookDocumentCellChanges>,
        metadata: Option<lsp_types::LspObject>,
        new_version: DocumentVersion,
    ) -> crate::Result<()> {
        let position_encoding = session.position_encoding();
        {
            let mut index = session.index_mut();

            index.update_notebook_document(
                &self.key(),
                cells,
                metadata,
                new_version,
                position_encoding,
            )?;

            self.set_version(new_version);
        }

        self.update_in_db(session);
        Ok(())
    }

    fn update_in_db(&self, session: &mut Session) {
        let path = self.notebook_or_file_path();
        let changes = match path {
            AnySystemPath::System(system_path) => {
                [ChangeEvent::file_content_changed(system_path.clone())]
            }
            AnySystemPath::SystemVirtual(virtual_path) => {
                [ChangeEvent::ChangedVirtual(virtual_path.clone())]
            }
        };

        session.apply_changes(&changes);
    }

    fn set_version(&mut self, version: DocumentVersion) {
        let self_version = match self {
            DocumentHandle::Text { version, .. }
            | DocumentHandle::Notebook { version, .. }
            | DocumentHandle::Cell { version, .. } => version,
        };

        *self_version = version;
    }

    /// De-registers a document, specified by its key.
    /// Calling this multiple times for the same document is a logic error.
    ///
    /// Returns `true` if the client needs to clear the diagnostics for this document.
    ///
    /// # Errors
    ///
    /// This can return an error when the document does not exist in the
    /// session index.
    pub(crate) fn close(&self, session: &mut Session) -> crate::Result<bool> {
        let is_cell = self.is_cell();
        let path = self.notebook_or_file_path();

        let removed_document = session.index_mut().close_document(&self.key())?;

        // Close the text or notebook file in the database but skip this
        // step for cells because closing a cell doesn't close its notebook.
        let requires_clear_diagnostics = if is_cell {
            true
        } else {
            let db = session.project_db_mut(path);

            match path {
                AnySystemPath::System(system_path) => {
                    if let Some(file) = db.files().try_system(db, system_path) {
                        db.project().close_file(db, file);

                        // In case we preferred the language given by the Client
                        // over the one detected by the file extension, remove the file
                        // from the project to handle cases where a user changes the language
                        // of a file (which results in a didClose and didOpen for the same path but with different languages).
                        if removed_document.language_id().is_some()
                            && system_path
                                .extension()
                                .and_then(PySourceType::try_from_extension)
                                .is_none()
                        {
                            db.project().remove_file(db, file);
                        }
                    } else {
                        // This can only fail when the path is a directory or it doesn't exists but the
                        // file should exists for this handler in this branch. This is because every
                        // close call is preceded by an open call, which ensures that the file is
                        // interned in the lookup table (`Files`).
                        tracing::warn!("Salsa file does not exists for {}", system_path);
                    }

                    // For non-virtual files, we clear diagnostics if:
                    //
                    // 1. The file does not belong to any workspace e.g., opening a random file from
                    //    outside the workspace because closing it acts like the file doesn't exists
                    // 2. The diagnostic mode is set to open-files only
                    session.workspaces().for_path(system_path).is_none()
                        || session
                            .global_settings()
                            .diagnostic_mode()
                            .is_open_files_only()
                }
                AnySystemPath::SystemVirtual(virtual_path) => {
                    if let Some(virtual_file) = db.files().try_virtual_file(virtual_path) {
                        db.project().close_file(db, virtual_file.file());
                        virtual_file.close(db);
                    } else {
                        tracing::warn!("Salsa virtual file does not exists for {}", virtual_path);
                    }

                    // Always clear diagnostics for virtual files, as they don't really exist on disk
                    // which means closing them is like deleting the file.
                    true
                }
            }
        };

        if !is_cell {
            match path {
                AnySystemPath::System(system_path) => {
                    session
                        .apply_changes(&[ChangeEvent::file_content_changed(system_path.clone())]);

                    let state = session.project_state_mut(path);
                    if let Some(file) = state.db.files().try_system(&state.db, system_path)
                        && let Some(chalk_project) = state.chalk_project.as_mut()
                        && chalk_project.close_source(file)
                        && let Err(error) = chalk_project.refresh(&mut state.db)
                    {
                        tracing::error!(
                            "Failed to refresh Chalk sources after closing `{system_path}`: {error}"
                        );
                    }
                }
                AnySystemPath::SystemVirtual(virtual_path) => {
                    for db in session.projects_mut() {
                        File::sync_virtual_path(db, virtual_path);
                    }
                }
            }
        }

        if is_cell || matches!(path, AnySystemPath::SystemVirtual(_)) {
            session.bump_revision();
        }

        Ok(requires_clear_diagnostics)
    }
}

fn is_chalk_sql_path(path: &SystemPath) -> bool {
    path.file_name()
        .is_some_and(|name| name.ends_with(".chalk.sql"))
}

/// Warns about unknown options received by the server.
///
/// If `workspace_uri` is `Some`, it indicates that the unknown options were received during a
/// workspace initialization, otherwise they were received during the server initialization.
pub(super) fn warn_about_unknown_options(
    client: &Client,
    workspace_uri: Option<&Uri>,
    unknown_options: &HashMap<String, serde_json::Value>,
) {
    let message = if let Some(workspace_uri) = workspace_uri {
        format!(
            "Received unknown options for workspace `{workspace_uri}`: {}",
            serde_json::to_string_pretty(unknown_options)
                .unwrap_or_else(|_| format!("{unknown_options:?}"))
        )
    } else {
        format!(
            "Received unknown options during initialization: {}",
            serde_json::to_string_pretty(unknown_options)
                .unwrap_or_else(|_| format!("{unknown_options:?}"))
        )
    };
    tracing::warn!("{message}");
    client.show_warning_message(message);
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use anyhow::{Context, anyhow};
    use lsp_types::{LanguageKind, TextDocumentContentChangeEvent};
    use ruff_db::Db;
    use ruff_db::files::system_path_to_file;
    use ruff_db::source::source_text;
    use ruff_db::system::{InMemorySystem, System, SystemPath, SystemPathBuf};
    use ruff_python_ast::PythonVersion;
    use ruff_ranged_value::{RangedValue, ValueSource};
    use ty_project::metadata::options::ProjectOptionsOverrides;
    use ty_project::metadata::python_version::SupportedPythonVersion;
    use ty_project::{CheckMode, Db as _, ProjectDatabase, ProjectMetadata};

    use super::{
        ChangeEvent, ClientName, CreatedKind, InitializationOptions, ProjectKind, ProjectState,
        Session, TextDocument, Uri, WorkspaceSettings,
    };
    use crate::PositionEncoding;
    use crate::capabilities::ResolvedClientCapabilities;
    use crate::system::LSPSystem;

    fn session() -> anyhow::Result<(Session, Arc<InMemorySystem>)> {
        let native_system = Arc::new(InMemorySystem::new("/workspace".into()));
        let workspace_root = native_system.current_directory().to_path_buf();
        let workspace_uri = Uri::from_file_path(workspace_root.as_std_path())
            .map_err(|()| anyhow!("workspace path must be a valid URI"))?;
        let mut session = Session::new(
            ResolvedClientCapabilities::default(),
            PositionEncoding::UTF8,
            vec![workspace_uri],
            InitializationOptions::default(),
            native_system.clone(),
            ClientName::Other,
            true,
        )?;

        let system = LSPSystem::new(
            session
                .index
                .as_ref()
                .context("session index must be available")?
                .clone(),
            native_system.clone(),
        );
        let metadata = ProjectMetadata::new("workspace", workspace_root.clone());
        let mut db = ProjectDatabase::use_defaults(metadata, system);
        db.set_check_mode(CheckMode::OpenFiles);
        session.projects.insert(
            workspace_root.clone(),
            ProjectState {
                kind: ProjectKind::Workspace,
                workspace_root: Some(workspace_root),
                untracked_files_with_pushed_diagnostics: Vec::new(),
                chalk_project: None,
                db,
            },
        );

        Ok((session, native_system))
    }

    fn open_python_document(
        session: &mut Session,
        path: &SystemPath,
    ) -> anyhow::Result<super::DocumentHandle> {
        open_document(session, path, "", LanguageKind::Python)
    }

    fn open_document(
        session: &mut Session,
        path: &SystemPath,
        contents: &str,
        language_id: LanguageKind,
    ) -> anyhow::Result<super::DocumentHandle> {
        let uri = Uri::from_file_path(path.as_std_path())
            .map_err(|()| anyhow!("document path must be a valid URI"))?;
        Ok(
            session.open_text_document(TextDocument::new(
                uri,
                contents.to_string(),
                1,
                language_id,
            )),
        )
    }

    fn has_open_file(state: &ProjectState, path: &SystemPath) -> bool {
        state
            .db
            .files()
            .try_system(&state.db, path)
            .is_some_and(|file| state.db.project().open_files(&state.db).contains(&file))
    }

    fn source_paths(state: &ProjectState) -> Vec<String> {
        state
            .chalk_project
            .as_ref()
            .unwrap()
            .input()
            .source_files(&state.db)
            .iter()
            .map(|file| file.path(&state.db).as_str().to_string())
            .collect()
    }

    #[test]
    fn containing_chalk_project_routes_workspace_document() -> anyhow::Result<()> {
        let (mut session, native_system) = session()?;
        native_system.fs().write_files_all([
            (SystemPathBuf::from("/chalk.yml"), ""),
            (SystemPathBuf::from("/workspace/main.py"), ""),
        ])?;

        let main = SystemPath::new("/workspace/main.py");
        let handle = open_python_document(&mut session, main)?;
        let (routing_root, state) = session
            .project_entry_for_path(main)
            .context("the containing Chalk project must own the document")?;

        assert_eq!(routing_root.as_path(), SystemPath::new("/"));
        assert_eq!(state.kind(), ProjectKind::Chalk);
        assert_eq!(
            state.workspace_root.as_deref(),
            Some(SystemPath::new("/workspace"))
        );
        assert!(has_open_file(state, main));
        assert!(!has_open_file(
            &session.projects[SystemPath::new("/workspace")],
            main
        ));
        assert_eq!(source_paths(state), ["/workspace/main.py"]);
        assert!(
            session
                .snapshot_document(handle.uri())?
                .chalk_project()
                .is_some()
        );

        let snapshot = session.snapshot_session();
        let owners = snapshot
            .routed_projects()
            .iter()
            .filter_map(|project| {
                let file = system_path_to_file(project.db(), main).ok()?;
                snapshot
                    .project_owns_file(project, file)
                    .then(|| project.routing_root())
            })
            .collect::<Vec<_>>();
        assert_eq!(owners, [SystemPath::new("/")]);

        Ok(())
    }

    #[test]
    fn containing_chalk_project_routes_unsaved_chalk_sql_source() -> anyhow::Result<()> {
        let (mut session, native_system) = session()?;
        native_system
            .fs()
            .write_files_all([(SystemPathBuf::from("/chalk.yml"), "")])?;

        let sql = SystemPath::new("/workspace/features.chalk.sql");
        open_document(&mut session, sql, "select 1", LanguageKind::new("sql"))?;
        let (routing_root, state) = session
            .project_entry_for_path(sql)
            .context("the containing Chalk project must own the SQL source")?;

        assert_eq!(routing_root.as_path(), SystemPath::new("/"));
        assert_eq!(state.kind(), ProjectKind::Chalk);
        assert_eq!(
            state.workspace_root.as_deref(),
            Some(SystemPath::new("/workspace"))
        );
        assert_eq!(source_paths(state), ["/workspace/features.chalk.sql"]);
        assert!(!has_open_file(state, sql));

        Ok(())
    }

    #[test]
    fn lazy_chalk_projects_route_siblings_and_survive_marker_deletion() -> anyhow::Result<()> {
        let (mut session, native_system) = session()?;
        native_system.fs().write_files_all([
            (SystemPathBuf::from("/workspace/parent.py"), ""),
            (SystemPathBuf::from("/workspace/first/chalk.yaml"), "{}"),
            (
                SystemPathBuf::from("/workspace/first/unopened.py"),
                "def invalid(:\n",
            ),
            (SystemPathBuf::from("/workspace/second/chalk.yml"), "{}"),
        ])?;

        let first_file = SystemPath::new("/workspace/first/src/first.py");
        let second_file = SystemPath::new("/workspace/second/src/second.py");
        open_python_document(&mut session, first_file)?;
        open_python_document(&mut session, second_file)?;

        assert_eq!(session.projects.len(), 3);
        {
            let snapshot = session.snapshot_session();
            assert_eq!(snapshot.routed_projects().len(), 3);
            assert_eq!(
                snapshot
                    .routed_projects()
                    .iter()
                    .map(|project| (project.routing_root(), project.kind()))
                    .collect::<Vec<_>>(),
                [
                    (SystemPath::new("/workspace"), ProjectKind::Workspace),
                    (SystemPath::new("/workspace/first"), ProjectKind::Chalk),
                    (SystemPath::new("/workspace/second"), ProjectKind::Chalk),
                ]
            );
            for (path, expected_root) in [
                (
                    SystemPath::new("/workspace/parent.py"),
                    SystemPath::new("/workspace"),
                ),
                (first_file, SystemPath::new("/workspace/first")),
                (second_file, SystemPath::new("/workspace/second")),
            ] {
                let owners: Vec<_> = snapshot
                    .routed_projects()
                    .iter()
                    .filter_map(|project| {
                        let file = ruff_db::files::system_path_to_file(project.db(), path).ok()?;
                        snapshot
                            .project_owns_file(project, file)
                            .then(|| project.routing_root())
                    })
                    .collect();
                assert_eq!(owners, [expected_root]);
            }
        }

        for (file, expected_root) in [
            (first_file, SystemPath::new("/workspace/first")),
            (second_file, SystemPath::new("/workspace/second")),
        ] {
            let (routing_root, state) = session
                .project_entry_for_path(file)
                .context("opened file must have a project")?;
            assert_eq!(routing_root.as_path(), expected_root);
            assert_eq!(state.kind(), ProjectKind::Chalk);
            assert_eq!(
                state.workspace_root.as_deref(),
                Some(SystemPath::new("/workspace"))
            );
            assert!(
                state.db.check().is_empty(),
                "the lazy project must preserve open-files check mode"
            );
        }

        native_system
            .fs()
            .remove_file("/workspace/first/chalk.yaml")?;
        let retained_file = SystemPath::new("/workspace/first/src/retained.py");
        open_python_document(&mut session, retained_file)?;

        assert_eq!(session.projects.len(), 3);
        let (routing_root, state) = session
            .project_entry_for_path(retained_file)
            .context("retained file must have a project")?;
        assert_eq!(routing_root.as_path(), SystemPath::new("/workspace/first"));
        assert_eq!(state.kind(), ProjectKind::Chalk);

        Ok(())
    }

    #[test]
    fn lazy_chalk_project_migrates_existing_open_documents() -> anyhow::Result<()> {
        let (mut session, native_system) = session()?;
        let chalk_root = SystemPath::new("/workspace/project");
        let existing_file = chalk_root.join("existing.py");
        let triggering_file = chalk_root.join("trigger.py");
        open_python_document(&mut session, &existing_file)?;
        open_python_document(&mut session, &triggering_file)?;

        let workspace_state = session
            .projects
            .get(SystemPath::new("/workspace"))
            .context("workspace project must exist")?;
        assert!(has_open_file(workspace_state, &existing_file));
        assert!(has_open_file(workspace_state, &triggering_file));

        native_system
            .fs()
            .write_files_all([(chalk_root.join("chalk.yaml"), "{}")])?;
        open_python_document(&mut session, &triggering_file)?;

        let workspace_state = session
            .projects
            .get(SystemPath::new("/workspace"))
            .context("workspace project must remain")?;
        assert!(!has_open_file(workspace_state, &existing_file));
        assert!(!has_open_file(workspace_state, &triggering_file));

        let chalk_state = session
            .projects
            .get(chalk_root)
            .context("Chalk project must exist")?;
        assert!(has_open_file(chalk_state, &existing_file));
        assert!(has_open_file(chalk_state, &triggering_file));

        Ok(())
    }

    #[test]
    fn chalk_input_is_carried_and_refreshes_without_rerouting() -> anyhow::Result<()> {
        let (mut session, native_system) = session()?;
        native_system.fs().write_files_all([
            (SystemPathBuf::from("/workspace/project/chalk.yml"), ""),
            (SystemPathBuf::from("/workspace/project/main.py"), ""),
        ])?;
        let main = SystemPath::new("/workspace/project/main.py");
        let handle = open_python_document(&mut session, main)?;

        let state = session
            .projects
            .get(SystemPath::new("/workspace/project"))
            .context("Chalk project must exist")?;
        assert_eq!(source_paths(state), ["/workspace/project/main.py"]);

        let document_snapshot = session.snapshot_document(handle.uri())?;
        assert!(document_snapshot.chalk_project().is_some());

        native_system
            .fs()
            .write_file("/workspace/project/added.chalk.sql", "")?;
        session.apply_changes_to_all(&[ChangeEvent::Created {
            path: SystemPathBuf::from("/workspace/project/added.chalk.sql"),
            kind: CreatedKind::File,
        }]);
        assert_eq!(
            source_paths(&session.projects[SystemPath::new("/workspace/project")]),
            [
                "/workspace/project/added.chalk.sql",
                "/workspace/project/main.py"
            ]
        );

        native_system
            .fs()
            .remove_file("/workspace/project/chalk.yml")?;
        session.apply_changes_to_all(&[ChangeEvent::Deleted {
            path: SystemPathBuf::from("/workspace/project/chalk.yml"),
            kind: ty_project::watch::DeletedKind::File,
        }]);
        let state = &session.projects[SystemPath::new("/workspace/project")];
        assert_eq!(state.kind(), ProjectKind::Chalk);
        assert_eq!(
            source_paths(state),
            [
                "/workspace/project/added.chalk.sql",
                "/workspace/project/main.py"
            ]
        );

        Ok(())
    }

    #[test]
    fn editor_file_revisions_are_synchronized_to_every_project() -> anyhow::Result<()> {
        let (mut session, native_system) = session()?;
        native_system.fs().write_files_all([
            (SystemPathBuf::from("/workspace/a/chalk.yml"), ""),
            (SystemPathBuf::from("/workspace/a/main.py"), ""),
            (SystemPathBuf::from("/workspace/b/chalk.yml"), ""),
            (SystemPathBuf::from("/workspace/b/main.py"), ""),
            (SystemPathBuf::from("/workspace/shared.py"), "on disk"),
        ])?;
        open_python_document(&mut session, SystemPath::new("/workspace/a/main.py"))?;
        open_python_document(&mut session, SystemPath::new("/workspace/b/main.py"))?;

        let shared = SystemPath::new("/workspace/shared.py");
        let uri = Uri::from_file_path(shared.as_std_path())
            .map_err(|()| anyhow!("document path must be a valid URI"))?;
        let mut handle = session.open_text_document(TextDocument::new(
            uri,
            "in editor".to_string(),
            1,
            LanguageKind::Python,
        ));

        for state in session.projects.values() {
            let file = ruff_db::files::system_path_to_file(&state.db, shared)?;
            assert_eq!(source_text(&state.db, file).as_str(), "in editor");
        }

        handle.update_text_document(
            &mut session,
            vec![
                TextDocumentContentChangeEvent::TextDocumentContentChangeWholeDocument(
                    lsp_types::TextDocumentContentChangeWholeDocument {
                        text: "edited".to_string(),
                    },
                ),
            ],
            2,
        )?;
        for state in session.projects.values() {
            let file = ruff_db::files::system_path_to_file(&state.db, shared)?;
            assert_eq!(source_text(&state.db, file).as_str(), "edited");
        }

        handle.close(&mut session)?;
        for state in session.projects.values() {
            let file = ruff_db::files::system_path_to_file(&state.db, shared)?;
            assert_eq!(source_text(&state.db, file).as_str(), "on disk");
        }

        Ok(())
    }

    #[test]
    fn unsaved_open_source_is_retained_until_close() -> anyhow::Result<()> {
        let (mut session, native_system) = session()?;
        native_system.fs().write_files_all([
            (SystemPathBuf::from("/workspace/project/chalk.yml"), ""),
            (SystemPathBuf::from("/workspace/project/resolver.py"), ""),
        ])?;
        open_python_document(
            &mut session,
            SystemPath::new("/workspace/project/resolver.py"),
        )?;

        let helper = SystemPath::new("/workspace/project/helper.py");
        assert!(!native_system.path_exists(helper));
        let uri = Uri::from_file_path(helper.as_std_path())
            .map_err(|()| anyhow!("document path must be a valid URI"))?;
        let mut handle = session.open_text_document(TextDocument::new(
            uri,
            "value = 1".to_string(),
            1,
            LanguageKind::Python,
        ));

        let state = &session.projects[SystemPath::new("/workspace/project")];
        assert_eq!(
            source_paths(state),
            [
                "/workspace/project/helper.py",
                "/workspace/project/resolver.py"
            ]
        );
        let file = ruff_db::files::system_path_to_file(&state.db, helper)?;
        assert_eq!(source_text(&state.db, file).as_str(), "value = 1");

        handle.update_text_document(
            &mut session,
            vec![
                TextDocumentContentChangeEvent::TextDocumentContentChangeWholeDocument(
                    lsp_types::TextDocumentContentChangeWholeDocument {
                        text: "value = 2".to_string(),
                    },
                ),
            ],
            2,
        )?;
        let state = &session.projects[SystemPath::new("/workspace/project")];
        let file = ruff_db::files::system_path_to_file(&state.db, helper)?;
        assert_eq!(source_text(&state.db, file).as_str(), "value = 2");
        assert_eq!(
            source_paths(state),
            [
                "/workspace/project/helper.py",
                "/workspace/project/resolver.py"
            ]
        );

        handle.close(&mut session)?;
        let state = &session.projects[SystemPath::new("/workspace/project")];
        assert_eq!(source_paths(state), ["/workspace/project/resolver.py"]);

        Ok(())
    }

    #[test]
    fn closing_unsaved_membership_inputs_restores_disk_source_set() -> anyhow::Result<()> {
        let (mut session, native_system) = session()?;
        native_system.fs().write_files_all([
            (
                SystemPathBuf::from("/workspace/project/chalk.yml"),
                "chalkignore: .chalkignore\n",
            ),
            (SystemPathBuf::from("/workspace/project/.chalkignore"), ""),
            (
                SystemPathBuf::from("/workspace/project/alternate.ignore"),
                "ignored.py\n",
            ),
            (SystemPathBuf::from("/workspace/project/ignored.py"), ""),
            (SystemPathBuf::from("/workspace/project/main.py"), ""),
        ])?;
        open_python_document(&mut session, SystemPath::new("/workspace/project/main.py"))?;

        let chalk_root = SystemPath::new("/workspace/project");
        assert_eq!(
            source_paths(&session.projects[chalk_root]),
            [
                "/workspace/project/ignored.py",
                "/workspace/project/main.py"
            ]
        );

        let local_ignore = open_document(
            &mut session,
            SystemPath::new("/workspace/project/.chalkignore"),
            "ignored.py\n",
            LanguageKind::new("text"),
        )?;
        assert_eq!(
            source_paths(&session.projects[chalk_root]),
            ["/workspace/project/main.py"]
        );
        local_ignore.close(&mut session)?;
        assert_eq!(
            source_paths(&session.projects[chalk_root]),
            [
                "/workspace/project/ignored.py",
                "/workspace/project/main.py"
            ]
        );

        let config = open_document(
            &mut session,
            SystemPath::new("/workspace/project/chalk.yml"),
            "chalkignore: alternate.ignore\n",
            LanguageKind::new("yaml"),
        )?;
        assert_eq!(
            source_paths(&session.projects[chalk_root]),
            ["/workspace/project/main.py"]
        );
        config.close(&mut session)?;
        assert_eq!(
            source_paths(&session.projects[chalk_root]),
            [
                "/workspace/project/ignored.py",
                "/workspace/project/main.py"
            ]
        );

        Ok(())
    }

    #[test]
    fn closing_unsaved_external_ignore_restores_disk_source_set() -> anyhow::Result<()> {
        let (mut session, native_system) = session()?;
        native_system.fs().write_files_all([
            (
                SystemPathBuf::from("/workspace/project/chalk.yml"),
                "chalkignore: /external/chalk.ignore\n",
            ),
            (SystemPathBuf::from("/external/chalk.ignore"), ""),
            (SystemPathBuf::from("/workspace/project/ignored.py"), ""),
            (SystemPathBuf::from("/workspace/project/main.py"), ""),
        ])?;
        open_python_document(&mut session, SystemPath::new("/workspace/project/main.py"))?;

        let chalk_root = SystemPath::new("/workspace/project");
        assert_eq!(
            source_paths(&session.projects[chalk_root]),
            [
                "/workspace/project/ignored.py",
                "/workspace/project/main.py"
            ]
        );

        let ignore = open_document(
            &mut session,
            SystemPath::new("/external/chalk.ignore"),
            "ignored.py\n",
            LanguageKind::new("text"),
        )?;
        assert_eq!(
            source_paths(&session.projects[chalk_root]),
            ["/workspace/project/main.py"]
        );
        ignore.close(&mut session)?;
        assert_eq!(
            source_paths(&session.projects[chalk_root]),
            [
                "/workspace/project/ignored.py",
                "/workspace/project/main.py"
            ]
        );

        Ok(())
    }

    #[test]
    fn chalk_sql_document_lazily_creates_project_without_opening_python_file() -> anyhow::Result<()>
    {
        let (mut session, native_system) = session()?;
        native_system
            .fs()
            .write_files_all([(SystemPathBuf::from("/workspace/project/chalk.yml"), "")])?;
        let sql = SystemPath::new("/workspace/project/features.chalk.sql");

        open_document(&mut session, sql, "select 1", LanguageKind::new("sql"))?;

        let state = session
            .projects
            .get(SystemPath::new("/workspace/project"))
            .context("Chalk project must exist")?;
        assert_eq!(
            source_paths(state),
            ["/workspace/project/features.chalk.sql"]
        );
        assert!(!has_open_file(state, sql));

        Ok(())
    }

    #[test]
    fn lazy_chalk_project_migrates_unsaved_chalk_sql_document() -> anyhow::Result<()> {
        let (mut session, native_system) = session()?;
        let chalk_root = SystemPath::new("/workspace/project");
        let sql = chalk_root.join("features.chalk.sql");
        open_document(&mut session, &sql, "select 1", LanguageKind::new("sql"))?;
        assert!(!native_system.path_exists(&sql));

        native_system.fs().write_files_all([
            (chalk_root.join("chalk.yml"), ""),
            (chalk_root.join("main.py"), ""),
        ])?;
        open_python_document(&mut session, &chalk_root.join("main.py"))?;

        let state = session
            .projects
            .get(chalk_root)
            .context("Chalk project must exist")?;
        assert_eq!(
            source_paths(state),
            [
                "/workspace/project/features.chalk.sql",
                "/workspace/project/main.py"
            ]
        );
        let sql_file = ruff_db::files::system_path_to_file(&state.db, &sql)?;
        assert_eq!(source_text(&state.db, sql_file).as_str(), "select 1");
        assert!(!has_open_file(state, &sql));

        Ok(())
    }

    #[test]
    fn chalk_fallback_applies_workspace_overrides() -> anyhow::Result<()> {
        let (mut session, native_system) = session()?;
        session
            .workspaces
            .workspaces
            .get_mut(SystemPath::new("/workspace"))
            .unwrap()
            .initialize(WorkspaceSettings {
                overrides: Some(ProjectOptionsOverrides {
                    fallback_python_version: Some(RangedValue::new(
                        SupportedPythonVersion::Py310,
                        ValueSource::Editor,
                    )),
                    ..ProjectOptionsOverrides::default()
                }),
                ..WorkspaceSettings::default()
            });
        native_system.fs().write_files_all([
            (SystemPathBuf::from("/workspace/project/chalk.yml"), ""),
            (
                SystemPathBuf::from("/workspace/project/ty.toml"),
                "this is not = valid",
            ),
            (SystemPathBuf::from("/workspace/project/main.py"), ""),
        ])?;

        open_python_document(&mut session, SystemPath::new("/workspace/project/main.py"))?;
        let state = session
            .projects
            .get(SystemPath::new("/workspace/project"))
            .context("Chalk project must exist")?;
        assert_eq!(state.db.python_version(), PythonVersion::PY310);

        Ok(())
    }
}
