//! Independent MCP client backend. Registrations contain references, never credential values.
use rmcp::{
    ClientHandler, ClientLifecycleMode, ClientServiceExt, RoleClient,
    model::*,
    service::{NotificationContext, Peer, RunningService, ServiceError},
    transport::{
        StreamableHttpClientTransport, TokioChildProcess,
        streamable_http_client::StreamableHttpClientTransportConfig,
    },
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet, HashSet},
    process::Stdio,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::sync::{Mutex, RwLock};
use tokio_util::sync::CancellationToken;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const CLOSE_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_PAGES: usize = 128;
const MAX_ITEMS: usize = 4096;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct McpRegistration {
    pub id: String,
    pub name: String,
    pub enabled: bool,
    pub transport: McpTransport,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum McpTransport {
    Stdio {
        command: String,
        #[serde(default)]
        args: Vec<String>,
        #[serde(default)]
        env: BTreeMap<String, String>,
    },
    StreamableHttp {
        url: String,
        #[serde(default)]
        bearer_token_env: Option<String>,
    },
}
impl McpRegistration {
    pub fn new(name: impl Into<String>, transport: McpTransport) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            name: name.into(),
            enabled: true,
            transport,
        }
    }
    pub fn validate(&self) -> Result<(), McpError> {
        if uuid::Uuid::parse_str(&self.id).is_err() || self.name.trim().is_empty() {
            return Err(McpError::invalid(
                "Registration requires a stable UUID and nonempty name.",
            ));
        }
        match &self.transport {
            McpTransport::Stdio { command, args, env } => {
                if command.trim().is_empty()
                    || command.contains('\0')
                    || args.iter().any(|a| a.contains('\0'))
                    || env.iter().any(|(k, v)| !valid_env(k) || !valid_env(v))
                {
                    return Err(McpError::invalid(
                        "Stdio requires an executable, valid arguments, and environment variable references.",
                    ));
                }
            }
            McpTransport::StreamableHttp {
                url,
                bearer_token_env,
            } => {
                validate_mcp_url(url)?;
                if bearer_token_env.as_ref().is_some_and(|r| !valid_env(r)) {
                    return Err(McpError::invalid(
                        "Credential reference must be an environment variable name.",
                    ));
                }
            }
        }
        Ok(())
    }
}
fn valid_env(s: &str) -> bool {
    !s.is_empty()
        && s.bytes()
            .enumerate()
            .all(|(i, b)| b == b'_' || b.is_ascii_alphabetic() || (i > 0 && b.is_ascii_digit()))
}
/// Separate from inference validation: HTTPS may be remote; plain HTTP must be loopback.
/// URL credentials, fragments and query parameters are rejected to avoid persisting tokens.
pub fn validate_mcp_url(raw: &str) -> Result<reqwest::Url, McpError> {
    let url = reqwest::Url::parse(raw)
        .map_err(|_| McpError::invalid("MCP endpoint must be an absolute HTTP(S) URL."))?;
    let loopback = url.host_str().is_some_and(|host| {
        host.eq_ignore_ascii_case("localhost")
            || host
                .trim_matches(['[', ']'])
                .parse::<std::net::IpAddr>()
                .is_ok_and(|ip| ip.is_loopback())
    });
    if url.host().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || !(url.scheme() == "https" || (url.scheme() == "http" && loopback))
    {
        return Err(McpError::invalid(
            "Use remote HTTPS or loopback HTTP, without URL credentials, query parameters, or fragments. Use a bearer credential reference for authentication.",
        ));
    }
    Ok(url)
}
/// Deliberately neither Serialize nor Debug. Drop after connect; SDK holds only the active token.
#[derive(Default)]
pub struct McpCredentials {
    bearer_token: Option<String>,
    env: BTreeMap<String, String>,
}
impl McpCredentials {
    pub fn with_bearer_token(mut self, token: impl Into<String>) -> Self {
        self.bearer_token = Some(token.into());
        self
    }
    /// Current-run child environment override, keyed by configured child variable name.
    pub fn with_env(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.insert(name.into(), value.into());
        self
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum McpConnectionState {
    Disconnected,
    Connecting,
    Connected,
    Failed,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum McpErrorKind {
    InvalidRegistration,
    Disabled,
    NotConnected,
    Authentication,
    Protocol,
    Timeout,
    Cancelled,
    Transport,
    InvalidArguments,
    NotSelected,
    Provider,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct McpError {
    pub kind: McpErrorKind,
    pub message: String,
    pub outcome_unknown: bool,
}
impl McpError {
    fn new(kind: McpErrorKind, message: &str, outcome_unknown: bool) -> Self {
        Self {
            kind,
            message: message.into(),
            outcome_unknown,
        }
    }
    fn invalid(message: &str) -> Self {
        Self::new(McpErrorKind::InvalidRegistration, message, false)
    }
    fn disconnected() -> Self {
        Self::new(
            McpErrorKind::NotConnected,
            "MCP server is disconnected; connect explicitly before use.",
            false,
        )
    }
}
impl std::fmt::Display for McpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}
impl std::error::Error for McpError {}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct McpTool {
    pub id: String,
    pub server_id: String,
    pub server_name: String,
    pub name: String,
    pub description: Option<String>,
    pub input_schema: Value,
}
impl McpTool {
    pub fn openai_definition(&self) -> Value {
        let mut function = json!({"name":self.id, "parameters":self.input_schema});
        if let Some(description) = &self.description {
            function["description"] = Value::String(description.clone());
        }
        json!({"type":"function", "function":function})
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct McpConnectionInfo {
    pub id: String,
    pub name: String,
    pub state: McpConnectionState,
    pub protocol_version: Option<String>,
    pub server_info: Option<Value>,
    pub tools: Vec<McpTool>,
    pub resources: Vec<Value>,
    pub prompts: Vec<Value>,
    pub discovery_changed: bool,
    pub error: Option<McpError>,
}
#[derive(Clone)]
struct Handler {
    dirty: Arc<AtomicBool>,
}
impl ClientHandler for Handler {
    async fn on_tool_list_changed(&self, _: NotificationContext<RoleClient>) {
        self.dirty.store(true, Ordering::Release);
    }
    async fn on_resource_list_changed(&self, _: NotificationContext<RoleClient>) {
        self.dirty.store(true, Ordering::Release);
    }
    async fn on_prompt_list_changed(&self, _: NotificationContext<RoleClient>) {
        self.dirty.store(true, Ordering::Release);
    }
}
struct Connection {
    registration: McpRegistration,
    info: RwLock<McpConnectionInfo>,
    service: Mutex<Option<RunningService<RoleClient, Handler>>>,
    peer: RwLock<Option<Peer<RoleClient>>>,
    dirty: Arc<AtomicBool>,
    stop: CancellationToken,
    refresh_lock: Mutex<()>,
    subscription: Mutex<Option<tokio::task::JoinHandle<()>>>,
}
impl Connection {
    fn new(registration: McpRegistration) -> Self {
        Self {
            info: RwLock::new(McpConnectionInfo {
                id: registration.id.clone(),
                name: registration.name.clone(),
                state: McpConnectionState::Connecting,
                protocol_version: None,
                server_info: None,
                tools: vec![],
                resources: vec![],
                prompts: vec![],
                discovery_changed: false,
                error: None,
            }),
            registration,
            service: Mutex::new(None),
            peer: RwLock::new(None),
            dirty: Arc::new(AtomicBool::new(false)),
            stop: CancellationToken::new(),
            refresh_lock: Mutex::new(()),
            subscription: Mutex::new(None),
        }
    }
    async fn snapshot(&self) -> McpConnectionInfo {
        let mut info = self.info.read().await.clone();
        info.discovery_changed = self.dirty.load(Ordering::Acquire);
        if info.state == McpConnectionState::Connected
            && self
                .peer
                .read()
                .await
                .as_ref()
                .is_none_or(|p| p.is_transport_closed())
        {
            info.state = McpConnectionState::Failed;
            info.error = Some(McpError::disconnected());
        }
        info
    }
    async fn close(&self) {
        self.stop.cancel();
        if let Some(task) = self.subscription.lock().await.take() {
            task.abort();
        }
        self.peer.write().await.take();
        if let Some(mut service) = self.service.lock().await.take() {
            let _ = service.close_with_timeout(CLOSE_TIMEOUT).await;
        }
    }
}
/// Cloneable handle; creation and registration restoration perform no network/process work.
#[derive(Clone, Default)]
pub struct McpManager {
    connections: Arc<RwLock<BTreeMap<String, Arc<Connection>>>>,
}
impl McpManager {
    pub fn new() -> Self {
        Self::default()
    }
    pub async fn connections(&self) -> Vec<McpConnectionInfo> {
        let entries: Vec<_> = self.connections.read().await.values().cloned().collect();
        let mut result = Vec::new();
        for entry in entries {
            result.push(entry.snapshot().await);
        }
        result
    }
    pub async fn connect(
        &self,
        registration: McpRegistration,
        credentials: McpCredentials,
    ) -> Result<McpConnectionInfo, McpError> {
        registration.validate()?;
        if !registration.enabled {
            return Err(McpError::new(
                McpErrorKind::Disabled,
                "Enable this MCP registration before connecting.",
                false,
            ));
        }
        let connection = Arc::new(Connection::new(registration));
        let old = self
            .connections
            .write()
            .await
            .insert(connection.registration.id.clone(), connection.clone());
        if let Some(old) = old {
            old.close().await;
        }
        let result = tokio::select! {
            biased;
            _ = connection.stop.cancelled() => Err(McpError::disconnected()),
            result = tokio::time::timeout(REQUEST_TIMEOUT, self.start(&connection, credentials)) => result.unwrap_or_else(|_| Err(McpError::new(McpErrorKind::Timeout, "MCP connection/discovery timed out; check server startup and protocol support.", false))),
        };
        if let Err(error) = result {
            connection.close().await;
            let mut info = connection.info.write().await;
            if info.state != McpConnectionState::Disconnected {
                info.state = McpConnectionState::Failed;
                info.error = Some(error.clone());
            }
            return Err(error);
        }
        Ok(connection.snapshot().await)
    }
    async fn start(
        &self,
        c: &Arc<Connection>,
        credentials: McpCredentials,
    ) -> Result<(), McpError> {
        let handler = Handler {
            dirty: c.dirty.clone(),
        };
        let service = match &c.registration.transport {
            McpTransport::Stdio { command, args, env } => {
                let mut cmd = tokio::process::Command::new(command);
                cmd.args(args).kill_on_drop(true).stderr(Stdio::null());
                // Inherit ordinary execution environment, but resolve only explicitly configured references.
                for (name, reference) in env { cmd.env(name, match credentials.env.get(name) { Some(value) => value.clone(), None => resolve_env(reference)? }); }
                let transport = TokioChildProcess::builder(cmd).stderr(Stdio::null()).spawn().map_err(|_| McpError::new(McpErrorKind::Transport, "Cannot launch MCP executable; check executable path and permissions.", false))?.0;
                handler.serve_with_lifecycle(transport, client_lifecycle()).await
            }
            McpTransport::StreamableHttp { url, bearer_token_env } => {
                let token = match credentials.bearer_token { Some(token) => Some(token), None => bearer_token_env.as_deref().map(resolve_env).transpose()? };
                let mut config = StreamableHttpClientTransportConfig::with_uri(url.clone()).reinit_on_expired_session(false).max_sse_event_size(1024 * 1024);
                if let Some(token) = token {
                    if token.trim().is_empty() || reqwest::header::HeaderValue::from_str(&format!("Bearer {token}")).is_err() { return Err(McpError::new(McpErrorKind::Authentication, "Bearer credential is empty or invalid; enter a current-run token or fix its environment reference.", false)); }
                    config = config.auth_header(token);
                }
                let client = reqwest::Client::builder().redirect(reqwest::redirect::Policy::none()).connect_timeout(Duration::from_secs(10)).build().map_err(|_| McpError::new(McpErrorKind::Transport, "Cannot initialize MCP HTTP client.", false))?;
                handler.serve_with_lifecycle(StreamableHttpClientTransport::with_client(client, config), client_lifecycle()).await
            }
        }.map_err(|error| {
            if error.auth_challenge().is_some() {
                McpError::new(McpErrorKind::Authentication, "MCP authentication was required or rejected. Supply a valid bearer token; OAuth/browser login and challenge-driven authentication are unsupported.", false)
            } else {
                McpError::new(McpErrorKind::Protocol, "MCP handshake failed. Check protocol compatibility, URL and credentials. Only preconfigured bearer tokens are supported; OAuth/browser login and challenge-driven authentication are unavailable.", false)
            }
        })?;
        let peer = service.peer().clone();
        let negotiated = peer.peer_info().ok_or_else(|| {
            McpError::new(
                McpErrorKind::Protocol,
                "MCP server did not supply negotiated capabilities.",
                false,
            )
        })?;
        {
            let mut info = c.info.write().await;
            info.protocol_version = Some(negotiated.protocol_version.to_string());
            info.server_info = Some(serde_json::to_value(&*negotiated).unwrap_or(Value::Null));
        }
        *c.peer.write().await = Some(peer.clone());
        *c.service.lock().await = Some(service);
        // New protocol versions require explicit list-change subscriptions; legacy callbacks work directly.
        if negotiated.protocol_version >= ProtocolVersion::V_2026_07_28 {
            let caps = &negotiated.capabilities;
            let mut filter = SubscriptionFilter::default();
            filter.tools_list_changed = caps.tools.as_ref().and_then(|v| v.list_changed);
            filter.prompts_list_changed = caps.prompts.as_ref().and_then(|v| v.list_changed);
            filter.resources_list_changed = caps.resources.as_ref().and_then(|v| v.list_changed);
            if filter.tools_list_changed == Some(true)
                || filter.prompts_list_changed == Some(true)
                || filter.resources_list_changed == Some(true)
            {
                let mut subscription = peer
                    .listen(filter)
                    .await
                    .map_err(|e| map_service_error(e, false))?;
                let dirty = c.dirty.clone();
                let stop = c.stop.clone();
                *c.subscription.lock().await = Some(tokio::spawn(async move {
                    loop {
                        tokio::select! { _ = stop.cancelled() => break, notification = subscription.next() => { match notification { Ok(Some(_)) => { dirty.store(true, Ordering::Release); }, _ => { dirty.store(true, Ordering::Release); break; } } } }
                    }
                }));
            }
        }
        discover(c).await?;
        c.info.write().await.state = McpConnectionState::Connected;
        Ok(())
    }
    pub async fn disconnect(&self, id: &str) -> Result<(), McpError> {
        let c = self.connection(id).await?;
        {
            let mut info = c.info.write().await;
            info.state = McpConnectionState::Disconnected;
            info.tools.clear();
            info.resources.clear();
            info.prompts.clear();
        }
        c.close().await;
        Ok(())
    }
    pub async fn disconnect_all(&self) {
        let ids: Vec<_> = self.connections.read().await.keys().cloned().collect();
        for id in ids {
            let _ = self.disconnect(&id).await;
        }
    }
    async fn connection(&self, id: &str) -> Result<Arc<Connection>, McpError> {
        self.connections
            .read()
            .await
            .get(id)
            .cloned()
            .ok_or_else(McpError::disconnected)
    }
    pub async fn refresh(&self, id: &str) -> Result<McpConnectionInfo, McpError> {
        let c = self.connection(id).await?;
        let result = tokio::select! { _ = c.stop.cancelled() => Err(McpError::disconnected()), result = tokio::time::timeout(REQUEST_TIMEOUT, discover(&c)) => result.unwrap_or_else(|_| Err(McpError::new(McpErrorKind::Timeout, "MCP discovery timed out.", false))) };
        if let Err(error) = result {
            c.dirty.store(true, Ordering::Release);
            c.info.write().await.error = Some(error.clone());
            return Err(error);
        }
        Ok(c.snapshot().await)
    }
    /// Call on the application worker when a snapshot reports discovery_changed.
    pub async fn refresh_changed(&self) -> Vec<Result<McpConnectionInfo, McpError>> {
        let entries: Vec<_> = self.connections.read().await.values().cloned().collect();
        let mut result = vec![];
        for c in entries {
            if c.dirty.load(Ordering::Acquire) && !c.stop.is_cancelled() {
                result.push(self.refresh(&c.registration.id).await);
            }
        }
        result
    }
    pub async fn selected_tools(&self, selected: &BTreeSet<String>) -> Vec<McpTool> {
        self.connections()
            .await
            .into_iter()
            .filter(|c| {
                c.state == McpConnectionState::Connected
                    && c.error.is_none()
                    && !c.discovery_changed
            })
            .flat_map(|c| c.tools)
            .filter(|t| selected.contains(&t.id))
            .collect()
    }
    pub async fn call_selected_tool(
        &self,
        selected: &BTreeSet<String>,
        id: &str,
        arguments: Value,
        cancellation: CancellationToken,
    ) -> Result<CallToolResult, McpError> {
        if !selected.contains(id) {
            return Err(McpError::new(
                McpErrorKind::NotSelected,
                "MCP tool is not selected for this session.",
                false,
            ));
        }
        self.call_tool(id, arguments, cancellation).await
    }
    /// Caller must enforce session policy and durably record intent before dispatch.
    pub async fn call_tool(
        &self,
        id: &str,
        arguments: Value,
        cancellation: CancellationToken,
    ) -> Result<CallToolResult, McpError> {
        let mut found = None;
        for info in self.connections().await {
            if info.state == McpConnectionState::Connected
                && info.error.is_none()
                && !info.discovery_changed
                && let Some(tool) = info.tools.into_iter().find(|t| t.id == id)
            {
                found = Some(tool);
                break;
            }
        }
        let tool = found.ok_or_else(|| {
            McpError::new(
                McpErrorKind::NotConnected,
                "MCP tool is unavailable; refresh discovery and select it explicitly.",
                false,
            )
        })?;
        validate_arguments(&tool.input_schema, &arguments)?;
        let args = arguments.as_object().cloned().ok_or_else(|| {
            McpError::new(
                McpErrorKind::InvalidArguments,
                "MCP tool arguments must be a JSON object.",
                false,
            )
        })?;
        if cancellation.is_cancelled() {
            return Err(McpError::new(
                McpErrorKind::Cancelled,
                "MCP call cancelled before dispatch.",
                false,
            ));
        }
        let c = self.connection(&tool.server_id).await?;
        let peer = active_peer(&c).await?;
        tokio::select! { biased;
            _ = cancellation.cancelled() => Err(McpError::new(McpErrorKind::Cancelled, "MCP call cancelled; side effects may have occurred. Do not retry automatically.", true)),
            _ = c.stop.cancelled() => Err(McpError::new(McpErrorKind::Transport, "MCP disconnected during call; outcome unknown. Do not retry automatically.", true)),
            result = tokio::time::timeout(REQUEST_TIMEOUT, peer.call_tool_once(CallToolRequestParams::new(tool.name).with_arguments(args))) => result.map_err(|_| McpError::new(McpErrorKind::Timeout, "MCP tool timed out; outcome unknown. Do not retry automatically.", true))?.map_err(|e| map_service_error(e, true)).and_then(|r| match r { CallToolResponse::Complete(result) => Ok(result), _ => Err(McpError::new(McpErrorKind::Protocol, "MCP tool requested unsupported interactive input; no continuation or retry was sent.", true)) }),
        }
    }
    pub async fn read_resource(
        &self,
        server_id: &str,
        uri: &str,
    ) -> Result<ReadResourceResult, McpError> {
        let c = self.connection(server_id).await?;
        if !c
            .info
            .read()
            .await
            .resources
            .iter()
            .any(|v| v["uri"].as_str() == Some(uri))
        {
            return Err(McpError::new(
                McpErrorKind::NotSelected,
                "Resource is not in current discovery; refresh and select an advertised resource.",
                false,
            ));
        }
        let peer = active_peer(&c).await?;
        tokio::select! { _ = c.stop.cancelled() => Err(McpError::disconnected()), result = tokio::time::timeout(REQUEST_TIMEOUT, peer.read_resource(ReadResourceRequestParams::new(uri))) => result.map_err(|_| McpError::new(McpErrorKind::Timeout, "MCP resource read timed out.", false))?.map_err(|e| map_service_error(e, false)) }
    }
    pub async fn get_prompt(
        &self,
        server_id: &str,
        name: &str,
        arguments: Value,
    ) -> Result<GetPromptResult, McpError> {
        let c = self.connection(server_id).await?;
        if !c
            .info
            .read()
            .await
            .prompts
            .iter()
            .any(|v| v["name"].as_str() == Some(name))
        {
            return Err(McpError::new(
                McpErrorKind::NotSelected,
                "Prompt is not in current discovery; refresh and select an advertised prompt.",
                false,
            ));
        }
        let args = arguments
            .as_object()
            .cloned()
            .filter(|a| a.values().all(Value::is_string))
            .ok_or_else(|| {
                McpError::new(
                    McpErrorKind::InvalidArguments,
                    "MCP prompt arguments must be an object of strings.",
                    false,
                )
            })?;
        let peer = active_peer(&c).await?;
        tokio::select! { _ = c.stop.cancelled() => Err(McpError::disconnected()), result = tokio::time::timeout(REQUEST_TIMEOUT, peer.get_prompt(GetPromptRequestParams::new(name).with_arguments(args))) => result.map_err(|_| McpError::new(McpErrorKind::Timeout, "MCP prompt request timed out.", false))?.map_err(|e| map_service_error(e, false)) }
    }
}
fn client_lifecycle() -> ClientLifecycleMode {
    ClientLifecycleMode::Auto {
        preferred_versions: ProtocolVersion::KNOWN_VERSIONS
            .iter()
            .rev()
            .cloned()
            .collect(),
        legacy_version: Some(ProtocolVersion::V_2025_11_25),
    }
}

fn resolve_env(reference: &str) -> Result<String, McpError> {
    std::env::var(reference).map_err(|_| McpError::new(McpErrorKind::Authentication, "Configured environment reference is missing or not Unicode; set it before launching the app or enter a current-run credential.", false))
}
async fn active_peer(c: &Connection) -> Result<Peer<RoleClient>, McpError> {
    if c.stop.is_cancelled() {
        return Err(McpError::disconnected());
    }
    c.peer
        .read()
        .await
        .clone()
        .filter(|p| !p.is_transport_closed())
        .ok_or_else(McpError::disconnected)
}
fn map_service_error(error: ServiceError, dispatched: bool) -> McpError {
    match error {
        ServiceError::McpError(_) => McpError::new(
            McpErrorKind::Provider,
            "MCP provider rejected the operation. Inspect the server configuration; the call was not retried.",
            false,
        ),
        ServiceError::UnexpectedResponse | ServiceError::InputRequiredRoundsExceeded { .. } => {
            McpError::new(
                McpErrorKind::Protocol,
                "MCP response is unsupported or malformed; interactive input is unavailable. No retry was made.",
                dispatched,
            )
        }
        _ => McpError::new(
            McpErrorKind::Transport,
            "MCP transport failed. Check connection and authentication; OAuth/challenge flows are unsupported. No retry was made.",
            dispatched,
        ),
    }
}
fn validate_arguments(schema: &Value, arguments: &Value) -> Result<(), McpError> {
    // Only document-local references: schema validation must never fetch remote resources.
    fn external_ref(v: &Value) -> bool {
        match v {
            Value::Object(o) => o.iter().any(|(k, v)| {
                ((k == "$ref" || k == "$dynamicRef")
                    && v.as_str().is_some_and(|s| !s.starts_with('#')))
                    || external_ref(v)
            }),
            Value::Array(a) => a.iter().any(external_ref),
            _ => false,
        }
    }
    if external_ref(schema) {
        return Err(McpError::new(
            McpErrorKind::InvalidArguments,
            "Tool schema uses unsupported external references; no operation was dispatched.",
            false,
        ));
    }
    let validator = jsonschema::validator_for(schema).map_err(|_| {
        McpError::new(
            McpErrorKind::InvalidArguments,
            "Provider tool schema is invalid or unsupported; no operation was dispatched.",
            false,
        )
    })?;
    if !validator.is_valid(arguments) {
        return Err(McpError::new(
            McpErrorKind::InvalidArguments,
            "Tool arguments do not match the advertised JSON schema; no operation was dispatched.",
            false,
        ));
    }
    Ok(())
}
/// Stable UUID-v5 namespacing never aliases a different tool after discovery changes.
fn tool_id(server_id: &str, name: &str) -> String {
    let namespace = uuid::Uuid::parse_str(server_id).expect("validated registration UUID");
    format!(
        "mcp_{}",
        uuid::Uuid::new_v5(&namespace, name.as_bytes()).simple()
    )
}
async fn discover(c: &Connection) -> Result<(), McpError> {
    let _guard = c.refresh_lock.lock().await;
    let peer = active_peer(c).await?;
    let caps = peer
        .peer_info()
        .ok_or_else(McpError::disconnected)?
        .capabilities
        .clone();
    c.dirty.store(false, Ordering::Release);
    let mut tools = vec![];
    let mut resources = vec![];
    let mut prompts = vec![];
    for category in 0..3 {
        if (category == 0 && caps.tools.is_none())
            || (category == 1 && caps.resources.is_none())
            || (category == 2 && caps.prompts.is_none())
        {
            continue;
        }
        let mut cursor = None;
        let mut seen = HashSet::new();
        let mut done = false;
        for _ in 0..MAX_PAGES {
            let mut page = PaginatedRequestParams::default();
            page.cursor = cursor;
            let params = Some(page);
            // Typed SDK requests bypass its stale-response cache on explicit Refresh.
            let request = match category {
                0 => ClientRequest::ListToolsRequest(ListToolsRequest {
                    method: Default::default(),
                    params,
                    extensions: Default::default(),
                }),
                1 => ClientRequest::ListResourcesRequest(ListResourcesRequest {
                    method: Default::default(),
                    params,
                    extensions: Default::default(),
                }),
                _ => ClientRequest::ListPromptsRequest(ListPromptsRequest {
                    method: Default::default(),
                    params,
                    extensions: Default::default(),
                }),
            };
            let result = peer
                .send_request(request)
                .await
                .map_err(|e| map_service_error(e, false))?;
            cursor = match (category, result) {
                (0, ServerResult::ListToolsResult(result)) => {
                    tools.extend(result.tools);
                    result.next_cursor
                }
                (1, ServerResult::ListResourcesResult(result)) => {
                    resources.extend(result.resources);
                    result.next_cursor
                }
                (2, ServerResult::ListPromptsResult(result)) => {
                    prompts.extend(result.prompts);
                    result.next_cursor
                }
                _ => {
                    return Err(McpError::new(
                        McpErrorKind::Protocol,
                        "Unexpected MCP discovery response.",
                        false,
                    ));
                }
            };
            if tools.len() + resources.len() + prompts.len() > MAX_ITEMS {
                break;
            }
            if let Some(next) = &cursor {
                if !seen.insert(next.clone()) {
                    break;
                }
            } else {
                done = true;
                break;
            }
        }
        if !done {
            return Err(McpError::new(
                McpErrorKind::Protocol,
                "MCP discovery exceeded item/page limits or repeated a cursor; no partial discovery was published.",
                false,
            ));
        }
    }
    tools.sort_by(|a, b| a.name.cmp(&b.name));
    let mut names = HashSet::new();
    let mut exported = vec![];
    for tool in tools {
        if !names.insert(tool.name.clone()) {
            return Err(McpError::new(
                McpErrorKind::Protocol,
                "Server advertised duplicate tool names; discovery is ambiguous.",
                false,
            ));
        }
        exported.push(McpTool {
            id: tool_id(&c.registration.id, &tool.name),
            server_id: c.registration.id.clone(),
            server_name: c.registration.name.clone(),
            name: tool.name.to_string(),
            description: tool.description.map(|s| s.into_owned()),
            input_schema: Value::Object((*tool.input_schema).clone()),
        });
    }
    let mut info = c.info.write().await;
    info.tools = exported;
    info.resources = resources
        .into_iter()
        .map(|v| serde_json::to_value(v).unwrap_or(Value::Null))
        .collect();
    info.prompts = prompts
        .into_iter()
        .map(|v| serde_json::to_value(v).unwrap_or(Value::Null))
        .collect();
    info.error = None;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rmcp::{
        ErrorData, RoleServer, ServerHandler,
        service::RequestContext,
        transport::streamable_http_server::{
            StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
        },
    };
    use std::sync::atomic::AtomicUsize;

    #[derive(Clone, Default)]
    struct Fixture {
        calls: Arc<AtomicUsize>,
        started: Arc<tokio::sync::Notify>,
        changed: Arc<AtomicBool>,
        bad_cursor: bool,
        slow_discovery: bool,
        incompatible: bool,
        legacy: bool,
    }
    impl ServerHandler for Fixture {
        async fn discover(
            &self,
            _: RequestContext<RoleServer>,
        ) -> Result<DiscoverResult, ErrorData> {
            if self.legacy {
                Err(ErrorData::method_not_found::<DiscoverRequestMethod>())
            } else {
                Ok(DiscoverResult::from_server_info(
                    self.supported_protocol_versions().into_owned(),
                    self.get_info(),
                ))
            }
        }
        fn accepted_subscription_filter(
            &self,
            requested: &SubscriptionFilter,
        ) -> Option<SubscriptionFilter> {
            Some(requested.supported_by(&self.get_info().capabilities))
        }
        fn supported_protocol_versions(&self) -> std::borrow::Cow<'static, [ProtocolVersion]> {
            if self.incompatible {
                std::borrow::Cow::Owned(vec![])
            } else {
                std::borrow::Cow::Borrowed(ProtocolVersion::KNOWN_VERSIONS)
            }
        }
        fn get_info(&self) -> ServerConfig {
            ServerConfig::new(
                ServerCapabilities::builder()
                    .enable_tools()
                    .enable_tool_list_changed()
                    .enable_resources()
                    .enable_prompts()
                    .build(),
            )
        }
        async fn list_tools(
            &self,
            request: Option<PaginatedRequestParams>,
            _: RequestContext<RoleServer>,
        ) -> Result<ListToolsResult, ErrorData> {
            if self.slow_discovery {
                tokio::time::sleep(Duration::from_secs(60)).await;
            }
            let mut tools: Vec<Tool> =
                serde_json::from_str(include_str!("../../../fixtures/mcp/tools.json")).unwrap();
            let page = request.and_then(|p| p.cursor);
            let tool = if page.is_none() {
                tools.remove(0)
            } else {
                tools.remove(1)
            };
            let mut result = ListToolsResult::with_all_items(vec![tool]);
            if page.is_none() || self.bad_cursor {
                result.next_cursor = Some("next".into());
            }
            if page.is_none() && self.changed.load(Ordering::SeqCst) {
                result.tools.push(
                    serde_json::from_value(
                        json!({"name":"aaa_new", "inputSchema":{"type":"object"}}),
                    )
                    .unwrap(),
                );
            }
            Ok(result)
        }
        async fn list_resources(
            &self,
            request: Option<PaginatedRequestParams>,
            _: RequestContext<RoleServer>,
        ) -> Result<ListResourcesResult, ErrorData> {
            let page = request.and_then(|p| p.cursor);
            let name = if page.is_none() { "first" } else { "second" };
            let mut result = ListResourcesResult::with_all_items(vec![
                serde_json::from_value(json!({"uri": format!("fixture://{name}"),"name":name}))
                    .unwrap(),
            ]);
            if page.is_none() {
                result.next_cursor = Some("next".into());
            }
            Ok(result)
        }
        async fn list_prompts(
            &self,
            request: Option<PaginatedRequestParams>,
            _: RequestContext<RoleServer>,
        ) -> Result<ListPromptsResult, ErrorData> {
            let page = request.and_then(|p| p.cursor);
            let mut result = ListPromptsResult::with_all_items(vec![
                serde_json::from_value(
                    json!({"name": if page.is_none() { "first" } else { "second" }}),
                )
                .unwrap(),
            ]);
            if page.is_none() {
                result.next_cursor = Some("next".into());
            }
            Ok(result)
        }
        async fn call_tool(
            &self,
            request: CallToolRequestParams,
            context: RequestContext<RoleServer>,
        ) -> Result<CallToolResponse, ErrorData> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.started.notify_one();
            if request.name == "wait" {
                tokio::time::sleep(Duration::from_secs(60)).await;
            }
            if request
                .arguments
                .as_ref()
                .is_some_and(|a| a.get("notify") == Some(&Value::Bool(true)))
            {
                self.changed.store(true, Ordering::SeqCst);
                context.peer.notify_tool_list_changed().await.unwrap();
            }
            Ok(CallToolResult::success(vec![ContentBlock::text(
                request
                    .arguments
                    .unwrap_or_default()
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("done"),
            )])
            .into())
        }
        async fn read_resource(
            &self,
            request: ReadResourceRequestParams,
            _: RequestContext<RoleServer>,
        ) -> Result<ReadResourceResponse, ErrorData> {
            Ok(ReadResourceResult::new(vec![ResourceContents::text(
                "synthetic context",
                request.uri,
            )])
            .into())
        }
        async fn get_prompt(
            &self,
            _: GetPromptRequestParams,
            _: RequestContext<RoleServer>,
        ) -> Result<GetPromptResponse, ErrorData> {
            Ok(serde_json::from_value::<GetPromptResult>(json!({"messages":[{"role":"user","content":{"type":"text","text":"synthetic prompt"}}]})).unwrap().into())
        }
    }
    struct HttpFixture {
        url: String,
        stop: CancellationToken,
        task: tokio::task::JoinHandle<()>,
        fixture: Fixture,
    }
    impl Drop for HttpFixture {
        fn drop(&mut self) {
            self.stop.cancel();
            self.task.abort();
        }
    }
    async fn server(fixture: Fixture) -> HttpFixture {
        let stop = CancellationToken::new();
        let factory = fixture.clone();
        let service: StreamableHttpService<Fixture, LocalSessionManager> =
            StreamableHttpService::new(
                move || Ok(factory.clone()),
                Default::default(),
                StreamableHttpServerConfig::default().with_cancellation_token(stop.child_token()),
            );
        let router = axum::Router::new()
            .nest_service("/mcp", service)
            .route("/alive", axum::routing::get(|| async { "alive" }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/mcp", listener.local_addr().unwrap());
        let cancel = stop.clone();
        let task = tokio::spawn(async move {
            axum::serve(listener, router)
                .with_graceful_shutdown(cancel.cancelled_owned())
                .await
                .unwrap();
        });
        HttpFixture {
            url,
            stop,
            task,
            fixture,
        }
    }
    fn registration(url: &str) -> McpRegistration {
        McpRegistration::new(
            "same display name",
            McpTransport::StreamableHttp {
                url: url.into(),
                bearer_token_env: None,
            },
        )
    }
    fn named<'a>(info: &'a McpConnectionInfo, name: &str) -> &'a McpTool {
        info.tools.iter().find(|t| t.name == name).unwrap()
    }
    #[test]
    fn endpoints_and_nonsecret_serialization() {
        for url in [
            "https://example.test/mcp",
            "http://localhost:80/mcp",
            "http://127.0.0.1:80/mcp",
            "http://[::1]/mcp",
        ] {
            assert!(validate_mcp_url(url).is_ok(), "{url}");
        }
        for url in [
            "http://example.test/mcp",
            "file:///tmp/mcp",
            "https://user:secret@example.test/mcp",
            "https://example.test/mcp?token=secret",
            "https://example.test/mcp#secret",
        ] {
            assert!(validate_mcp_url(url).is_err());
        }
        let mut reg = registration("https://example.test/mcp");
        reg.transport = McpTransport::Stdio {
            command: "fixture".into(),
            args: vec![],
            env: BTreeMap::from([("TOKEN".into(), "MCP_TOKEN_REF".into())]),
        };
        let _credentials = McpCredentials::default()
            .with_bearer_token("sensitive-token")
            .with_env("TOKEN", "sensitive-env");
        let json = serde_json::to_string(&reg).unwrap();
        assert!(json.contains("MCP_TOKEN_REF"));
        assert!(!json.contains("sensitive"));
        assert_eq!(serde_json::from_str::<McpRegistration>(&json).unwrap(), reg);
        let injected = json.replace("\"enabled\":true", "\"enabled\":true,\"token\":\"secret\"");
        assert!(serde_json::from_str::<McpRegistration>(&injected).is_err());
    }
    #[test]
    fn ids_survive_renames_and_new_tools() {
        let a = uuid::Uuid::new_v4().to_string();
        let b = uuid::Uuid::new_v4().to_string();
        assert_ne!(tool_id(&a, "echo"), tool_id(&b, "echo"));
        assert_ne!(tool_id(&a, "a-b"), tool_id(&a, "a_b"));
        assert_eq!(tool_id(&a, "echo"), tool_id(&a, "echo"));
        assert!(tool_id(&a, &"unsafe /🔥".repeat(100)).len() <= 64);
    }
    #[test]
    fn validates_real_schema_without_fetching_references() {
        let schema = json!({"type":"object", "required":["value"], "properties":{"value":{"type":"integer","minimum":2}}, "additionalProperties":false});
        assert!(validate_arguments(&schema, &json!({"value":3})).is_ok());
        for args in [
            json!({}),
            json!({"value":"3"}),
            json!({"value":1}),
            json!({"value":3,"extra":true}),
        ] {
            assert_eq!(
                validate_arguments(&schema, &args).unwrap_err().kind,
                McpErrorKind::InvalidArguments
            );
        }
        assert!(
            validate_arguments(&json!({"$ref":"https://example.test/schema"}), &json!({})).is_err()
        );
    }
    #[tokio::test]
    async fn paginated_sdk_discovery_routing_context_refresh_and_http_ownership() {
        let other_server = server(Fixture {
            legacy: true,
            ..Default::default()
        })
        .await;
        let server = server(Fixture::default()).await;
        let manager = McpManager::new();
        let first = manager
            .connect(registration(&server.url), McpCredentials::default())
            .await
            .unwrap();
        let second = manager
            .connect(registration(&other_server.url), McpCredentials::default())
            .await
            .unwrap();
        assert_eq!(first.state, McpConnectionState::Connected);
        assert_eq!(first.protocol_version.as_deref(), Some("2026-07-28"));
        assert_eq!(second.protocol_version.as_deref(), Some("2025-11-25"));
        assert_eq!(
            (
                first.tools.len(),
                first.resources.len(),
                first.prompts.len()
            ),
            (2, 2, 2)
        );
        let echo = named(&first, "echo").id.clone();
        assert_ne!(echo, named(&second, "echo").id);
        manager
            .call_tool(
                &named(&second, "echo").id,
                json!({"message":"other server"}),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(other_server.fixture.calls.load(Ordering::SeqCst), 1);
        assert_eq!(server.fixture.calls.load(Ordering::SeqCst), 0);
        let selected = BTreeSet::from([echo.clone()]);
        let exported = manager.selected_tools(&selected).await;
        assert_eq!(exported.len(), 1);
        assert_eq!(exported[0].openai_definition()["function"]["name"], echo);
        assert!(manager.selected_tools(&BTreeSet::new()).await.is_empty());
        let error = manager
            .call_selected_tool(
                &BTreeSet::new(),
                &echo,
                json!({"message":"hello"}),
                CancellationToken::new(),
            )
            .await
            .unwrap_err();
        assert_eq!(error.kind, McpErrorKind::NotSelected);
        assert_eq!(server.fixture.calls.load(Ordering::SeqCst), 0);
        assert!(
            manager
                .call_tool(&echo, json!({"message":5}), CancellationToken::new())
                .await
                .is_err()
        );
        let result = manager
            .call_selected_tool(
                &selected,
                &echo,
                json!({"message":"hello","notify":true}),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(
            serde_json::to_value(result).unwrap()["content"][0]["text"],
            "hello"
        );
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if manager
                    .connections()
                    .await
                    .iter()
                    .any(|c| c.discovery_changed)
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let refreshed = manager.refresh(&first.id).await.unwrap();
        assert_eq!(refreshed.tools.len(), 3);
        assert_eq!(named(&refreshed, "echo").id, echo);
        assert!(
            manager
                .read_resource(&first.id, "fixture://second")
                .await
                .is_ok()
        );
        assert!(
            manager
                .read_resource(&first.id, "fixture://unselected")
                .await
                .is_err()
        );
        assert!(
            manager
                .get_prompt(&first.id, "second", json!({}))
                .await
                .is_ok()
        );
        manager.disconnect_all().await;
        assert!(manager.selected_tools(&selected).await.is_empty());
        assert_eq!(
            reqwest::get(server.url.replace("/mcp", "/alive"))
                .await
                .unwrap()
                .status(),
            200
        );
    }
    #[tokio::test]
    async fn disconnect_during_call_keeps_unknown_outcome_without_replay() {
        let server = server(Fixture::default()).await;
        let manager = McpManager::new();
        let info = manager
            .connect(registration(&server.url), McpCredentials::default())
            .await
            .unwrap();
        let id = named(&info, "wait").id.clone();
        let caller = manager.clone();
        let task = tokio::spawn(async move {
            caller
                .call_tool(&id, json!({}), CancellationToken::new())
                .await
        });
        tokio::time::timeout(Duration::from_secs(3), server.fixture.started.notified())
            .await
            .unwrap();
        manager.disconnect(&info.id).await.unwrap();
        let error = task.await.unwrap().unwrap_err();
        assert!(error.outcome_unknown);
        assert_eq!(server.fixture.calls.load(Ordering::SeqCst), 1);
        assert!(
            manager
                .selected_tools(&BTreeSet::from([named(&info, "wait").id.clone()]))
                .await
                .is_empty()
        );
        assert_eq!(
            reqwest::get(server.url.replace("/mcp", "/alive"))
                .await
                .unwrap()
                .status(),
            200
        );
    }
    #[tokio::test]
    async fn cancellation_before_and_after_dispatch_does_not_retry() {
        let server = server(Fixture::default()).await;
        let manager = McpManager::new();
        let info = manager
            .connect(registration(&server.url), McpCredentials::default())
            .await
            .unwrap();
        let id = named(&info, "wait").id.clone();
        let cancel = CancellationToken::new();
        cancel.cancel();
        assert!(
            !manager
                .call_tool(&id, json!({}), cancel)
                .await
                .unwrap_err()
                .outcome_unknown
        );
        assert_eq!(server.fixture.calls.load(Ordering::SeqCst), 0);
        let cancel = CancellationToken::new();
        let ct = cancel.clone();
        let caller = manager.clone();
        let task = tokio::spawn(async move { caller.call_tool(&id, json!({}), ct).await });
        tokio::time::timeout(Duration::from_secs(3), server.fixture.started.notified())
            .await
            .unwrap();
        cancel.cancel();
        let error = task.await.unwrap().unwrap_err();
        assert_eq!(error.kind, McpErrorKind::Cancelled);
        assert!(error.outcome_unknown);
        assert_eq!(server.fixture.calls.load(Ordering::SeqCst), 1);
        manager.disconnect_all().await;
    }
    #[tokio::test]
    async fn repeated_cursor_fails_without_publishing_partial_discovery() {
        let server = server(Fixture {
            bad_cursor: true,
            ..Default::default()
        })
        .await;
        let manager = McpManager::new();
        let error = manager
            .connect(registration(&server.url), McpCredentials::default())
            .await
            .unwrap_err();
        assert_eq!(error.kind, McpErrorKind::Protocol);
        let connections = manager.connections().await;
        assert_eq!(connections[0].state, McpConnectionState::Failed);
        assert!(connections[0].tools.is_empty());
    }
    #[tokio::test]
    async fn disabled_missing_credentials_and_invalid_stdio_fail_without_secrets() {
        let manager = McpManager::new();
        let mut reg = registration("https://example.test/mcp");
        reg.enabled = false;
        assert_eq!(
            manager
                .connect(reg.clone(), McpCredentials::default())
                .await
                .unwrap_err()
                .kind,
            McpErrorKind::Disabled
        );
        reg.enabled = true;
        reg.transport = McpTransport::StreamableHttp {
            url: "https://example.test/mcp".into(),
            bearer_token_env: Some("RAPID_MCP_FIXTURE_MISSING_CREDENTIAL_F39C123".into()),
        };
        assert_eq!(
            manager
                .connect(reg, McpCredentials::default())
                .await
                .unwrap_err()
                .kind,
            McpErrorKind::Authentication
        );
        let reg = McpRegistration::new(
            "missing executable",
            McpTransport::Stdio {
                command: "/nonexistent/rapid-mcp-fixture".into(),
                args: vec![],
                env: BTreeMap::new(),
            },
        );
        assert_eq!(
            manager
                .connect(reg, McpCredentials::default())
                .await
                .unwrap_err()
                .kind,
            McpErrorKind::Transport
        );
    }
    #[tokio::test]
    async fn rejected_auth_and_malformed_bodies_are_sanitized() {
        use axum::{
            http::{HeaderMap, StatusCode, header},
            response::IntoResponse,
        };
        let attempts = Arc::new(AtomicUsize::new(0));
        let observed = attempts.clone();
        let router = axum::Router::new()
            .route(
                "/auth",
                axum::routing::post(move |headers: HeaderMap| {
                    let observed = observed.clone();
                    async move {
                        assert_eq!(
                            headers.get(header::AUTHORIZATION).unwrap(),
                            "Bearer synthetic-private-token"
                        );
                        observed.fetch_add(1, Ordering::SeqCst);
                        (
                            StatusCode::UNAUTHORIZED,
                            [(
                                header::WWW_AUTHENTICATE,
                                "Bearer realm=synthetic-private-challenge",
                            )],
                            "synthetic-private-body",
                        )
                            .into_response()
                    }
                }),
            )
            .route(
                "/malformed",
                axum::routing::post(|| async {
                    (
                        [(header::CONTENT_TYPE, "application/json")],
                        "synthetic-private-malformed-json",
                    )
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        let manager = McpManager::new();
        let error = manager
            .connect(
                registration(&format!("{base}/auth")),
                McpCredentials::default().with_bearer_token("synthetic-private-token"),
            )
            .await
            .unwrap_err();
        assert_eq!(error.kind, McpErrorKind::Authentication);
        assert!(
            !serde_json::to_string(&error)
                .unwrap()
                .contains("synthetic-private")
        );
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
        let error = manager
            .connect(
                registration(&format!("{base}/malformed")),
                McpCredentials::default(),
            )
            .await
            .unwrap_err();
        assert_eq!(error.kind, McpErrorKind::Protocol);
        assert!(
            !serde_json::to_string(&error)
                .unwrap()
                .contains("synthetic-private")
        );
        task.abort();
    }
    #[tokio::test]
    async fn version_mismatch_never_publishes_connected() {
        let server = server(Fixture {
            incompatible: true,
            ..Default::default()
        })
        .await;
        let manager = McpManager::new();
        let error = manager
            .connect(registration(&server.url), McpCredentials::default())
            .await
            .unwrap_err();
        assert_eq!(error.kind, McpErrorKind::Protocol);
        let connections = manager.connections().await;
        assert_eq!(connections[0].state, McpConnectionState::Failed);
        assert!(connections[0].tools.is_empty());
    }
    #[tokio::test]
    async fn connect_timeout_keeps_failed_state_and_no_partial_tools() {
        let server = server(Fixture {
            slow_discovery: true,
            ..Default::default()
        })
        .await;
        let manager = McpManager::new();
        let error = manager
            .connect(registration(&server.url), McpCredentials::default())
            .await
            .unwrap_err();
        assert_eq!(error.kind, McpErrorKind::Timeout);
        let connections = manager.connections().await;
        assert_eq!(connections[0].state, McpConnectionState::Failed);
        assert!(connections[0].tools.is_empty());
    }
    #[tokio::test]
    async fn call_timeout_has_unknown_outcome_and_no_retry() {
        let server = server(Fixture::default()).await;
        let manager = McpManager::new();
        let info = manager
            .connect(registration(&server.url), McpCredentials::default())
            .await
            .unwrap();
        let error = manager
            .call_tool(
                &named(&info, "wait").id,
                json!({}),
                CancellationToken::new(),
            )
            .await
            .unwrap_err();
        assert_eq!(error.kind, McpErrorKind::Timeout);
        assert!(error.outcome_unknown);
        assert_eq!(server.fixture.calls.load(Ordering::SeqCst), 1);
        manager.disconnect_all().await;
    }
    #[cfg(unix)]
    #[tokio::test]
    async fn cancelled_stdio_startup_reaps_only_the_owned_child() {
        let temp = tempfile::tempdir().unwrap();
        let pid_file = temp.path().join("owned.pid");
        // This intentionally never speaks MCP: cancellation must clean up even
        // when startup/handshake has not completed. Successful protocol tests
        // above use the official Rust SDK HTTP server.
        let registration = McpRegistration::new(
            "owned blocked startup",
            McpTransport::Stdio {
                command: "/bin/sh".into(),
                args: vec![
                    "-c".into(),
                    "echo $$ > \"$1\"; exec /bin/sleep 60".into(),
                    "fixture".into(),
                    pid_file.to_string_lossy().into_owned(),
                ],
                env: BTreeMap::new(),
            },
        );
        let id = registration.id.clone();
        let manager = McpManager::new();
        let starter = manager.clone();
        let task = tokio::spawn(async move {
            starter
                .connect(registration, McpCredentials::default())
                .await
        });
        let pid: u32 = tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if let Ok(text) = tokio::fs::read_to_string(&pid_file).await
                    && let Ok(pid) = text.trim().parse::<u32>()
                {
                    break pid;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        manager.disconnect(&id).await.unwrap();
        assert!(task.await.unwrap().is_err());
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                let exists = tokio::process::Command::new("/bin/kill")
                    .args(["-0", &pid.to_string()])
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status()
                    .await
                    .unwrap()
                    .success();
                if !exists {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("owned startup child must be killed and reaped");
        assert_eq!(
            manager.connections().await[0].state,
            McpConnectionState::Disconnected
        );
    }
    /// Explicit prerequisite avoids building an example recursively inside Cargo tests.
    #[cfg(unix)]
    #[tokio::test]
    #[ignore = "First run cargo build -p rapid-mlx-core --example mcp_fixture; see docs/mcp.md"]
    async fn real_stdio_fixture_discovers_calls_cancels_and_exits_gracefully() {
        let fixture = match std::env::var_os("RAPID_MCP_FIXTURE_BIN") {
            Some(path) => std::path::PathBuf::from(path),
            None => std::env::current_exe()
                .unwrap()
                .parent()
                .unwrap()
                .parent()
                .unwrap()
                .join("examples")
                .join("mcp_fixture"),
        }
        .canonicalize()
        .expect("build the mcp_fixture example or set RAPID_MCP_FIXTURE_BIN to its executable");
        let markers = tempfile::tempdir().unwrap();
        let registration = McpRegistration::new(
            "Rust stdio fixture",
            McpTransport::Stdio {
                command: fixture.to_string_lossy().into_owned(),
                args: vec![
                    "--lifecycle-dir".into(),
                    markers.path().to_string_lossy().into_owned(),
                ],
                env: BTreeMap::new(),
            },
        );
        let manager = McpManager::new();
        let info = manager
            .connect(registration, McpCredentials::default())
            .await
            .unwrap();
        assert_eq!(info.state, McpConnectionState::Connected);
        assert_eq!(info.protocol_version.as_deref(), Some("2026-07-28"));
        assert_eq!(
            info.server_info.as_ref().unwrap()["serverInfo"]["name"],
            "rapid-mlx-mcp-fixture"
        );
        assert_eq!(
            (info.tools.len(), info.resources.len(), info.prompts.len()),
            (2, 1, 1)
        );
        let pid: u32 = tokio::fs::read_to_string(markers.path().join("pid"))
            .await
            .unwrap()
            .parse()
            .unwrap();
        let echo = named(&info, "echo").id.clone();
        let selected = BTreeSet::from([echo.clone(), named(&info, "wait").id.clone()]);
        let definitions = manager.selected_tools(&selected).await;
        assert_eq!(definitions.len(), 2);
        let output = manager
            .call_selected_tool(
                &selected,
                &echo,
                json!({"message":"stdio echo ✓"}),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(
            serde_json::to_value(output).unwrap()["content"][0]["text"],
            "stdio echo ✓"
        );
        let resource = manager
            .read_resource(&info.id, "fixture://readme")
            .await
            .unwrap();
        assert!(
            serde_json::to_value(resource).unwrap()["contents"][0]["text"]
                .as_str()
                .unwrap()
                .contains("Synthetic Rust stdio")
        );
        assert!(
            manager
                .get_prompt(&info.id, "fixture_greeting", json!({}))
                .await
                .is_ok()
        );
        let cancellation = CancellationToken::new();
        let token = cancellation.clone();
        let caller = manager.clone();
        let wait = named(&info, "wait").id.clone();
        let waiting = tokio::spawn(async move { caller.call_tool(&wait, json!({}), token).await });
        async fn marker(path: &std::path::Path) -> String {
            tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    if let Ok(value) = tokio::fs::read_to_string(path).await
                        && !value.is_empty()
                    {
                        break value;
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .expect("synthetic stdio lifecycle marker must arrive")
        }
        assert_eq!(marker(&markers.path().join("wait_started")).await, "1");
        cancellation.cancel();
        let error = waiting.await.unwrap().unwrap_err();
        assert_eq!(error.kind, McpErrorKind::Cancelled);
        assert!(error.outcome_unknown);
        assert_eq!(
            marker(&markers.path().join("cancellation_received")).await,
            "received"
        );
        // Prove the connection remains usable after a cancelled in-flight call.
        assert!(
            manager
                .call_tool(
                    &echo,
                    json!({"message":"still connected"}),
                    CancellationToken::new()
                )
                .await
                .is_ok()
        );
        assert_eq!(marker(&markers.path().join("wait_started")).await, "1");
        manager.disconnect(&info.id).await.unwrap();
        assert_eq!(
            marker(&markers.path().join("graceful_exit")).await,
            pid.to_string()
        );
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let alive = tokio::process::Command::new("/bin/kill")
                    .args(["-0", &pid.to_string()])
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status()
                    .await
                    .unwrap()
                    .success();
                if !alive {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("stdio child must be gone and reaped after graceful disconnect");
        assert_eq!(
            manager.connections().await[0].state,
            McpConnectionState::Disconnected
        );
        assert!(manager.selected_tools(&selected).await.is_empty());
    }
}
