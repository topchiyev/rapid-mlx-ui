//! The application actor owns SQLite. Background jobs commit intent through it before I/O.
use crate::{
    mcp_bridge, persistence,
    turn::{self, TurnEvent, TurnRequest},
    view::*,
};
use anyhow::{Context, Result, anyhow, bail, ensure};
use rapid_mlx_core::{
    audio, catalog, chat,
    domain::{self, MessageStatus, Record, RecordStatus, Session},
    mcp, metrics,
    paths::AppPaths,
    runtime,
    storage::Store,
    tools,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex as StdMutex,
        mpsc::{self, Receiver, SyncSender},
    },
    time::{Duration, Instant},
};
use tokio::sync::{Mutex, mpsc as async_mpsc};
use tokio_util::sync::CancellationToken;

type Service = Arc<Mutex<runtime::RuntimeSupervisor>>;

pub struct Backend {
    pub commands: SyncSender<Command>,
    pub snapshots: Receiver<Snapshot>,
    thread: std::thread::JoinHandle<()>,
}
impl Backend {
    pub fn start() -> Self {
        let (commands, input) = mpsc::sync_channel(128);
        let (output, snapshots) = mpsc::sync_channel(16);
        let thread = std::thread::spawn(move || {
            let result = (|| -> Result<()> {
                let executor = tokio::runtime::Builder::new_multi_thread()
                    .enable_all()
                    .worker_threads(4)
                    .build()?;
                executor.block_on(async {
                    let paths = AppPaths::discover()?;
                    let _instance = instance_lock(&paths)?;
                    cleanup_staging(&paths)?;
                    let mut actor = Worker::new(paths)?;
                    actor.run(input, output.clone()).await
                })
            })();
            if let Err(error) = result {
                let _ = output.try_send(Snapshot {
                    error: Some(format!("Application state unavailable: {error:#}")),
                    status: "No alternate storage location was used.".into(),
                    ..Default::default()
                });
            }
        });
        Self {
            commands,
            snapshots,
            thread,
        }
    }
    pub fn finish(thread: std::thread::JoinHandle<()>, commands: SyncSender<Command>) {
        let _ = commands.send(Command::Shutdown);
        let _ = thread.join();
    }
    pub fn into_parts(
        self,
    ) -> (
        SyncSender<Command>,
        Receiver<Snapshot>,
        std::thread::JoinHandle<()>,
    ) {
        (self.commands, self.snapshots, self.thread)
    }
}

struct Active {
    label: String,
    session: Option<String>,
    cancel: CancellationToken,
}

#[derive(Clone, Serialize, Deserialize)]
struct Job {
    id: String,
    model: String,
    revision: Option<String>,
    text: String,
    status: String,
    elapsed_ms: Option<u64>,
    transcript: Option<audio::Transcript>,
    source: String,
    #[serde(default)]
    language: Option<String>,
    #[serde(default)]
    dictation_session: Option<String>,
}

#[derive(Clone)]
struct DictationTarget {
    id: String,
    session: String,
}

enum Event {
    Catalog {
        available: Vec<catalog::AvailableModel>,
        local: catalog::CatalogReport,
        error: Option<String>,
    },
    Runtime {
        role: String,
        model: String,
        result: std::result::Result<Option<chat::Capabilities>, String>,
    },
    RuntimeChecked {
        role: String,
        state: RuntimeView,
        pid: Option<u32>,
    },
    ModelOperation {
        key: String,
        result: std::result::Result<runtime::OperationResult, String>,
    },
    RemovalPreview(std::result::Result<runtime::RemovalPreview, String>),
    Mcp(Vec<mcp::McpConnectionInfo>, Option<String>),
    Context {
        session: String,
        result: std::result::Result<String, String>,
    },
    Recorded {
        result: std::result::Result<audio::StagedAudio, String>,
        dictation: Option<DictationTarget>,
    },
    RecordingStarted,
    Transcript {
        id: String,
        result: std::result::Result<audio::TranscriptionOutcome, String>,
        recording: Option<std::result::Result<Option<PathBuf>, String>>,
    },
}

struct Worker {
    paths: AppPaths,
    store: Store,
    sessions: Vec<Session>,
    preferences: Preferences,
    focused_session: String,
    registrations: Vec<mcp::McpRegistration>,
    connections: Vec<mcp::McpConnectionInfo>,
    mcp: Arc<mcp::McpManager>,
    available: Vec<catalog::AvailableModel>,
    local: Vec<catalog::LocalModel>,
    models: Vec<ModelView>,
    catalog_source: String,
    chat: Service,
    asr: Service,
    chat_view: RuntimeView,
    asr_view: RuntimeView,
    capabilities: Option<chat::Capabilities>,
    effort_profile: chat::EffortProfile,
    runtime_busy: BTreeSet<String>,
    uses: runtime::ModelUseRegistry,
    active: Option<Active>,
    operations: BTreeMap<String, CancellationToken>,
    removals: BTreeSet<String>,
    jobs: Vec<Job>,
    task_handles: Vec<tokio::task::JoinHandle<()>>,
    credentials: BTreeMap<String, String>,
    events: async_mpsc::Sender<Event>,
    event_rx: async_mpsc::Receiver<Event>,
    turns: async_mpsc::Sender<TurnEvent>,
    turn_rx: async_mpsc::Receiver<TurnEvent>,
    metric_history: Arc<StdMutex<metrics::SampleHistory>>,
    owned_pids: Arc<StdMutex<(Option<u32>, Option<u32>)>>,
    recorder: Option<mpsc::Sender<bool>>,
    recording: bool,
    dictation_error: Option<(String, String)>,
    pending_removal: Option<runtime::RemovalPreview>,
    preview_notice: Option<(String, String)>,
    dirty_sessions: BTreeSet<String>,
    status: String,
    error: Option<String>,
    sampling_cancel: CancellationToken,
    sampler: Option<std::thread::JoinHandle<()>>,
}

impl Worker {
    fn new(paths: AppPaths) -> Result<Self> {
        let store = Store::open(paths.clone())?;
        let sessions = store.load_sessions()?;
        let focused_session = store.get_setting("focused_session")?.unwrap_or_default();
        let preferences = store
            .get_setting("preferences")?
            .unwrap_or_else(Preferences::initial);
        let registrations = store.get_setting("mcp_registrations")?.unwrap_or_default();
        let jobs = store
            .load_records("transcription")?
            .into_iter()
            .map(|r| {
                let mut job: Job = serde_json::from_value(r.value)?;
                if matches!(r.status, Some(RecordStatus::Interrupted)) {
                    job.status = "interrupted".into();
                }
                Ok(job)
            })
            .collect::<Result<Vec<_>>>()?;
        let (events, event_rx) = async_mpsc::channel(256);
        let (turns, turn_rx) = async_mpsc::channel(256);
        let uses = runtime::ModelUseRegistry::default();
        Ok(Self {
            paths,
            store,
            sessions,
            preferences,
            focused_session,
            registrations,
            jobs,
            connections: vec![],
            mcp: Arc::new(mcp::McpManager::new()),
            available: vec![],
            local: vec![],
            models: vec![],
            catalog_source: "Catalog has not been refreshed.".into(),
            chat: Arc::new(Mutex::new(runtime::RuntimeSupervisor::with_registry(
                uses.clone(),
            )?)),
            asr: Arc::new(Mutex::new(runtime::RuntimeSupervisor::with_registry(
                uses.clone(),
            )?)),
            chat_view: RuntimeView {
                state: "Stopped".into(),
                ..Default::default()
            },
            asr_view: RuntimeView {
                state: "Stopped".into(),
                ..Default::default()
            },
            capabilities: None,
            effort_profile: chat::EffortProfile::Unknown,
            runtime_busy: BTreeSet::new(),
            uses,
            active: None,
            operations: BTreeMap::new(),
            removals: BTreeSet::new(),
            task_handles: vec![],
            credentials: BTreeMap::new(),
            events,
            event_rx,
            turns,
            turn_rx,
            metric_history: Arc::default(),
            owned_pids: Arc::default(),
            recorder: None,
            recording: false,
            dictation_error: None,
            pending_removal: None,
            preview_notice: None,
            dirty_sessions: BTreeSet::new(),
            status: "Local state restored. Choose a model and connect or load it to start.".into(),
            error: None,
            sampling_cancel: CancellationToken::new(),
            sampler: None,
        })
    }

    async fn run(&mut self, input: Receiver<Command>, output: SyncSender<Snapshot>) -> Result<()> {
        if let Err(error) = self.start_sampler() {
            self.error = Some(format!("System metrics are unavailable: {error:#}"));
        }
        if let Err(error) = self.refresh_models() {
            self.error = Some(format!("Model inventory is unavailable: {error:#}"));
        }
        let mut last_checkpoint = Instant::now();
        let mut last_check = Instant::now();
        let mut last_snapshot = Instant::now() - Duration::from_secs(1);
        loop {
            let mut stop = false;
            for _ in 0..32 {
                match input.try_recv() {
                    Ok(Command::Shutdown) => {
                        stop = true;
                        break;
                    }
                    Ok(command) => {
                        if let Err(error) = self.command(command).await {
                            self.error = Some(format!("{error:#}"));
                        }
                    }
                    Err(mpsc::TryRecvError::Disconnected) => {
                        stop = true;
                        break;
                    }
                    Err(mpsc::TryRecvError::Empty) => break,
                }
            }
            if stop {
                break;
            }
            tokio::select! {
                Some(event) = self.turn_rx.recv() => self.turn_event(event),
                Some(event) = self.event_rx.recv() => { if let Err(error) = self.event(event).await { self.error = Some(format!("{error:#}")); } },
                _ = tokio::time::sleep(Duration::from_millis(20)) => {},
            }
            if last_checkpoint.elapsed() >= Duration::from_millis(250) {
                self.checkpoint();
                last_checkpoint = Instant::now();
            }
            if last_check.elapsed() >= Duration::from_secs(3) {
                self.check_services();
                last_check = Instant::now();
            }
            if last_snapshot.elapsed() >= Duration::from_millis(100) {
                let snapshot = self.snapshot();
                match output.try_send(snapshot) {
                    Ok(()) => {
                        self.preview_notice = None;
                    }
                    Err(mpsc::TrySendError::Disconnected(_)) => break,
                    Err(mpsc::TrySendError::Full(_)) => {}
                }
                last_snapshot = Instant::now();
            }
            self.task_handles.retain(|task| !task.is_finished());
            if self.recording
                && self
                    .active
                    .as_ref()
                    .is_some_and(|a| a.cancel.is_cancelled())
                && let Some(control) = &self.recorder
            {
                let _ = control.send(false);
            }
        }
        self.shutdown().await;
        Ok(())
    }

    fn start_sampler(&mut self) -> Result<()> {
        let pids = self.owned_pids.clone();
        let mut known = (None, None);
        let mut provider = metrics::SystemProvider::new(std::process::id(), None, None)?;
        self.sampler = Some(metrics::spawn_sampler(
            move || {
                let current = pids.lock().map(|p| *p).unwrap_or(known);
                if current != known
                    && let Ok(next) =
                        metrics::SystemProvider::new(std::process::id(), current.0, current.1)
                {
                    known = current;
                    provider = next;
                }
                provider.sample()
            },
            self.metric_history.clone(),
            self.sampling_cancel.clone(),
        ));
        Ok(())
    }

    fn cache(&self) -> Result<PathBuf> {
        Ok(catalog::CacheRoots::from_env(
            (!self.preferences.cache.trim().is_empty())
                .then(|| PathBuf::from(&self.preferences.cache)),
        )?
        .resolve())
    }
    fn executable(&self) -> Result<PathBuf> {
        runtime::discover_executable(
            (self.preferences.executable.trim() != "rapid-mlx"
                && !self.preferences.executable.trim().is_empty())
            .then(|| Path::new(&self.preferences.executable)),
        )
    }
    fn model_operations(&self) -> Result<runtime::ModelOperations> {
        Ok(runtime::ModelOperations {
            executable: self.executable()?,
            cache_root: self.cache()?,
            uses: self.uses.clone(),
        })
    }
    fn session_mut(&mut self, id: &str) -> Result<&mut Session> {
        self.sessions
            .iter_mut()
            .find(|s| s.id == id)
            .context("Session is unavailable")
    }
    fn persist_session(&mut self, id: &str) -> Result<()> {
        let session = self
            .sessions
            .iter()
            .find(|s| s.id == id)
            .context("Session is unavailable")?;
        self.store.save_session(session)?;
        self.dirty_sessions.remove(id);
        Ok(())
    }
    fn checkpoint(&mut self) {
        for id in self.dirty_sessions.clone() {
            if let Err(error) = self.persist_session(&id) {
                self.error = Some(format!("Content is not saved: {error:#}"));
            }
        }
    }

    fn refresh_models(&mut self) -> Result<()> {
        let cache = self.cache()?;
        let roots = self.preferences.model_roots.clone();
        let operations = self.model_operations().ok();
        let tx = self.events.clone();
        self.status = "Refreshing local model inventory and runtime catalog…".into();
        self.task_handles.push(tokio::spawn(async move {
            let report =
                tokio::task::spawn_blocking(move || catalog::discover(&cache, &roots)).await;
            let local = report.unwrap_or_else(|error| catalog::CatalogReport {
                models: vec![],
                errors: vec![error.to_string()],
            });
            let (available, error) = if let Some(operations) = operations {
                match operations.available().await {
                    Ok(models) => (models, None),
                    Err(e) => (vec![], Some(e.to_string())),
                }
            } else {
                (
                    vec![],
                    Some("rapid-mlx was not found. Configure its executable in Settings.".into()),
                )
            };
            let _ = tx
                .send(Event::Catalog {
                    available,
                    local,
                    error,
                })
                .await;
        }));
        Ok(())
    }

    async fn command(&mut self, command: Command) -> Result<()> {
        self.error = None;
        match command {
            Command::WindowSize(size) => {
                ensure!(
                    size.into_iter().all(|n| n.is_finite() && n > 0.0),
                    "Invalid window size"
                );
                self.preferences.window_size = Some(size);
                self.store.set_setting("preferences", &self.preferences)?;
            }
            Command::NewSession => {
                let session = Session::new("New session", "");
                self.store.save_session(&session)?;
                self.focused_session = session.id.clone();
                self.store
                    .set_setting("focused_session", &self.focused_session)?;
                self.sessions.insert(0, session);
            }
            Command::FocusSession(id) => {
                ensure!(
                    self.sessions.iter().any(|s| s.id == id),
                    "Session is unavailable"
                );
                self.store.set_setting("focused_session", &id)?;
                self.focused_session = id;
            }
            Command::Rename { id, title } => {
                ensure!(!title.trim().is_empty(), "Session title is empty");
                self.session_mut(&id)?.title = title.trim().into();
                self.persist_session(&id)?;
            }
            Command::DeleteSession(id) => {
                ensure!(
                    self.active.as_ref().and_then(|a| a.session.as_deref()) != Some(&id),
                    "Stop the active turn before deleting its session"
                );
                self.store.delete_session(&id)?;
                self.sessions.retain(|s| s.id != id);
            }
            Command::SaveSession {
                id,
                draft,
                model,
                generation,
            } => {
                let settings = persistence::settings(&generation);
                settings.validate()?;
                let session = self.session_mut(&id)?;
                session.draft = draft;
                session.model = model;
                session.settings = settings;
                self.persist_session(&id)?;
            }
            Command::Send { id, text } => self.start_turn(id, Some(text), None, None).await?,
            Command::Retry { id, message } => {
                self.start_turn(id, None, Some(message), None).await?
            }
            Command::Edit { id, message, text } => {
                self.start_turn(id, None, None, Some((message, text)))
                    .await?
            }
            Command::Branch { id, head } => {
                ensure!(self.active.is_none(), "Finish or stop the active job first");
                let session = self.session_mut(&id)?;
                let head = persistence::branch_head(session, &head)
                    .context("The selected branch is unavailable")?
                    .to_owned();
                session.select_branch(Some(head))?;
                self.persist_session(&id)?;
            }
            Command::Cancel => {
                if let Some(active) = &self.active {
                    active.cancel.cancel();
                    self.status = "Stopping locally; completed side effects are retained.".into();
                } else {
                    for cancel in self.operations.values() {
                        cancel.cancel();
                    }
                    self.status = "Cancelling model operations…".into();
                }
                if let Some(recorder) = &self.recorder {
                    let _ = recorder.send(false);
                }
            }
            Command::CancelOperation(key) => {
                self.operations
                    .get(&key)
                    .context("This transfer is no longer active")?
                    .cancel();
                self.status = "Cancelling the selected model transfer…".into();
            }
            Command::RefreshModels => self.refresh_models()?,
            Command::Preferences(preferences) => {
                chat::validate_endpoint(&preferences.chat_endpoint)?;
                chat::validate_endpoint(&preferences.asr_endpoint)?;
                ensure!(
                    preferences.chat_endpoint != preferences.asr_endpoint,
                    "Chat and transcription must use distinct endpoints"
                );
                let connections_changed = preferences.executable != self.preferences.executable
                    || preferences.chat_endpoint != self.preferences.chat_endpoint
                    || preferences.asr_endpoint != self.preferences.asr_endpoint
                    || preferences.cache != self.preferences.cache
                    || preferences.model_roots != self.preferences.model_roots;
                ensure!(
                    !connections_changed || self.active.is_none() && self.runtime_busy.is_empty(),
                    "Finish or stop active inference and runtime changes before changing connection/model locations"
                );
                self.store.set_setting("preferences", &preferences)?;
                self.preferences = preferences;
                if connections_changed {
                    self.capabilities = None;
                    self.effort_profile = chat::EffortProfile::Unknown;
                    self.chat_view.state = "Settings changed · reconnect or reload".into();
                    self.asr_view.state = "Settings changed · reconnect or reload".into();
                    self.models.clear();
                    self.refresh_models()?;
                }
                self.status = "Preferences saved.".into();
            }
            Command::Credential { role, value } => {
                if value.is_empty() {
                    self.credentials.remove(&role);
                } else {
                    self.credentials.insert(role, value);
                }
                self.status = "Credential updated for this app run only.".into();
            }
            Command::Runtime {
                role,
                action,
                model,
            } => self.runtime_action(role, action, model)?,
            Command::Pull(id) => {
                let model = self
                    .models
                    .iter()
                    .find(|m| m.id == id)
                    .context("Choose an available model")?;
                let repo = model.repo.clone();
                let reference = if model.catalog {
                    model
                        .id
                        .strip_prefix("alias:")
                        .unwrap_or(&model.id)
                        .to_owned()
                } else {
                    repo.clone()
                };
                ensure!(
                    !self.operations.contains_key(&repo),
                    "A transfer is already active for this model"
                );
                let operations = self.model_operations()?;
                let tx = self.events.clone();
                let cancel = CancellationToken::new();
                self.operations.insert(repo.clone(), cancel.clone());
                self.status = format!(
                    "Pulling {repo} into {} (transfer in progress)…",
                    operations.cache_root.display()
                );
                self.task_handles.push(tokio::spawn(async move {
                    let result = operations
                        .pull_reference(&reference, &repo, cancel)
                        .await
                        .map_err(|e| e.to_string());
                    let _ = tx.send(Event::ModelOperation { key: repo, result }).await;
                }));
            }
            Command::PreviewDeletion(repo) => {
                self.validate_removal_connection()?;
                let operations = self.model_operations()?;
                let tx = self.events.clone();
                self.task_handles.push(tokio::spawn(async move {
                    let preview =
                        tokio::task::spawn_blocking(move || operations.removal_preview(&repo))
                            .await
                            .map_err(|e| e.to_string())
                            .and_then(|r| r.map_err(|e| e.to_string()));
                    let _ = tx.send(Event::RemovalPreview(preview)).await;
                }));
            }
            Command::DeleteModel(repo) => {
                self.validate_removal_connection()?;
                let preview = self
                    .pending_removal
                    .take()
                    .context("Request a fresh deletion preview first")?;
                ensure!(
                    preview.repository == repo,
                    "Deletion target changed; request a new preview"
                );
                let operations = self.model_operations()?;
                let tx = self.events.clone();
                self.removals.insert(repo.clone());
                self.status = format!("Removing pulled model {repo}…");
                self.task_handles.push(tokio::spawn(async move {
                    let result = operations
                        .remove_confirmed(&preview, true)
                        .await
                        .map_err(|e| e.to_string());
                    let _ = tx.send(Event::ModelOperation { key: repo, result }).await;
                }));
            }
            Command::SaveMcp(form) => {
                let registration = mcp_bridge::registration(&form)?;
                if self.registrations.iter().any(|r| r.id == registration.id)
                    && let Err(error) = self.mcp.disconnect(&registration.id).await
                    && error.kind != mcp::McpErrorKind::NotConnected
                {
                    return Err(error.into());
                }
                let mut next = self.registrations.clone();
                next.retain(|r| r.id != registration.id);
                next.push(registration);
                self.store.set_setting("mcp_registrations", &next)?;
                self.registrations = next;
                self.refresh_mcp();
                self.status = "MCP registration saved. Connect explicitly to use it.".into();
            }
            Command::Mcp { id, action } => {
                if action == "remove" {
                    if let Err(error) = self.mcp.disconnect(&id).await
                        && error.kind != mcp::McpErrorKind::NotConnected
                    {
                        return Err(error.into());
                    }
                    let next: Vec<_> = self
                        .registrations
                        .iter()
                        .filter(|r| r.id != id)
                        .cloned()
                        .collect();
                    self.store.set_setting("mcp_registrations", &next)?;
                    self.registrations = next;
                    for session in &mut self.sessions {
                        session.selected_mcp_servers.retain(|s| s != &id);
                        let mapping = session
                            .metadata
                            .get("mcp_tool_servers")
                            .and_then(Value::as_object)
                            .cloned()
                            .unwrap_or_default();
                        let selected = session
                            .metadata
                            .get("mcp_tools")
                            .and_then(Value::as_array)
                            .cloned()
                            .unwrap_or_default()
                            .into_iter()
                            .filter(|tool| {
                                tool.as_str().is_none_or(|name| {
                                    mapping.get(name).and_then(Value::as_str) != Some(&id)
                                })
                            })
                            .collect::<Vec<_>>();
                        if session.metadata.is_object() {
                            session.metadata["mcp_tools"] = json!(selected);
                            if let Some(mapping) = session
                                .metadata
                                .get_mut("mcp_tool_servers")
                                .and_then(Value::as_object_mut)
                            {
                                mapping.retain(|_, server| server.as_str() != Some(&id));
                            }
                        }
                        self.store.save_session(session)?;
                    }
                    self.refresh_mcp();
                } else {
                    let manager = self.mcp.clone();
                    let registration = self
                        .registrations
                        .iter()
                        .find(|r| r.id == id)
                        .cloned()
                        .context("MCP registration is unavailable")?;
                    let tx = self.events.clone();
                    let credentials = self
                        .credentials
                        .get(&id)
                        .map(|token| {
                            mcp::McpCredentials::default().with_bearer_token(token.clone())
                        })
                        .unwrap_or_default();
                    self.status = format!("MCP {action}: {}…", registration.name);
                    self.task_handles.push(tokio::spawn(async move {
                        let result = match action.as_str() {
                            "connect" => {
                                manager.connect(registration, credentials).await.map(|_| ())
                            }
                            "disconnect" => manager.disconnect(&id).await,
                            "refresh" => manager.refresh(&id).await.map(|_| ()),
                            _ => Ok(()),
                        };
                        let error = result.err().map(|e| e.to_string());
                        let _ = tx
                            .send(Event::Mcp(manager.connections().await, error))
                            .await;
                    }));
                }
            }
            Command::SelectTool {
                session,
                server,
                tool,
                enabled,
            } => {
                let state = self.session_mut(&session)?;
                let mut selected: BTreeSet<String> = serde_json::from_value(
                    state
                        .metadata
                        .get("mcp_tools")
                        .cloned()
                        .unwrap_or(json!([])),
                )?;
                if enabled {
                    selected.insert(tool.clone());
                    if !state.selected_mcp_servers.contains(&server) {
                        state.selected_mcp_servers.push(server.clone());
                    }
                } else {
                    selected.remove(&tool);
                }
                if !state.metadata.is_object() {
                    state.metadata = json!({});
                }
                state.metadata["mcp_tools"] = json!(selected);
                if !state.metadata["mcp_tool_servers"].is_object() {
                    state.metadata["mcp_tool_servers"] = json!({});
                }
                if enabled {
                    state.metadata["mcp_tool_servers"][&tool] = json!(server);
                } else if let Some(mapping) = state.metadata["mcp_tool_servers"].as_object_mut() {
                    mapping.remove(&tool);
                }
                self.persist_session(&session)?;
            }
            Command::McpContext {
                session,
                server,
                kind,
                name,
                arguments,
            } => {
                let manager = self.mcp.clone();
                let tx = self.events.clone();
                self.task_handles.push(tokio::spawn(async move {
                    let result: Result<String> = async {
                        if kind == "resource" {
                            Ok(serde_json::to_string_pretty(
                                &manager.read_resource(&server, &name).await?,
                            )?)
                        } else {
                            Ok(serde_json::to_string_pretty(
                                &manager.get_prompt(&server, &name, arguments).await?,
                            )?)
                        }
                    }
                    .await;
                    let result = result
                        .map(|text| format!("\n[MCP {kind} from {server}: {name}]\n{text}\n"))
                        .map_err(|e| e.to_string());
                    let _ = tx.send(Event::Context { session, result }).await;
                }));
            }
            Command::Tools { session, policy } => {
                let configured = tool_policy(&policy)?;
                if policy.enabled {
                    tools::ToolExecutor::new(configured.clone())?;
                }
                let target = self.session_mut(&session)?;
                target.tool_policy = json!(configured);
                if !target.metadata.is_object() {
                    target.metadata = json!({});
                }
                target.metadata["tool_settings"] = json!(policy);
                self.persist_session(&session)?;
            }
            Command::Transcribe(path) => self.transcribe(path, None).await?,
            Command::Record => self.record(None)?,
            Command::RecordForChat { id, session } => {
                self.dictation_error = None;
                let result = (|| {
                    ensure!(
                        !id.is_empty() && !self.jobs.iter().any(|job| job.id == id),
                        "Invalid dictation identity"
                    );
                    self.session_mut(&session)?;
                    self.record(Some(DictationTarget {
                        id: id.clone(),
                        session,
                    }))
                })();
                if let Err(error) = &result {
                    self.dictation_error = Some((id, error.to_string()));
                }
                result?;
            }
            Command::InsertDictation { transcript, draft } => {
                let job = self
                    .jobs
                    .iter()
                    .find(|job| job.id == transcript)
                    .context("Voice transcript is unavailable")?;
                ensure!(
                    job.status == "completed",
                    "Dictation did not complete successfully"
                );
                let session = job
                    .dictation_session
                    .as_ref()
                    .context("This transcript is not chat dictation")?;
                let index = self
                    .sessions
                    .iter()
                    .position(|s| &s.id == session)
                    .context(
                        "The original chat is unavailable; transcript remains in Transcription",
                    )?;
                if !self.sessions[index].transcript_ids.contains(&transcript) {
                    let mut saved = self.sessions[index].clone();
                    let base = draft.as_deref().unwrap_or(&saved.draft);
                    saved.draft = if job.text.trim().is_empty() {
                        base.to_owned()
                    } else {
                        audio::insert_transcript(base, &job.text, audio::InsertMode::Append)
                    };
                    saved.transcript_ids.push(transcript);
                    self.store.save_session(&saved)?;
                    self.sessions[index] = saved;
                    self.status = if job.text.trim().is_empty() {
                        "No speech detected. Chat draft unchanged."
                    } else {
                        "Voice text appended to the original chat draft. Review it, then Send."
                    }
                    .into();
                }
            }
            Command::StopRecording => {
                self.recorder
                    .as_ref()
                    .context("No recording is active")?
                    .send(true)?;
                self.status = "Finishing recording…".into();
            }
            Command::SaveTranscript { id, text } => {
                let job = self
                    .jobs
                    .iter_mut()
                    .find(|j| j.id == id)
                    .context("Transcript is unavailable")?;
                job.text = text.clone();
                if let Some(transcript) = &mut job.transcript {
                    transcript.edited_text = Some(text);
                }
                let record = job_record(job)?;
                self.store.save_record(&record)?;
                self.status = "Transcript edits saved.".into();
            }
            Command::LinkTranscript {
                session,
                transcript,
            } => {
                ensure!(
                    self.jobs.iter().any(|j| j.id == transcript),
                    "Transcript is unavailable"
                );
                let target = self.session_mut(&session)?;
                if !target.transcript_ids.contains(&transcript) {
                    target.transcript_ids.push(transcript);
                }
                self.persist_session(&session)?;
            }
            Command::Shutdown => {}
        }
        Ok(())
    }

    fn runtime_action(&mut self, role: String, action: String, id: String) -> Result<()> {
        ensure!(
            self.removals.is_empty(),
            "Wait for model removal to finish before changing a runtime"
        );
        ensure!(
            self.active.is_none(),
            "Finish or stop active inference before changing a runtime"
        );
        ensure!(
            !self.runtime_busy.contains(&role),
            "This runtime is already changing"
        );
        let is_asr = role == "asr";
        let service = if is_asr {
            self.asr.clone()
        } else {
            self.chat.clone()
        };
        let endpoint = if is_asr {
            self.preferences.asr_endpoint.clone()
        } else {
            self.preferences.chat_endpoint.clone()
        };
        let credential = self.credentials.get(&role).cloned();
        let selected = self.models.iter().find(|m| m.id == id).cloned();
        // A catalog alias identifies its exact repository; external servers do
        // not know the app's `alias:` UI identity.
        let connection_identity = selected
            .as_ref()
            .filter(|model| is_asr && model.catalog && model.supported_asr())
            .map(|model| model.repo.clone())
            .unwrap_or_else(|| id.clone());
        let tx = self.events.clone();
        let kind = if is_asr {
            runtime::RuntimeRole::Transcription
        } else {
            runtime::RuntimeRole::Chat
        };
        let config = if action == "start" {
            let model = selected.as_ref().context("Select a pulled model first")?;
            ensure!(!is_asr || model.supported_asr(), catalog::UNSUPPORTED_ASR);
            ensure!(
                model.pulled && !model.path.as_os_str().is_empty(),
                "Select an exact local revision from Pulled models"
            );
            ensure!(
                model.compatible,
                "The selected model cannot be loaded: {}",
                model.detail
            );
            let mut config = runtime::RuntimeConfig::new(
                self.executable()?,
                model.path.clone(),
                model.id.clone(),
                kind,
                self.cache()?,
            );
            config.port = chat::validate_endpoint(&endpoint)?
                .port_or_known_default()
                .context("Configure a runtime port")?;
            ensure!(
                chat::validate_endpoint(&endpoint)?.host_str() == Some("127.0.0.1"),
                "Managed runtimes bind 127.0.0.1; configure that address before loading. IPv6 loopback is available for external connections."
            );
            config.app_state_root = Some(self.paths.root.clone());
            Some(config)
        } else {
            None
        };
        let view = if is_asr {
            &mut self.asr_view
        } else {
            &mut self.chat_view
        };
        view.state = format!("{action} in progress…");
        if !is_asr {
            self.capabilities = None;
        }
        self.status = format!("Runtime {action} ({role})…");
        self.runtime_busy.insert(role.clone());
        self.task_handles.push(tokio::spawn(async move {
            let outcome = async {
                let mut supervisor = service.lock().await;
                if action == "stop" {
                    supervisor.stop().await?;
                    return Ok(None);
                }
                if let Some(config) = config {
                    if supervisor.owned_pid().is_some() {
                        supervisor.stop().await?;
                    }
                    supervisor.start(config).await?;
                }
                let caps = if is_asr {
                    None
                } else {
                    Some(
                        chat::negotiate_for_runtime(
                            &endpoint,
                            credential.as_deref(),
                            (action == "start").then_some("1.0.2"),
                        )
                        .await?,
                    )
                };
                if action == "connect" {
                    let advertised = if let Some(caps) = &caps {
                        caps.model_ids.clone()
                    } else {
                        runtime::advertised_models(&endpoint, credential.as_deref()).await?
                    };
                    let expected = advertised
                        .iter()
                        .find(|served| *served == &connection_identity)
                        .or_else(|| (!is_asr).then(|| advertised.first()).flatten())
                        .cloned()
                        .context("Server does not advertise the selected model; select its exact identity")?;
                    supervisor
                        .attach_with_credential(&endpoint, &expected, kind, credential.as_deref())
                        .await?;
                }
                Ok::<_, anyhow::Error>(caps)
            }
            .await
            .map_err(|e| e.to_string());
            let _ = tx
                .send(Event::Runtime {
                    role,
                    model: id,
                    result: outcome,
                })
                .await;
        }));
        Ok(())
    }

    fn validate_removal_connection(&self) -> Result<()> {
        ensure!(
            self.runtime_busy.is_empty(),
            "Wait for runtime changes before deleting model files"
        );
        ensure!(
            !self.chat_view.external && !self.asr_view.external,
            "Disconnect external inference services before deleting model files. The app cannot determine every cache file an external service still uses; disconnecting does not stop its process."
        );
        Ok(())
    }

    async fn start_turn(
        &mut self,
        id: String,
        text: Option<String>,
        retry: Option<String>,
        edit: Option<(String, String)>,
    ) -> Result<()> {
        ensure!(
            self.active.is_none(),
            "A chat, recording, or transcription job is already active"
        );
        let caps = self
            .capabilities
            .clone()
            .context("Connect to or load the chat runtime first")?;
        let session = self
            .sessions
            .iter_mut()
            .find(|s| s.id == id)
            .context("Select a session")?;
        ensure!(
            caps.model_ids.contains(&session.model),
            "Selected model is not served. Select the exact served model or load the selected revision."
        );
        let attempt = if let Some(text) = text {
            self.store.begin_turn(session, text)?
        } else if let Some(message) = retry {
            self.store.retry(session, &message)?
        } else if let Some((message, text)) = edit {
            self.store.edit_prompt(session, &message, text)?
        } else {
            bail!("Missing turn action")
        };
        let snapshot = session.clone();
        let profile = if snapshot.model == self.chat_view.model {
            self.effort_profile.clone()
        } else {
            chat::EffortProfile::Unknown
        };
        let tools_verified = caps
            .models
            .get("data")
            .and_then(Value::as_array)
            .is_some_and(|models| {
                models.iter().any(|m| {
                    m["id"].as_str() == Some(&snapshot.model)
                        && m["capabilities"]
                            .as_array()
                            .is_some_and(|c| c.iter().any(|v| v.as_str() == Some("tools")))
                })
            });
        let selected_mcp = serde_json::from_value(
            snapshot
                .metadata
                .get("mcp_tools")
                .cloned()
                .unwrap_or(json!([])),
        )?;
        let local_policy = (!snapshot.tool_policy.is_null())
            .then(|| serde_json::from_value(snapshot.tool_policy.clone()))
            .transpose()?;
        let cancel = CancellationToken::new();
        self.active = Some(Active {
            label: format!("Chat · {}", snapshot.title),
            session: Some(id),
            cancel: cancel.clone(),
        });
        let request = TurnRequest {
            session: snapshot,
            attempt,
            endpoint: self.preferences.chat_endpoint.clone(),
            credential: self.credentials.get("chat").cloned(),
            capabilities: caps,
            effort_profile: profile,
            tools_verified,
            local_policy,
            mcp: self.mcp.clone(),
            selected_mcp,
        };
        let sender = self.turns.clone();
        self.status = "Generating…".into();
        self.task_handles
            .push(tokio::spawn(turn::run(request, sender, cancel)));
        Ok(())
    }

    fn turn_event(&mut self, event: TurnEvent) {
        let (session_id, attempt_id) = match &event {
            TurnEvent::ModelStart {
                session, attempt, ..
            }
            | TurnEvent::Delta {
                session, attempt, ..
            }
            | TurnEvent::ModelEnd {
                session, attempt, ..
            }
            | TurnEvent::ToolStart {
                session, attempt, ..
            }
            | TurnEvent::ToolEnd {
                session, attempt, ..
            }
            | TurnEvent::Finished {
                session, attempt, ..
            } => (session, attempt),
        };
        let current = self
            .active
            .as_ref()
            .is_some_and(|active| active.session.as_ref() == Some(session_id))
            && self
                .sessions
                .iter()
                .find(|session| &session.id == session_id)
                .is_some_and(|session| {
                    session.active_head.as_ref() == Some(attempt_id)
                        && session
                            .message(attempt_id)
                            .is_ok_and(|message| message.status.is_active())
                });
        if !current {
            match event {
                TurnEvent::ModelStart { ack, .. }
                | TurnEvent::ModelEnd { ack, .. }
                | TurnEvent::ToolStart { ack, .. }
                | TurnEvent::ToolEnd { ack, .. } => {
                    let _ = ack.send(Err(
                        "Stale or terminal turn; no operation was authorized".into()
                    ));
                }
                _ => {}
            }
            return;
        }
        match event {
            TurnEvent::ModelStart {
                session,
                attempt,
                request,
                ack,
            } => {
                let result = (|| -> Result<()> {
                    ensure!(
                        !self.active.as_ref().is_none_or(|a| a.cancel.is_cancelled()),
                        "Turn stopped before dispatch"
                    );
                    let message = self.session_mut(&session)?.message_mut(&attempt)?;
                    message.status = MessageStatus::Streaming;
                    message.upsert_model_request(request);
                    self.persist_session(&session)
                })();
                let _ = ack.send(result.map_err(|e| e.to_string()));
            }
            TurnEvent::Delta {
                session,
                attempt,
                request_id,
                event,
            } => {
                if self.active.as_ref().is_none_or(|a| a.cancel.is_cancelled()) {
                    return;
                }
                let result = (|| -> Result<()> {
                    let message = self.session_mut(&session)?.message_mut(&attempt)?;
                    if message.status.is_terminal()
                        || !message.model_requests.iter().any(|r| r.id == request_id)
                    {
                        return Ok(());
                    }
                    match event {
                        chat::ChatEvent::Content(text) => message.content.push_str(&text),
                        chat::ChatEvent::Reasoning(text) => message.reasoning.push_str(&text),
                        chat::ChatEvent::Usage(raw) => {
                            if let Some(request) = message
                                .model_requests
                                .iter_mut()
                                .find(|r| r.id == request_id)
                            {
                                request.usage = usage(raw);
                            }
                        }
                        _ => {}
                    }
                    self.dirty_sessions.insert(session);
                    Ok(())
                })();
                if let Err(e) = result {
                    self.error = Some(e.to_string());
                }
            }
            TurnEvent::ModelEnd {
                session,
                attempt,
                request_id,
                outcome,
                ack,
            } => {
                let result = (|| -> Result<()> {
                    let stopped = self
                        .active
                        .as_ref()
                        .is_some_and(|a| a.cancel.is_cancelled());
                    let message = self.session_mut(&session)?.message_mut(&attempt)?;
                    let request = message
                        .model_requests
                        .iter_mut()
                        .find(|r| r.id == request_id)
                        .context("Unknown model request")?;
                    request.status = if stopped {
                        MessageStatus::Cancelled
                    } else {
                        match outcome.status {
                            chat::ChatStatus::Completed => MessageStatus::Completed,
                            chat::ChatStatus::Cancelled => MessageStatus::Cancelled,
                            chat::ChatStatus::Failed => MessageStatus::Failed,
                        }
                    };
                    if let Some(raw) = outcome.usage {
                        request.usage = usage(raw);
                    }
                    request.client_elapsed_ms = Some(outcome.elapsed_ms);
                    request.client_first_output_ms = outcome.first_output_ms;
                    self.persist_session(&session)
                })();
                let _ = ack.send(result.map_err(|e| e.to_string()));
            }
            TurnEvent::ToolStart {
                session,
                attempt,
                record,
                ack,
            } => {
                let result = (|| -> Result<()> {
                    ensure!(
                        !self.active.as_ref().is_none_or(|a| a.cancel.is_cancelled()),
                        "Turn stopped before tool dispatch"
                    );
                    let message = self.session_mut(&session)?.message_mut(&attempt)?;
                    ensure!(
                        !message.tools.iter().any(|t| t.id == record.id),
                        "Duplicate tool call"
                    );
                    message.tools.push(record);
                    self.persist_session(&session)
                })();
                let _ = ack.send(result.map_err(|e| e.to_string()));
            }
            TurnEvent::ToolEnd {
                session,
                attempt,
                record,
                ack,
            } => {
                let result = (|| -> Result<()> {
                    let message = self.session_mut(&session)?.message_mut(&attempt)?;
                    let tool = message
                        .tools
                        .iter_mut()
                        .find(|t| t.id == record.id)
                        .context("Unknown tool call")?;
                    *tool = record;
                    self.persist_session(&session)
                })();
                let _ = ack.send(result.map_err(|e| e.to_string()));
            }
            TurnEvent::Finished {
                session,
                attempt,
                status,
                error,
            } => {
                let stopped = self
                    .active
                    .as_ref()
                    .is_some_and(|a| a.cancel.is_cancelled());
                let result = (|| -> Result<()> {
                    let message = self.session_mut(&session)?.message_mut(&attempt)?;
                    message.status = if stopped {
                        MessageStatus::Cancelled
                    } else {
                        status
                    };
                    if !message.metadata.is_object() {
                        message.metadata = json!({});
                    }
                    if let Some(error) = error {
                        message.metadata["error"] = json!(error);
                    }
                    for request in &mut message.model_requests {
                        if request.status.is_active() {
                            request.status = message.status;
                        }
                    }
                    for tool in &mut message.tools {
                        if tool.status.is_active() {
                            tool.status = domain::ToolStatus::OutcomeUnknown;
                        }
                    }
                    self.persist_session(&session)
                })();
                if let Err(error) = result {
                    self.error = Some(format!("Final response is not saved: {error:#}"));
                }
                self.active = None;
                self.status = "Turn finished. Usage includes all model requests.".into();
            }
        }
    }

    fn record(&mut self, dictation: Option<DictationTarget>) -> Result<()> {
        ensure!(
            self.active.is_none(),
            "Finish active inference before recording"
        );
        self.selected_asr()?;
        let root = self.paths.root.clone();
        let tx = self.events.clone();
        let (control, input) = mpsc::channel();
        self.recorder = Some(control);
        self.recording = true;
        self.active = Some(Active {
            label: "Recording microphone".into(),
            session: dictation.as_ref().map(|target| target.session.clone()),
            cancel: CancellationToken::new(),
        });
        std::thread::spawn(move || {
            let result = (|| -> Result<audio::StagedAudio> {
                let recording = audio::Recording::start(&root, audio::AudioLimits::default())?;
                let _ = tx.blocking_send(Event::RecordingStarted);
                loop {
                    if let Some(error) = recording.error() {
                        recording.cancel()?;
                        bail!(error);
                    }
                    match input.recv_timeout(Duration::from_millis(100)) {
                        Ok(true) => return recording.stop(),
                        Ok(false) | Err(mpsc::RecvTimeoutError::Disconnected) => {
                            recording.cancel()?;
                            bail!("Recording cancelled");
                        }
                        Err(mpsc::RecvTimeoutError::Timeout) => {}
                    }
                }
            })()
            .map_err(|e| e.to_string());
            let _ = tx.blocking_send(Event::Recorded { result, dictation });
        });
        Ok(())
    }

    async fn transcribe(
        &mut self,
        source: PathBuf,
        staged: Option<audio::StagedAudio>,
    ) -> Result<()> {
        self.transcribe_to(source, staged, None).await
    }

    fn selected_asr(&self) -> Result<ModelView> {
        ensure!(
            !self.runtime_busy.contains("asr"),
            "Transcription runtime is still changing"
        );
        let model = self
            .models
            .iter()
            .find(|m| m.id == self.preferences.asr_model)
            .cloned()
            .context("Select and load a transcription model in Voice settings first")?;
        ensure!(
            model.supported_asr()
                && (model.pulled && !model.path.as_os_str().is_empty() || model.served),
            catalog::UNSUPPORTED_ASR
        );
        let supervisor = self
            .asr
            .try_lock()
            .map_err(|_| anyhow!("Transcription runtime is still changing"))?;
        ensure!(
            matches!(supervisor.state(), runtime::RuntimeState::Ready { model: served, .. } if served == &model.id),
            "Load the selected transcription model in Voice settings first"
        );
        ensure!(
            supervisor
                .endpoint()
                .map(chat::validate_endpoint)
                .transpose()?
                == Some(chat::validate_endpoint(&self.preferences.asr_endpoint)?),
            "Transcription connection settings changed; reload or reconnect the service"
        );
        Ok(model)
    }

    async fn transcribe_to(
        &mut self,
        source: PathBuf,
        staged: Option<audio::StagedAudio>,
        dictation: Option<DictationTarget>,
    ) -> Result<()> {
        ensure!(
            self.active.is_none(),
            "Finish active chat or transcription first"
        );
        let model = self.selected_asr()?;
        let client = audio::TranscriptionClient::with_credential(
            &self.preferences.asr_endpoint,
            self.credentials.get("asr").cloned(),
        )?;
        let id = dictation
            .as_ref()
            .map(|target| target.id.clone())
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        let dictation_session = dictation.map(|target| target.session);
        let job = Job {
            id: id.clone(),
            model: model.id,
            revision: (!model.revision.is_empty()).then_some(model.revision),
            text: String::new(),
            status: "transcribing".into(),
            elapsed_ms: None,
            transcript: None,
            source: source.display().to_string(),
            language: (!self.preferences.asr_language.trim().is_empty())
                .then(|| self.preferences.asr_language.trim().to_owned()),
            dictation_session: dictation_session.clone(),
        };
        self.store.save_record(&job_record(&job)?)?;
        self.jobs.insert(0, job);
        let cancel = CancellationToken::new();
        self.active = Some(Active {
            label: "Transcription".into(),
            session: dictation_session,
            cancel: cancel.clone(),
        });
        let tx = self.events.clone();
        let root = self.paths.root.clone();
        let retain = self.preferences.retain_audio;
        // The installed route maps this explicit served identity to the verified local folder.
        let model_path = self.preferences.asr_model.clone();
        let language = (!self.preferences.asr_language.trim().is_empty())
            .then(|| self.preferences.asr_language.trim().to_owned());
        self.task_handles.push(tokio::spawn(async move {
            let result = client
                .transcribe(
                    &source,
                    &model_path,
                    language.as_deref(),
                    audio::AudioLimits::default(),
                    &cancel,
                )
                .await
                .map_err(|e| e.to_string());
            let recording = staged.map(|staged| {
                if retain {
                    staged.retain(&root).map(Some).map_err(|e| {
                        format!("Transcript preserved, but recording retention failed: {e}")
                    })
                } else {
                    drop(staged);
                    Ok(None)
                }
            });
            let _ = tx
                .send(Event::Transcript {
                    id,
                    result,
                    recording,
                })
                .await;
        }));
        self.status = "Transcribing locally…".into();
        Ok(())
    }

    fn refresh_mcp(&mut self) {
        let manager = self.mcp.clone();
        let tx = self.events.clone();
        self.task_handles.push(tokio::spawn(async move {
            let _ = tx.send(Event::Mcp(manager.connections().await, None)).await;
        }));
    }

    async fn event(&mut self, event: Event) -> Result<()> {
        match event {
            Event::Catalog {
                available,
                local,
                error,
            } => {
                self.local = local.models;
                let scanned = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
                if error.is_none() {
                    self.store.set_setting("catalog", &available)?;
                    self.store.set_setting("catalog_updated", &scanned)?;
                    self.available = available;
                } else if self.available.is_empty() {
                    self.available = self.store.get_setting("catalog")?.unwrap_or_default();
                }
                let updated: Option<String> = self.store.get_setting("catalog_updated")?;
                self.catalog_source = format!(
                    "Read-only local scan: {scanned} · Available: {} runtime CLI catalog · last refreshed {}",
                    if error.is_some() { "cached" } else { "current" },
                    updated.as_deref().unwrap_or("unknown")
                );
                self.models = model_views(&self.available, &self.local);
                self.error =
                    error.or_else(|| (!local.errors.is_empty()).then(|| local.errors.join("\n")));
                self.status = format!(
                    "{} local model revisions · {} catalog entries",
                    self.local.len(),
                    self.available.len()
                );
            }
            Event::Runtime {
                role,
                model,
                result,
            } => {
                self.runtime_busy.remove(&role);
                let is_asr = role == "asr";
                let service = if is_asr { &self.asr } else { &self.chat };
                let supervisor = service.lock().await;
                let view = runtime_view(&supervisor);
                let pid = supervisor.owned_pid();
                if let Ok(mut pids) = self.owned_pids.lock() {
                    if is_asr {
                        pids.1 = pid;
                    } else {
                        pids.0 = pid;
                    }
                }
                if is_asr {
                    self.asr_view = view;
                } else {
                    self.chat_view = view;
                }
                drop(supervisor);
                match result {
                    Ok(caps) => {
                        if !is_asr {
                            self.capabilities = caps;
                            self.effort_profile = self
                                .capabilities
                                .as_ref()
                                .map(|caps| {
                                    verified_effort_profile(
                                        &self.chat_view.model,
                                        &self.models,
                                        caps,
                                    )
                                })
                                .unwrap_or(chat::EffortProfile::Unknown);
                            if self.chat_view.external
                                && let Some(caps) = &self.capabilities
                            {
                                for id in &caps.model_ids {
                                    if !self.models.iter().any(|m| &m.id == id) {
                                        self.models.push(ModelView { id: id.clone(), name: id.clone(), kind: "text".into(), served: true, compatible: true, detail: "Served by the explicitly connected external runtime".into(), ..Default::default() });
                                    }
                                }
                            }
                        } else if self.asr_view.external
                            && !self.models.iter().any(|m| m.id == self.asr_view.model)
                        {
                            self.models.push(ModelView { id: self.asr_view.model.clone(), name: self.asr_view.model.clone(), kind: "stt".into(), served: true, compatible: true, detail: "Served by the explicitly connected external transcription runtime".into(), ..Default::default() });
                        }
                        self.status = format!("{role} runtime updated · {model}");
                    }
                    Err(error) => {
                        self.error = Some(error);
                    }
                }
            }
            Event::RuntimeChecked { role, state, pid } => {
                if self.runtime_busy.contains(&role) {
                    return Ok(());
                }
                if let Ok(mut pids) = self.owned_pids.lock() {
                    if role == "asr" {
                        pids.1 = pid;
                    } else {
                        pids.0 = pid;
                    }
                }
                if role == "asr" {
                    self.asr_view = state;
                } else {
                    if state.state.starts_with("Failed") {
                        self.capabilities = None;
                    }
                    self.chat_view = state;
                }
            }
            Event::ModelOperation { key, result } => {
                self.operations.remove(&key);
                self.removals.remove(&key);
                let result = result.map_err(anyhow::Error::msg)?;
                self.status = if result.cancelled {
                    "Transfer cancelled. Incomplete entries remain unavailable.".into()
                } else if result.success {
                    result
                        .actual_reclaimed_bytes
                        .map(|bytes| format!("Model removal completed · reclaimed {bytes} bytes"))
                        .unwrap_or_else(|| "Model pull completed and locally validated.".into())
                } else {
                    self.error = Some(result.logs.join("\n"));
                    "Model operation failed.".into()
                };
                self.refresh_models()?;
            }
            Event::RemovalPreview(result) => {
                let preview = result.map_err(anyhow::Error::msg)?;
                self.preview_notice = Some((
                    preview.repository.clone(),
                    format!(
                        "Delete {} from {}?\n\nAll affected revisions:\n{}\n\nReclaimable bytes: {}. Shared retained blobs, sessions, and transcripts are preserved.",
                        preview.repository,
                        preview.cache_root.display(),
                        preview.revisions.join("\n"),
                        preview
                            .estimated_reclaimable_bytes
                            .map(|n| n.to_string())
                            .unwrap_or_else(|| "unknown until removal".into())
                    ),
                ));
                self.pending_removal = Some(preview);
            }
            Event::Mcp(connections, error) => {
                self.connections = connections;
                self.error = error;
                self.status = "MCP connection state updated.".into();
            }
            Event::Context { session, result } => {
                let text = result.map_err(anyhow::Error::msg)?;
                self.session_mut(&session)?.draft.push_str(&text);
                self.persist_session(&session)?;
                self.status = "Selected MCP context appended to the saved draft.".into();
            }
            Event::RecordingStarted => {
                self.status = "Recording microphone. Press Stop recording to transcribe.".into()
            }
            Event::Recorded { result, dictation } => {
                let cancelled = self
                    .active
                    .as_ref()
                    .is_some_and(|active| active.cancel.is_cancelled());
                self.recording = false;
                self.recorder = None;
                self.active = None;
                let outcome = if cancelled {
                    drop(result);
                    Err(anyhow!("Recording cancelled"))
                } else {
                    match result {
                        Ok(staged) => {
                            self.transcribe_to(
                                staged.path().to_owned(),
                                Some(staged),
                                dictation.clone(),
                            )
                            .await
                        }
                        Err(error) => Err(anyhow!(error)),
                    }
                };
                if let (Err(error), Some(target)) = (&outcome, dictation) {
                    self.dictation_error = Some((target.id, error.to_string()));
                }
                outcome?;
            }
            Event::Transcript {
                id,
                result,
                recording,
            } => {
                let cancelled = self
                    .active
                    .as_ref()
                    .is_some_and(|active| active.cancel.is_cancelled());
                self.active = None;
                let job = self
                    .jobs
                    .iter_mut()
                    .find(|j| j.id == id)
                    .context("Unknown transcription job")?;
                if let Some(recording) = recording {
                    job.source = match recording {
                        Ok(Some(path)) => path.display().to_string(),
                        Ok(None) => String::new(),
                        Err(error) => {
                            self.error = Some(error);
                            String::new()
                        }
                    };
                }
                match result {
                    Ok(audio::TranscriptionOutcome::Completed(transcript)) => {
                        job.text = transcript.text.clone();
                        job.elapsed_ms = Some(transcript.client_elapsed_ms);
                        job.status = if cancelled && job.dictation_session.is_some() {
                            "cancelled · result received"
                        } else {
                            "completed"
                        }
                        .into();
                        job.transcript = Some(transcript);
                    }
                    Ok(audio::TranscriptionOutcome::Cancelled {
                        server_outcome_unknown,
                    }) => {
                        job.status = if server_outcome_unknown {
                            "cancelled · server stop unconfirmed"
                        } else {
                            "cancelled"
                        }
                        .into();
                    }
                    Err(error) => {
                        job.status = "failed".into();
                        self.error = Some(error);
                    }
                }
                let saved = self.store.save_record(&job_record(job)?);
                if let Err(error) = &saved
                    && job.dictation_session.is_some()
                {
                    self.dictation_error = Some((id, error.to_string()));
                }
                saved?;
                self.status = "Transcription job finished; transcripts are saved locally.".into();
            }
        }
        Ok(())
    }

    fn check_services(&mut self) {
        for (role, service) in [("chat", self.chat.clone()), ("asr", self.asr.clone())] {
            let tx = self.events.clone();
            self.task_handles.push(tokio::spawn(async move {
                if let Ok(mut supervisor) = service.try_lock() {
                    let _ = supervisor.check().await;
                    let _ = tx
                        .send(Event::RuntimeChecked {
                            role: role.into(),
                            state: runtime_view(&supervisor),
                            pid: supervisor.owned_pid(),
                        })
                        .await;
                }
            }));
        }
        let manager = self.mcp.clone();
        let tx = self.events.clone();
        self.task_handles.push(tokio::spawn(async move {
            let changes = manager.refresh_changed().await;
            if !changes.is_empty() {
                let error = changes
                    .into_iter()
                    .find_map(|r| r.err())
                    .map(|e| e.to_string());
                let _ = tx
                    .send(Event::Mcp(manager.connections().await, error))
                    .await;
            }
        }));
    }

    fn snapshot(&self) -> Snapshot {
        let mut sessions: Vec<_> = self
            .sessions
            .iter()
            .map(persistence::session_view)
            .collect();
        if let Some(active) = &self.active
            && active.cancel.is_cancelled()
        {
            for session in &mut sessions {
                if Some(&session.id) == active.session.as_ref() {
                    for message in &mut session.messages {
                        if message.status == "streaming" || message.status == "pending" {
                            message.status = "cancelled locally · finalizing".into();
                        }
                    }
                }
            }
        }
        let mut snapshot = Snapshot {
            sessions,
            focused_session: self.focused_session.clone(),
            models: self.models.clone(),
            catalog_source: self.catalog_source.clone(),
            chat: self.chat_view.clone(),
            asr: self.asr_view.clone(),
            registrations: self
                .registrations
                .iter()
                .map(|r| mcp_bridge::view(r, self.connections.iter().find(|c| c.id == r.id)))
                .collect(),
            transcripts: self
                .jobs
                .iter()
                .map(|j| TranscriptView {
                    id: j.id.clone(),
                    text: j.text.clone(),
                    model: j.model.clone(),
                    status: j.status.clone(),
                    duration: j
                        .elapsed_ms
                        .map(|ms| format!("{:.2}s client elapsed", ms as f64 / 1000.0))
                        .unwrap_or_else(|| "Client elapsed unavailable".into()),
                    dictation_session: j.dictation_session.clone(),
                })
                .collect(),
            preferences: self.preferences.clone(),
            active: self.active.as_ref().map(|a| a.label.clone()),
            operations: self.operations.keys().cloned().collect(),
            status: self.status.clone(),
            error: self.error.clone(),
            state_root: self.paths.root.display().to_string(),
            ready: true,
            can_send: self.capabilities.is_some() && !self.runtime_busy.contains("chat"),
            recording: self.recording,
            dictation_error: self.dictation_error.clone(),
            deletion_preview: self.preview_notice.clone(),
            ..Default::default()
        };
        if let Ok(history) = self.metric_history.lock() {
            snapshot.metrics_stale = history.is_stale(Instant::now());
            snapshot.cpu_history = history
                .samples()
                .iter()
                .filter_map(|s| s.system_cpu_percent)
                .collect();
            if let Some(sample) = history.samples().back() {
                snapshot.sample_time =
                    chrono::DateTime::from_timestamp_millis(sample.timestamp_unix_ms)
                        .map(|date| {
                            date.with_timezone(&chrono::Local)
                                .format("Sampled %H:%M:%S")
                                .to_string()
                        })
                        .unwrap_or_else(|| "Sample time unavailable".into());
                snapshot.cpu = sample.system_cpu_percent;
                snapshot.memory = Some((sample.system_used_bytes, sample.system_total_bytes));
                snapshot.processes = sample
                    .processes
                    .iter()
                    .map(|p| {
                        (
                            format!("{} (CPU: 100% = one core)", p.role),
                            p.cpu_percent_one_core,
                            p.resident_bytes,
                        )
                    })
                    .collect();
            }
        }
        if matches!(self.effort_profile, chat::EffortProfile::ThinkingBudget) {
            snapshot.effort_levels = ["none", "minimal", "low", "medium", "high", "xhigh"]
                .iter()
                .map(|s| (*s).into())
                .collect();
            snapshot.effort_note = "Effort maps to a thinking-token budget (minimal 256 · low 512 · medium 2048 · high 8192 · xhigh 24000); none disables thinking.".into();
        }
        snapshot
    }

    async fn shutdown(&mut self) {
        if let Some(active) = &self.active {
            active.cancel.cancel();
        }
        if let Some(recorder) = self.recorder.take() {
            let _ = recorder.send(false);
        }
        for token in self.operations.values() {
            token.cancel();
        }
        // Give cancellation handlers time to reap command groups and commit their
        // terminal outcomes. Aborting immediately skips asynchronous cleanup.
        let deadline = tokio::time::Instant::now() + Duration::from_secs(6);
        while self.task_handles.iter().any(|task| !task.is_finished())
            && tokio::time::Instant::now() < deadline
        {
            tokio::select! {
                Some(event) = self.turn_rx.recv() => self.turn_event(event),
                Some(event) = self.event_rx.recv() => {
                    // A completed capture must not launch a new transcription
                    // while closing. Dropping its owned staging cleans it up.
                    if let event @ Event::Transcript { .. } = event
                        && let Err(error) = self.event(event).await {
                        self.error = Some(error.to_string());
                    }
                }
                _ = tokio::time::sleep(Duration::from_millis(20)) => {}
            }
        }
        while let Ok(event) = self.turn_rx.try_recv() {
            self.turn_event(event);
        }
        while let Ok(event) = self.event_rx.try_recv() {
            if let event @ Event::Transcript { .. } = event {
                let _ = self.event(event).await;
            }
        }
        for task in self.task_handles.drain(..) {
            task.abort();
            let _ = task.await;
        }
        for session in &mut self.sessions {
            session.recover_interrupted();
            let _ = self.store.save_session(session);
        }
        self.checkpoint();
        let _ = tokio::time::timeout(Duration::from_secs(10), self.mcp.disconnect_all()).await;
        for service in [&self.chat, &self.asr] {
            let mut supervisor = service.lock().await;
            supervisor.set_inference_active(false);
            let _ = supervisor.stop().await;
        }
        self.sampling_cancel.cancel();
        if let Some(thread) = self.sampler.take() {
            let _ = thread.join();
        }
    }
}

fn runtime_view(supervisor: &runtime::RuntimeSupervisor) -> RuntimeView {
    let (state, model, external) = match supervisor.state() {
        runtime::RuntimeState::Ready {
            model, external, ..
        } => ("Ready".into(), model.clone(), *external),
        runtime::RuntimeState::Failed { message } => {
            (format!("Failed: {message}"), String::new(), false)
        }
        other => (format!("{other:?}"), String::new(), false),
    };
    RuntimeView {
        state,
        model,
        external,
        logs: supervisor.logs(),
    }
}

fn model_views(
    available: &[catalog::AvailableModel],
    local: &[catalog::LocalModel],
) -> Vec<ModelView> {
    let mut views = Vec::new();
    for model in local {
        let known = available
            .iter()
            .find(|a| model.repository.as_deref() == Some(&a.hf_path));
        let repo = model.repository.clone().unwrap_or_default();
        let suffix = model.cache_root.as_ref().and_then(|root| {
            let root = root.canonicalize().unwrap_or_else(|_| root.clone());
            let revision = model.revision.as_ref()?;
            let checkpoint = root
                .join(format!("models--{}", repo.replace('/', "--")))
                .join("snapshots")
                .join(revision);
            model
                .load_path
                .strip_prefix(checkpoint)
                .ok()
                .filter(|p| !p.as_os_str().is_empty())
                .map(|p| p.display().to_string())
        });
        let id = if let (Some(repo), Some(revision)) = (&model.repository, &model.revision) {
            format!(
                "{repo}@{revision}{}",
                suffix.as_ref().map(|s| format!("/{s}")).unwrap_or_default()
            )
        } else {
            model.load_path.display().to_string()
        };
        let kind = if known.is_some_and(|a| a.is_asr())
            || catalog::local_asr_family(&model.load_path)
                .ok()
                .flatten()
                .is_some()
        {
            "stt".into()
        } else if let Some(known) = known {
            if known.modality == "audio" {
                known.kind.clone().unwrap_or_else(|| "audio".into())
            } else {
                known.modality.clone()
            }
        } else {
            "text".into()
        };
        let mut name = model.repository.clone().unwrap_or_else(|| {
            model
                .load_path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned()
        });
        if let Some(suffix) = &suffix {
            name.push_str(&format!(" · {suffix}"));
        }
        if let Some(revision) = &model.revision {
            name.push_str(&format!(
                " · {}",
                revision.chars().take(8).collect::<String>()
            ));
        }
        let supported_stt = catalog::supported_asr_repository(&repo).is_some();
        let unsupported_stt = kind == "stt" && !supported_stt;
        let runnable = kind == "text" || kind == "stt" && supported_stt;
        views.push(ModelView {
            id,
            name,
            repo,
            revision: model.revision.clone().unwrap_or_default(),
            path: model.load_path.clone(),
            kind,
            size: format!(
                "{:.2} GiB logical weights",
                model.logical_weight_bytes as f64 / 1_073_741_824.0
            ),
            pulled: true,
            compatible: model.can_attempt_load() && runnable,
            detail: format!(
                "{:?}{}",
                model.compatibility,
                if unsupported_stt {
                    " · STT unavailable: only tested Qwen3 0.6B 5-bit and Nemotron 3.5 streaming 0.6B are supported"
                } else if runnable {
                    ""
                } else {
                    " · inventory only; chat and transcription have separate supported task types"
                }
            ),
            ..Default::default()
        });
    }
    for model in available {
        if model.modality == "audio" && model.kind.as_deref() == Some("stt") && !model.is_asr() {
            continue;
        }
        let pulled = local
            .iter()
            .any(|l| l.repository.as_deref() == Some(&model.hf_path));
        let kind = if model.modality == "audio" {
            model.kind.clone().unwrap_or_else(|| "audio".into())
        } else {
            model.modality.clone()
        };
        views.push(ModelView {
            id: format!("alias:{}", model.alias),
            name: model.alias.clone(),
            repo: model.hf_path.clone(),
            kind,
            size: model
                .size_bytes
                .map(|n| format!("{:.2} GiB download", n as f64 / 1_073_741_824.0))
                .unwrap_or_else(|| "Size unavailable".into()),
            pulled,
            catalog: true,
            compatible: false,
            detail: if pulled {
                "Catalog entry · choose an exact revision from Pulled models"
            } else {
                "Runtime catalog · explicit Pull required"
            }
            .into(),
            ..Default::default()
        });
    }
    views
}

fn usage(raw: Value) -> domain::Usage {
    domain::Usage {
        prompt_tokens: raw["prompt_tokens"].as_u64(),
        completion_tokens: raw["completion_tokens"].as_u64(),
        total_tokens: raw["total_tokens"].as_u64(),
        reasoning_tokens: raw
            .pointer("/completion_tokens_details/reasoning_tokens")
            .and_then(Value::as_u64),
        raw,
        ..Default::default()
    }
}

fn tool_policy(settings: &ToolSettings) -> Result<tools::ToolPolicy> {
    if !settings.enabled {
        return Ok(tools::ToolPolicy::default());
    }
    let root = PathBuf::from(&settings.directory)
        .canonicalize()
        .context("Choose an existing tool working directory")?;
    let mut enabled: BTreeSet<String> = tools::definitions()
        .iter()
        .filter_map(|d| {
            d.pointer("/function/name")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .collect();
    enabled.retain(|name| {
        (settings.writes || !matches!(name.as_str(), "os_write" | "os_create"))
            && (settings.commands || name != "os_command")
    });
    let commands = if settings.commands {
        settings
            .executables
            .lines()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|path| {
                let executable = PathBuf::from(path);
                ensure!(
                    executable.is_absolute() && executable.is_file(),
                    "Allowed executables must be existing absolute files"
                );
                Ok(tools::CommandGrant {
                    executable,
                    args_prefix: vec![],
                    allow_extra_args: true,
                    mutates: true,
                })
            })
            .collect::<Result<Vec<_>>>()?
    } else {
        vec![]
    };
    Ok(tools::ToolPolicy {
        enabled,
        roots: vec![root.clone()],
        working_dir: root,
        allow_mutations: settings.writes || settings.commands,
        commands,
        ..Default::default()
    })
}

fn verified_effort_profile(
    id: &str,
    models: &[ModelView],
    caps: &chat::Capabilities,
) -> chat::EffortProfile {
    if caps.runtime_version.as_deref() != Some("1.0.2") {
        return chat::EffortProfile::Unknown;
    }
    let Some(model) = models
        .iter()
        .find(|m| m.id == id && !m.path.as_os_str().is_empty())
    else {
        return chat::EffortProfile::Unknown;
    };
    let path = model.path.join("tokenizer_config.json");
    let Ok(meta) = std::fs::metadata(&path) else {
        return chat::EffortProfile::Unknown;
    };
    if meta.len() > 2 * 1024 * 1024 {
        return chat::EffortProfile::Unknown;
    }
    let Ok(text) = std::fs::read_to_string(path) else {
        return chat::EffortProfile::Unknown;
    };
    let Ok(config) = serde_json::from_str::<Value>(&text) else {
        return chat::EffortProfile::Unknown;
    };
    let template = config["chat_template"].as_str().unwrap_or_default();
    if template.contains("reasoning_effort") {
        return chat::EffortProfile::Unknown;
    } // Native ladders require explicit template evidence, never guessed.
    if template.contains("enable_thinking") && caps.has_request_field("reasoning_effort") {
        chat::EffortProfile::ThinkingBudget
    } else {
        chat::EffortProfile::Unknown
    }
}

fn job_record(job: &Job) -> Result<Record> {
    let mut record = Record::new("transcription", &job.id, serde_json::to_value(job)?);
    record.status = Some(match job.status.as_str() {
        "transcribing" => RecordStatus::Running,
        "completed" => RecordStatus::Completed,
        "interrupted" => RecordStatus::Interrupted,
        status if status.starts_with("cancelled") => RecordStatus::Cancelled,
        _ => RecordStatus::Failed,
    });
    Ok(record)
}

fn instance_lock(paths: &AppPaths) -> Result<std::fs::File> {
    let path = paths.root.join("instance.lock");
    if let Ok(metadata) = std::fs::symlink_metadata(&path) {
        ensure!(
            metadata.is_file() && !metadata.file_type().is_symlink(),
            "Invalid instance lock file"
        );
    }
    let mut options = std::fs::OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options.open(path)?;
    file.try_lock()
        .context("Another Rapid MLX UI instance is already using this state root")?;
    Ok(file)
}

fn cleanup_staging(paths: &AppPaths) -> Result<()> {
    // Only files with the recorder's exclusively created UUID.wav naming scheme are ours.
    for entry in std::fs::read_dir(&paths.audio_tmp)? {
        let entry = entry?;
        let path = entry.path();
        if entry.file_type()?.is_file()
            && path.extension().is_some_and(|ext| ext == "wav")
            && path
                .file_stem()
                .and_then(|s| s.to_str())
                .is_some_and(|name| uuid::Uuid::parse_str(name).is_ok())
        {
            std::fs::remove_file(path)?;
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "backend_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "integration_tests.rs"]
mod integration_tests;
