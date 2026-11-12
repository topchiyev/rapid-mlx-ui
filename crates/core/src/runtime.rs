//! Version-pinned CLI and loopback service control. External attachments never
//! acquire child handles. Network/model mutations only occur via explicit calls.
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    path::{Path, PathBuf},
    process::Stdio,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    io::AsyncReadExt,
    process::{Child, Command},
    task::JoinHandle,
    time::{Instant, sleep, timeout},
};
use tokio_util::sync::CancellationToken;

const HTTP_LIMIT: usize = 4 * 1024 * 1024;
const LOG_LINES: usize = 2_000;
const LINE_BYTES: usize = 8 * 1024;
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeRole {
    Chat,
    Transcription,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeConfig {
    pub executable: PathBuf,
    pub model_path: PathBuf,
    pub served_model: String,
    pub role: RuntimeRole,
    pub port: u16,
    pub cache_root: PathBuf,
    pub startup_timeout_secs: u64,
    pub disable_disk_caches: bool,
    #[serde(default)]
    pub app_state_root: Option<PathBuf>,
}
impl RuntimeConfig {
    pub fn new(
        executable: PathBuf,
        model_path: PathBuf,
        served_model: String,
        role: RuntimeRole,
        cache_root: PathBuf,
    ) -> Self {
        Self {
            executable,
            model_path,
            served_model,
            role,
            port: match role {
                RuntimeRole::Chat => 8000,
                RuntimeRole::Transcription => 8001,
            },
            cache_root,
            startup_timeout_secs: 300,
            disable_disk_caches: true,
            app_state_root: None,
        }
    }
    pub fn arguments(&self) -> Result<Vec<String>> {
        let model = crate::catalog::inspect_model(&self.model_path)?;
        if self.role == RuntimeRole::Transcription
            && crate::catalog::local_asr_family(&self.model_path)?.is_none()
        {
            bail!(
                "selected local folder has no verified ASR family metadata (TTS/aligner and unknown folders cannot activate as transcription)"
            );
        }
        if !model.can_attempt_load() {
            bail!("model is not usable: {:?}", model.compatibility);
        }
        if self.port == 0 || self.served_model.trim().is_empty() || self.startup_timeout_secs == 0 {
            bail!("nonzero port/deadline and explicit served identity required");
        }
        let mut args = vec![
            "serve".into(),
            model
                .load_path
                .to_str()
                .context("model path is not UTF-8")?
                .into(),
            "--served-model-name".into(),
            self.served_model.clone(),
            "--host".into(),
            "127.0.0.1".into(),
            "--port".into(),
            self.port.to_string(),
            "--disable-model-downloads".into(),
            "--watchdog-ppid".into(),
            std::process::id().to_string(),
        ];
        if self.disable_disk_caches {
            args.push("--disable-disk-caches".into());
        }
        Ok(args)
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum RuntimeState {
    Stopped,
    Starting,
    Ready {
        model: String,
        role: RuntimeRole,
        external: bool,
    },
    Stopping,
    Failed {
        message: String,
    },
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompatibilityProfile {
    pub version: String,
    pub local_only: bool,
    pub parent_watchdog: bool,
    pub cancellation_identity: String,
    pub live_model_verified: bool,
}
impl CompatibilityProfile {
    pub fn v1_0_2() -> Self {
        Self {
            version: "1.0.2".into(),
            local_only: true,
            parent_watchdog: true,
            cancellation_identity:
                "chatcmpl SSE id; abort support is engine dependent; ASR identity unavailable"
                    .into(),
            live_model_verified: false,
        }
    }
}
/// Resolve only configured path/PATH and conventional Homebrew paths, never scan
/// the machine or install software. A chosen binary is probed before use.
pub fn discover_executable(configured: Option<&Path>) -> Result<PathBuf> {
    if let Some(path) = configured {
        if path.is_file() {
            return Ok(path.to_path_buf());
        }
        bail!("configured rapid-mlx executable is missing");
    }
    if let Some(paths) = std::env::var_os("PATH") {
        for path in std::env::split_paths(&paths) {
            let exe = path.join("rapid-mlx");
            if exe.is_file() {
                return Ok(exe);
            }
        }
    }
    for path in ["/opt/homebrew/bin/rapid-mlx", "/usr/local/bin/rapid-mlx"] {
        if Path::new(path).is_file() {
            return Ok(path.into());
        }
    }
    bail!("rapid-mlx was not found; configure its executable path")
}
fn command(exe: &Path, cache: &Path, offline: bool) -> Command {
    let mut c = Command::new(exe);
    #[cfg(unix)]
    c.process_group(0);
    c.env("RAPID_MLX_TELEMETRY", "0")
        .env("RAPID_MLX_DISABLE_VERSION_CHECK", "1")
        .env("HF_HUB_CACHE", cache)
        .env("HF_HUB_DISABLE_TELEMETRY", "1")
        .env("DO_NOT_TRACK", "1")
        .env("HF_HUB_OFFLINE", if offline { "1" } else { "0" })
        .env("TRANSFORMERS_OFFLINE", if offline { "1" } else { "0" })
        .stdin(Stdio::null())
        .kill_on_drop(true);
    c
}
#[derive(Default)]
struct LogBuffer {
    lines: VecDeque<String>,
}
impl LogBuffer {
    fn push(&mut self, bytes: &[u8]) {
        let mut line = String::from_utf8_lossy(&bytes[..bytes.len().min(LINE_BYTES)]).into_owned();
        if line.len() > LINE_BYTES {
            let mut boundary = LINE_BYTES;
            while !line.is_char_boundary(boundary) {
                boundary -= 1;
            }
            line.truncate(boundary);
        }
        self.lines.push_back(line);
        while self.lines.len() > LOG_LINES {
            self.lines.pop_front();
        }
    }
}
async fn drain<R: tokio::io::AsyncRead + Unpin>(mut stream: R, logs: Arc<Mutex<LogBuffer>>) {
    let mut chunk = [0; 1024];
    let mut line = Vec::new();
    let mut overflow = false;
    loop {
        match stream.read(&mut chunk).await {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                for b in &chunk[..n] {
                    if *b == b'\n' {
                        if let Ok(mut l) = logs.lock() {
                            l.push(&line);
                        }
                        line.clear();
                        overflow = false;
                    } else if !overflow {
                        if line.len() < LINE_BYTES {
                            line.push(*b);
                        } else {
                            overflow = true;
                        }
                    }
                }
            }
        }
    }
    if !line.is_empty()
        && let Ok(mut l) = logs.lock()
    {
        l.push(&line);
    }
}
fn capture(child: &mut Child, logs: Arc<Mutex<LogBuffer>>) -> Vec<JoinHandle<()>> {
    let mut handles = Vec::new();
    if let Some(out) = child.stdout.take() {
        handles.push(tokio::spawn(drain(out, logs.clone())));
    }
    if let Some(err) = child.stderr.take() {
        handles.push(tokio::spawn(drain(err, logs)));
    }
    handles
}
async fn bounded_output(exe: &Path, args: &[&str], cache: &Path) -> Result<String> {
    let mut child = command(exe, cache, true)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let mut out = child.stdout.take().context("missing stdout")?;
    let mut bytes = Vec::new();
    let result = timeout(Duration::from_secs(10), async {
        (&mut out)
            .take(HTTP_LIMIT as u64 + 1)
            .read_to_end(&mut bytes)
            .await?;
        if bytes.len() > HTTP_LIMIT {
            bail!("CLI probe exceeded output limit");
        }
        let status = child.wait().await?;
        if !status.success() {
            bail!("CLI probe exited with {status}");
        }
        Ok(())
    })
    .await;
    match result {
        Ok(Ok(())) => Ok(String::from_utf8(bytes)?),
        Ok(Err(e)) => {
            let _ = child.kill().await;
            Err(e)
        }
        Err(_) => {
            let _ = child.kill().await;
            bail!("CLI probe timed out")
        }
    }
}
pub async fn probe_executable(exe: &Path, cache: &Path) -> Result<CompatibilityProfile> {
    let version = bounded_output(exe, &["--version"], cache).await?;
    if version.trim() != "rapid-mlx 1.0.2" {
        bail!(
            "unsupported runtime version: {}; only inspected 1.0.2 is enabled",
            version.trim()
        );
    }
    let help = bounded_output(exe, &["serve", "--help"], cache).await?;
    for flag in [
        "--served-model-name",
        "--host",
        "--port",
        "--disable-model-downloads",
        "--watchdog-ppid",
        "--disable-disk-caches",
    ] {
        if !help.contains(flag) {
            bail!("required CLI option is unavailable: {flag}");
        }
    }
    Ok(CompatibilityProfile::v1_0_2())
}
fn loopback_endpoint(endpoint: &str) -> Result<String> {
    let url = reqwest::Url::parse(endpoint)?;
    let host = url
        .host_str()
        .context("missing host")?
        .trim_matches(['[', ']']);
    if !host
        .parse::<std::net::IpAddr>()
        .is_ok_and(|ip| ip.is_loopback())
        || url.scheme() != "http"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.path() != "/"
        || url.query().is_some()
        || url.fragment().is_some()
    {
        bail!("use a numeric loopback http origin without credentials, path, query, or fragment");
    }
    Ok(endpoint.trim_end_matches('/').to_owned())
}
async fn json(client: &reqwest::Client, url: String, credential: Option<&str>) -> Result<Value> {
    let request = client.get(url);
    let request = if let Some(token) = credential {
        request.bearer_auth(token)
    } else {
        request
    };
    let mut response = request.send().await?.error_for_status()?;
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        if bytes.len() + chunk.len() > HTTP_LIMIT {
            bail!("runtime response exceeds 4 MiB");
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(serde_json::from_slice(&bytes)?)
}
/// List actual served identities independently of chat/ASR route negotiation.
/// Credentials remain request-local and responses share the bounded JSON reader.
pub async fn advertised_models(endpoint: &str, credential: Option<&str>) -> Result<Vec<String>> {
    let endpoint = loopback_endpoint(endpoint)?;
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(3))
        .build()?;
    let payload = json(&client, format!("{endpoint}/v1/models"), credential).await?;
    let rows = payload
        .get("data")
        .and_then(Value::as_array)
        .context("served model response has no data array")?;
    let mut identities = Vec::new();
    let mut seen = BTreeSet::new();
    for row in rows {
        let id = row
            .get("id")
            .and_then(Value::as_str)
            .filter(|id| !id.trim().is_empty())
            .context("served model row has no nonempty id")?;
        if seen.insert(id) {
            identities.push(id.to_owned());
        }
    }
    Ok(identities)
}
async fn readiness(
    client: &reqwest::Client,
    endpoint: &str,
    model: &str,
    role: RuntimeRole,
    credential: Option<&str>,
) -> Result<()> {
    let health = json(client, format!("{endpoint}/health"), credential).await?;
    if health.get("ready").and_then(Value::as_bool) != Some(true) {
        bail!("runtime has not completed readiness/warmup");
    }
    let model_type = health.get("model_type").and_then(Value::as_str);
    if role == RuntimeRole::Transcription && model_type != Some("stt") {
        bail!("runtime primary model is not a verified speech-to-text service");
    }
    if role == RuntimeRole::Chat && !matches!(model_type, Some("llm" | "mllm")) {
        bail!("runtime primary model is not a verified chat service");
    }
    let models = json(client, format!("{endpoint}/v1/models"), credential).await?;
    if !models
        .get("data")
        .and_then(Value::as_array)
        .is_some_and(|data| {
            data.iter()
                .any(|m| m.get("id").and_then(Value::as_str) == Some(model))
        })
    {
        bail!("runtime does not serve the selected identity {model}");
    }
    let api = json(client, format!("{endpoint}/openapi.json"), credential).await?;
    let paths = api
        .get("paths")
        .and_then(Value::as_object)
        .context("OpenAPI paths are missing")?;
    for (route, method) in [
        ("/health", "get"),
        ("/v1/models", "get"),
        (
            match role {
                RuntimeRole::Chat => "/v1/chat/completions",
                RuntimeRole::Transcription => "/v1/audio/transcriptions",
            },
            "post",
        ),
    ] {
        if paths.get(route).and_then(|p| p.get(method)).is_none() {
            bail!("required runtime route is unavailable: {method} {route}");
        }
    }
    Ok(())
}
async fn signal_owned_group(pid: u32, signal: &str) {
    #[cfg(unix)]
    {
        // Every child was spawned into its own process group by command().
        // This group id comes only from the owned child handle.
        let _ = Command::new("/bin/kill")
            .args([signal, "--", &format!("-{pid}")])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await;
    }
    #[cfg(not(unix))]
    {
        let _ = (pid, signal);
    }
}
async fn terminate(child: &mut Child) -> Result<()> {
    let pid = child.id();
    let alive = child.try_wait()?.is_none();
    if let Some(pid) = pid {
        signal_owned_group(pid, "-TERM").await;
    }
    if alive
        && timeout(Duration::from_secs(10), child.wait())
            .await
            .is_err()
    {
        child.kill().await?;
    }
    // Reap the direct child and remove any descendants left in its private
    // group (e.g. a worker ignoring TERM after the leader exits).
    let _ = child.wait().await?;
    if let Some(pid) = pid {
        signal_owned_group(pid, "-KILL").await;
    }
    Ok(())
}
pub struct RuntimeSupervisor {
    child: Option<Child>,
    owned_group: Option<u32>,
    state: RuntimeState,
    endpoint: Option<String>,
    client: reqwest::Client,
    logs: Arc<Mutex<LogBuffer>>,
    readers: Vec<JoinHandle<()>>,
    use_registry: ModelUseRegistry,
    use_lease: Option<ModelUseLease>,
    inference_active: bool,
    credential: Option<String>,
}
impl RuntimeSupervisor {
    pub fn new() -> Result<Self> {
        Self::with_registry(ModelUseRegistry::default())
    }
    pub fn with_registry(use_registry: ModelUseRegistry) -> Result<Self> {
        Ok(Self {
            child: None,
            owned_group: None,
            state: RuntimeState::Stopped,
            endpoint: None,
            client: reqwest::Client::builder()
                .no_proxy()
                .redirect(reqwest::redirect::Policy::none())
                .timeout(Duration::from_secs(3))
                .build()?,
            logs: Arc::new(Mutex::new(LogBuffer::default())),
            readers: Vec::new(),
            use_registry,
            use_lease: None,
            inference_active: false,
            credential: None,
        })
    }
    pub fn state(&self) -> &RuntimeState {
        &self.state
    }
    pub fn endpoint(&self) -> Option<&str> {
        self.endpoint.as_deref()
    }
    pub fn owned_pid(&self) -> Option<u32> {
        self.child.as_ref().and_then(Child::id)
    }
    pub fn logs(&self) -> Vec<String> {
        self.logs
            .lock()
            .map(|l| l.lines.iter().cloned().collect())
            .unwrap_or_default()
    }
    pub fn set_inference_active(&mut self, active: bool) {
        self.inference_active = active;
    }
    pub async fn start(&mut self, config: RuntimeConfig) -> Result<()> {
        if self.inference_active {
            bail!("finish or stop inference before switching the runtime");
        }
        if self.child.is_some()
            || !matches!(
                self.state,
                RuntimeState::Stopped | RuntimeState::Failed { .. }
            )
        {
            bail!("stop or detach the current runtime before starting another");
        }
        let result = self.start_inner(config).await;
        if let Err(e) = &result {
            if let Some(mut child) = self.child.take() {
                let _ = terminate(&mut child).await;
            }
            if let Some(pid) = self.owned_group.take() {
                signal_owned_group(pid, "-KILL").await;
            }
            self.finish_readers();
            self.use_lease = None;
            self.endpoint = None;
            self.state = RuntimeState::Failed {
                message: e.to_string(),
            };
        }
        result
    }
    async fn start_inner(&mut self, mut config: RuntimeConfig) -> Result<()> {
        self.state = RuntimeState::Starting;
        self.credential = None;
        config.cache_root = std::path::absolute(&config.cache_root)?;
        if config.executable.is_file() {
            config.executable = config.executable.canonicalize()?;
        }
        probe_executable(&config.executable, &config.cache_root).await?;
        let args = config.arguments()?;
        // Refuse a collision before spawn; readiness never adopts an occupant.
        let reservation = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, config.port))
            .context("configured runtime port is already occupied")?;
        let key = model_use_key(&config.model_path, &config.cache_root)?;
        self.use_lease = Some(self.use_registry.acquire(&key, "loaded runtime")?);
        drop(reservation);
        let state_root = match config.app_state_root {
            Some(root) => root,
            None => PathBuf::from(
                std::env::var_os("HOME").context("HOME unavailable for runtime state")?,
            )
            .join(".rapid-mlx-ui"),
        };
        if !state_root.is_absolute() {
            bail!("runtime app state root must be absolute");
        }
        let runtime_root = state_root.join("runtime").join(match config.role {
            RuntimeRole::Chat => "chat",
            RuntimeRole::Transcription => "asr",
        });
        let temporary = runtime_root.join("tmp");
        let cache = runtime_root.join("cache");
        tokio::fs::create_dir_all(&temporary).await?;
        tokio::fs::create_dir_all(&cache).await?;
        let mut child = command(&config.executable, &config.cache_root, true)
            .current_dir(&runtime_root)
            .env("TMPDIR", &temporary)
            .env("TMP", &temporary)
            .env("TEMP", &temporary)
            .env("XDG_CACHE_HOME", &cache)
            .env("HF_HOME", runtime_root.join("huggingface"))
            .env("RAPID_MLX_STATE_DIR", runtime_root.join("rapid-mlx"))
            .args(&args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .context("start rapid-mlx")?;
        self.owned_group = child.id();
        self.readers = capture(&mut child, self.logs.clone());
        self.child = Some(child);
        let endpoint = format!("http://127.0.0.1:{}", config.port);
        self.endpoint = Some(endpoint.clone());
        let deadline = Instant::now() + Duration::from_secs(config.startup_timeout_secs);
        let mut delay = Duration::from_millis(50);
        let mut last = "runtime not ready".to_owned();
        loop {
            if let Some(status) = self
                .child
                .as_mut()
                .context("owned child missing")?
                .try_wait()?
            {
                bail!("runtime exited before readiness: {status}");
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                bail!("startup deadline expired: {last}");
            }
            match timeout(
                remaining,
                readiness(
                    &self.client,
                    &endpoint,
                    &config.served_model,
                    config.role,
                    None,
                ),
            )
            .await
            {
                Ok(Ok(())) => {
                    if self
                        .child
                        .as_mut()
                        .context("owned child missing")?
                        .try_wait()?
                        .is_some()
                    {
                        bail!("runtime exited during readiness");
                    }
                    self.state = RuntimeState::Ready {
                        model: config.served_model,
                        role: config.role,
                        external: false,
                    };
                    return Ok(());
                }
                Ok(Err(e)) => last = e.to_string(),
                Err(_) => bail!("startup deadline expired: {last}"),
            }
            sleep(delay.min(deadline.saturating_duration_since(Instant::now()))).await;
            delay = (delay * 2).min(Duration::from_secs(1));
        }
    }
    pub async fn attach(
        &mut self,
        endpoint: &str,
        expected_model: &str,
        role: RuntimeRole,
    ) -> Result<()> {
        self.attach_with_credential(endpoint, expected_model, role, None)
            .await
    }
    pub async fn attach_with_credential(
        &mut self,
        endpoint: &str,
        expected_model: &str,
        role: RuntimeRole,
        credential: Option<&str>,
    ) -> Result<()> {
        if self.child.is_some() || self.inference_active {
            bail!("stop the owned service/inference before attaching");
        }
        let endpoint = loopback_endpoint(endpoint)?;
        readiness(&self.client, &endpoint, expected_model, role, credential).await?;
        self.credential = credential.map(str::to_owned);
        self.endpoint = Some(endpoint);
        self.state = RuntimeState::Ready {
            model: expected_model.into(),
            role,
            external: true,
        };
        Ok(())
    }
    pub async fn check(&mut self) -> Result<RuntimeState> {
        if let Some(child) = self.child.as_mut()
            && let Some(status) = child.try_wait()?
        {
            self.child = None;
            if let Some(pid) = self.owned_group.take() {
                signal_owned_group(pid, "-KILL").await;
            }
            self.use_lease = None;
            self.finish_readers();
            self.state = RuntimeState::Failed {
                message: format!("runtime exited: {status}"),
            };
            return Ok(self.state.clone());
        }
        if let (RuntimeState::Ready { model, role, .. }, Some(endpoint)) =
            (&self.state, &self.endpoint)
            && let Err(e) = readiness(
                &self.client,
                endpoint,
                model,
                *role,
                self.credential.as_deref(),
            )
            .await
        {
            self.state = RuntimeState::Failed {
                message: e.to_string(),
            };
        }
        Ok(self.state.clone())
    }
    pub async fn stop(&mut self) -> Result<()> {
        if self.inference_active {
            bail!("finish or cancel inference before stopping the runtime");
        }
        self.state = RuntimeState::Stopping;
        if let Some(mut child) = self.child.take()
            && let Err(e) = terminate(&mut child).await
        {
            self.child = Some(child);
            self.state = RuntimeState::Failed {
                message: e.to_string(),
            };
            return Err(e);
        }
        if let Some(pid) = self.owned_group.take() {
            signal_owned_group(pid, "-KILL").await;
        }
        self.finish_readers();
        self.use_lease = None;
        self.endpoint = None;
        self.credential = None;
        self.state = RuntimeState::Stopped;
        Ok(())
    }
    fn finish_readers(&mut self) {
        for reader in self.readers.drain(..) {
            reader.abort();
        }
    }
}
impl Drop for RuntimeSupervisor {
    fn drop(&mut self) {
        self.finish_readers(); /* kill_on_drop protects only our child; watchdog covers app death. */
    }
}

/// All app runtime/transfer/inference controllers must share this registry.
#[derive(Clone, Default)]
pub struct ModelUseRegistry(Arc<Mutex<BTreeMap<String, Vec<String>>>>);
pub struct ModelUseLease {
    registry: ModelUseRegistry,
    key: String,
    reason: String,
}
impl ModelUseRegistry {
    pub fn reasons(&self, key: &str) -> Vec<String> {
        self.0
            .lock()
            .map(|m| m.get(key).cloned().unwrap_or_default())
            .unwrap_or_else(|_| vec!["use registry unavailable".into()])
    }
    pub fn acquire(&self, key: &str, reason: &str) -> Result<ModelUseLease> {
        let mut m = self
            .0
            .lock()
            .map_err(|_| anyhow::anyhow!("model use registry unavailable"))?;
        let reasons = m.entry(key.into()).or_default();
        if reasons.iter().any(|r| r == "deleting repository") {
            bail!("repository deletion in progress");
        }
        reasons.push(reason.into());
        Ok(ModelUseLease {
            registry: self.clone(),
            key: key.into(),
            reason: reason.into(),
        })
    }
    fn exclusive(&self, key: &str) -> Result<ModelUseLease> {
        let mut m = self
            .0
            .lock()
            .map_err(|_| anyhow::anyhow!("model use registry unavailable"))?;
        if m.get(key).is_some_and(|r| !r.is_empty()) {
            bail!("model is in active use: {:?}", m.get(key));
        }
        m.insert(key.into(), vec!["deleting repository".into()]);
        Ok(ModelUseLease {
            registry: self.clone(),
            key: key.into(),
            reason: "deleting repository".into(),
        })
    }
}
impl Drop for ModelUseLease {
    fn drop(&mut self) {
        if let Ok(mut m) = self.registry.0.lock()
            && let Some(reasons) = m.get_mut(&self.key)
        {
            if let Some(i) = reasons.iter().position(|r| r == &self.reason) {
                reasons.remove(i);
            }
            if reasons.is_empty() {
                m.remove(&self.key);
            }
        }
    }
}
pub fn repository_use_key(cache_root: &Path, repository: &str) -> Result<String> {
    Ok(format!(
        "{}::{repository}",
        cache_root.canonicalize()?.display()
    ))
}
pub fn model_use_key(path: &Path, cache_root: &Path) -> Result<String> {
    let path = path.canonicalize()?;
    let root = match cache_root.canonicalize() {
        Ok(root) => root,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(path.display().to_string());
        }
        Err(e) => return Err(e.into()),
    };
    if let Ok(relative) = path.strip_prefix(&root)
        && let Some(first) = relative.components().next()
    {
        let name = first.as_os_str().to_string_lossy();
        if let Some(repo) = name.strip_prefix("models--") {
            return repository_use_key(&root, &repo.replace("--", "/"));
        }
    }
    Ok(path.display().to_string())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemovalPreview {
    pub repository: String,
    pub cache_root: PathBuf,
    pub revisions: Vec<String>,
    pub estimated_reclaimable_bytes: Option<u64>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OperationResult {
    pub success: bool,
    pub cancelled: bool,
    pub exit_code: Option<i32>,
    pub logs: Vec<String>,
    pub actual_reclaimed_bytes: Option<u64>,
}
#[derive(Clone)]
pub struct ModelOperations {
    pub executable: PathBuf,
    pub cache_root: PathBuf,
    pub uses: ModelUseRegistry,
}
fn valid_repository(repo: &str) -> bool {
    let parts: Vec<_> = repo.split('/').collect();
    parts.len() == 2
        && parts.iter().all(|p| {
            !p.is_empty()
                && !p.starts_with(['.', '-'])
                && !p.ends_with(['.', '-'])
                && !p.contains("..")
                && !p.contains("--")
                && p.chars()
                    .all(|c| c.is_ascii_alphanumeric() || "._-".contains(c))
        })
}
fn snapshot_revisions(root: &Path, repo: &str) -> Result<Vec<String>> {
    if !valid_repository(repo) {
        bail!("an exact org/repository id is required, not an alias or local path");
    }
    let directory = root.join(format!("models--{}", repo.replace('/', "--")));
    if std::fs::symlink_metadata(&directory)?
        .file_type()
        .is_symlink()
    {
        bail!("repository root must not be a symlink");
    }
    let snapshots = directory.join("snapshots");
    if std::fs::symlink_metadata(&snapshots)?
        .file_type()
        .is_symlink()
    {
        bail!("snapshots root must not be a symlink");
    }
    let mut revisions: Vec<String> = Vec::new();
    for e in std::fs::read_dir(snapshots)? {
        let e = e?;
        if e.file_type()?.is_symlink() {
            bail!("snapshot revision symlinks require manual review");
        }
        if e.file_type()?.is_dir() {
            revisions.push(
                e.file_name()
                    .to_str()
                    .context("invalid revision name")?
                    .into(),
            );
        }
    }
    // The installed HF API selects revision hashes globally, not by repo.
    // Reject a malformed cache with a hash reused by any retained repo type.
    let mut inspected = 0usize;
    for sibling in std::fs::read_dir(root)? {
        let sibling = sibling?;
        if sibling.path() == directory {
            continue;
        }
        let name = sibling.file_name();
        let name = name.to_string_lossy();
        if !["models--", "datasets--", "spaces--"]
            .iter()
            .any(|prefix| name.starts_with(prefix))
        {
            continue;
        }
        if sibling.file_type()?.is_symlink() {
            bail!("cache repository symlink requires manual review");
        }
        let snapshots = sibling.path().join("snapshots");
        if !snapshots.exists() {
            continue;
        }
        for other in std::fs::read_dir(snapshots)? {
            inspected += 1;
            if inspected > 100_000 {
                bail!("cache revision safety scan exceeded limit");
            }
            let other = other?;
            if revisions
                .iter()
                .any(|r| other.file_name().to_str() == Some(r.as_str()))
            {
                bail!("revision hash also appears in a retained repository; removal is unsafe");
            }
        }
    }
    revisions.sort();
    if revisions.is_empty() {
        bail!("no cached revisions to remove");
    }
    Ok(revisions)
}
fn physical_bytes(root: &Path) -> Result<u64> {
    fn walk(p: &Path, n: &mut usize) -> Result<u64> {
        let mut total = 0;
        for e in std::fs::read_dir(p)? {
            *n += 1;
            if *n > 100_000 {
                bail!("cache exceeds measurement limit");
            }
            let e = e?;
            let t = e.file_type()?;
            if t.is_dir() {
                total += walk(&e.path(), n)?;
            } else if t.is_file() {
                total += e.metadata()?.len();
            }
        }
        Ok(total)
    }
    walk(root, &mut 0)
}
impl ModelOperations {
    pub async fn available(&self) -> Result<Vec<crate::catalog::AvailableModel>> {
        let out = bounded_output(&self.executable, &["models", "--json"], &self.cache_root).await?;
        crate::catalog::parse_available_catalog(out.as_bytes())
    }
    pub async fn cached(&self) -> Result<Vec<crate::catalog::CachedModel>> {
        let out = bounded_output(
            &self.executable,
            &["models", "--cached", "--json"],
            &self.cache_root,
        )
        .await?;
        crate::catalog::parse_cached_catalog(out.as_bytes())
    }
    pub fn removal_preview(&self, repository: &str) -> Result<RemovalPreview> {
        let root = self.cache_root.canonicalize()?;
        let key = repository_use_key(&root, repository)?;
        if !self.uses.reasons(&key).is_empty() {
            bail!("repository is active: {:?}", self.uses.reasons(&key));
        }
        Ok(RemovalPreview {
            repository: repository.into(),
            revisions: snapshot_revisions(&root, repository)?,
            cache_root: root,
            estimated_reclaimable_bytes: None,
        })
    }
    /// The caller passes the exact preview the user confirmed. Recheck both
    /// revision scope and use locks immediately before dispatching CLI rm.
    pub async fn remove_confirmed(
        &self,
        preview: &RemovalPreview,
        confirmed: bool,
    ) -> Result<OperationResult> {
        if !confirmed {
            bail!("repository removal requires explicit confirmation");
        }
        if self.cache_root.canonicalize()? != preview.cache_root {
            bail!("cache destination changed; request a new preview");
        }
        let key = repository_use_key(&preview.cache_root, &preview.repository)?;
        let _lease = self.uses.exclusive(&key)?;
        if snapshot_revisions(&preview.cache_root, &preview.repository)? != preview.revisions {
            bail!("cached revision scope changed; request a new preview");
        }
        probe_executable(&self.executable, &self.cache_root).await?;
        let root = self.cache_root.clone();
        let before = tokio::task::spawn_blocking(move || physical_bytes(&root))
            .await?
            .ok();
        if snapshot_revisions(&preview.cache_root, &preview.repository)? != preview.revisions {
            bail!("cached revision scope changed; request a new preview");
        }
        let mut result = self
            .run(
                &["rm", &preview.repository, "--yes"],
                true,
                CancellationToken::new(),
                Duration::from_secs(120),
            )
            .await?;
        let root = self.cache_root.clone();
        let after = tokio::task::spawn_blocking(move || physical_bytes(&root))
            .await?
            .ok();
        result.actual_reclaimed_bytes = before.zip(after).map(|(a, b)| a.saturating_sub(b));
        if result.success
            && self
                .cache_root
                .join(format!("models--{}", preview.repository.replace('/', "--")))
                .join("snapshots")
                .exists()
        {
            result.success = false;
            result
                .logs
                .push("Removal returned success but snapshots remain; refresh inventory".into());
        }
        Ok(result)
    }
    pub async fn pull(
        &self,
        repository: &str,
        cancel: CancellationToken,
    ) -> Result<OperationResult> {
        self.pull_reference(repository, repository, cancel).await
    }
    /// Preserve an explicitly selected catalog alias so rapid-mlx retains its
    /// pinned revision/subfolder. The repository is independently validated and
    /// remains the entire scope of the transfer/deletion-use lease.
    pub async fn pull_reference(
        &self,
        reference: &str,
        repository: &str,
        cancel: CancellationToken,
    ) -> Result<OperationResult> {
        if !valid_repository(repository) {
            bail!("explicit pull requires an exact org/repository id");
        }
        let selected_alias = if reference != repository {
            if reference.is_empty()
                || reference.starts_with('-')
                || reference.contains('/')
                || reference.chars().any(char::is_control)
            {
                bail!("pull reference must be the exact repository or a catalog alias");
            }
            let mut matches: Vec<_> = self
                .available()
                .await?
                .into_iter()
                .filter(|model| model.alias == reference)
                .collect();
            if matches.len() != 1 || matches[0].hf_path != repository {
                bail!(
                    "selected alias is absent, ambiguous, or does not match the requested repository"
                );
            }
            matches.pop()
        } else {
            None
        };
        std::fs::create_dir_all(&self.cache_root)?;
        probe_executable(&self.executable, &self.cache_root).await?;
        let key = repository_use_key(&self.cache_root, repository)?;
        let _lease = self.uses.acquire(&key, "downloading")?;
        let mut result = self
            .run(
                &["pull", reference],
                false,
                cancel,
                Duration::from_secs(24 * 60 * 60),
            )
            .await?;
        if result.success {
            let root = self.cache_root.clone();
            let repo = repository.to_owned();
            let report =
                tokio::task::spawn_blocking(move || crate::catalog::discover(&root, &[])).await?;
            if !report
                .models
                .iter()
                .any(|m| m.repository.as_deref() == Some(&repo) && m.can_attempt_load())
            {
                result.success = false;
                result.logs.push("Transfer exited successfully but local model validation failed; refresh/retry explicitly".into());
            }
            if result.success
                && let Some(selected) = selected_alias
            {
                // A complete sibling must never certify the requested alias.
                // The cached CLI view supplies its exact checkpoint subfolder.
                let validation = async {
                    let cached = self.cached().await?;
                    let cache = self.cache_root.canonicalize()?;
                    let snapshots = cache
                        .join(format!("models--{}", repository.replace('/', "--")))
                        .join("snapshots");
                    Ok::<_, anyhow::Error>(cached.iter().any(|row| {
                        if row.repo != repository
                            || row.alias.as_deref() != Some(reference)
                            || row.state != "ok"
                            || row.external
                        {
                            return false;
                        }
                        let folder = Path::new(row.subfolder.as_deref().unwrap_or(""));
                        if folder.is_absolute()
                            || folder
                                .components()
                                .any(|c| !matches!(c, std::path::Component::Normal(_)))
                        {
                            return false;
                        }
                        report.models.iter().any(|model| {
                            model.repository.as_deref() == Some(repository)
                                && model.can_attempt_load()
                                && model.revision.as_ref().is_some_and(|revision| {
                                    selected.revision.as_ref().is_none_or(|pin| pin == revision)
                                        && model.load_path == snapshots.join(revision).join(folder)
                                })
                        })
                    }))
                }
                .await;
                match validation {
                    Ok(true) => {}
                    Ok(false) => {
                        result.success = false;
                        result.logs.push("Selected alias checkpoint/revision could not be validated locally; a complete sibling is not a substitute".into());
                    }
                    Err(e) => {
                        result.success = false;
                        result
                            .logs
                            .push(format!("Selected alias cache validation unavailable: {e}"));
                    }
                }
            }
        }
        Ok(result)
    }
    async fn run(
        &self,
        args: &[&str],
        offline: bool,
        cancel: CancellationToken,
        deadline: Duration,
    ) -> Result<OperationResult> {
        let logs = Arc::new(Mutex::new(LogBuffer::default()));
        let mut child = command(&self.executable, &self.cache_root, offline)
            .args(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let group = child.id();
        let readers = capture(&mut child, logs.clone());
        let mut cancelled = false;
        let status = tokio::select! {r=child.wait()=>r?,_=cancel.cancelled()=>{cancelled=true;terminate(&mut child).await?;child.wait().await?},_=sleep(deadline)=>{terminate(&mut child).await?;for r in readers{r.abort();}bail!("model operation timed out");}};
        if let Some(pid) = group {
            signal_owned_group(pid, "-KILL").await;
        }
        for reader in readers {
            let _ = timeout(Duration::from_secs(1), reader).await;
        }
        let lines = logs
            .lock()
            .map(|l| l.lines.iter().cloned().collect())
            .unwrap_or_default();
        Ok(OperationResult {
            success: status.success() && !cancelled,
            cancelled,
            exit_code: status.code(),
            logs: lines,
            actual_reclaimed_bytes: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    fn temp() -> PathBuf {
        let p = std::env::temp_dir().join(format!("rapid-runtime-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&p).unwrap();
        p.canonicalize().unwrap()
    }
    #[cfg(unix)]
    fn fake(root: &Path, body: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = root.join("fake rapid-mlx");
        fs::write(&path,format!("#!/bin/sh\nif [ \"$1\" = --version ]; then echo 'rapid-mlx 1.0.2'; exit; fi\nif [ \"$2\" = --help ]; then echo '--served-model-name --host --port --disable-model-downloads --watchdog-ppid --disable-disk-caches'; exit; fi\n{body}\n")).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        path
    }
    fn local_model(root: &Path) -> PathBuf {
        let p = root.join("models--fixture--chat/snapshots/aaaaaaaa");
        fs::create_dir_all(&p).unwrap();
        fs::write(p.join("config.json"), r#"{"model_type":"fixture"}"#).unwrap();
        fs::write(p.join("model.safetensors"), b"fake").unwrap();
        fs::write(p.join("tokenizer.json"), b"{}").unwrap();
        p
    }
    async fn server(listener: tokio::net::TcpListener, model: &str, cancel: CancellationToken) {
        let model = model.to_owned();
        loop {
            let accepted = tokio::select! {a=listener.accept()=>a,_=cancel.cancelled()=>break};
            let Ok((mut socket, _)) = accepted else { break };
            let mut request = [0; 4096];
            let n = socket.read(&mut request).await.unwrap_or(0);
            let path = String::from_utf8_lossy(&request[..n]);
            let body = if path.starts_with("GET /v1/models ") {
                serde_json::json!({"data":[{"id":model}]}).to_string()
            } else if path.starts_with("GET /openapi.json ") {
                include_str!("../../../fixtures/runtime/openapi.json").into()
            } else {
                include_str!("../../../fixtures/runtime/health.json").into()
            };
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = socket.write_all(response.as_bytes()).await;
        }
    }
    #[test]
    fn rejects_remote_and_ambiguous_origins() {
        for endpoint in [
            "https://127.0.0.1:8000",
            "http://localhost:8000",
            "http://192.168.1.2",
            "http://127.0.0.1/api",
            "http://user:secret@127.0.0.1",
            "http://127.0.0.1?x=1",
        ] {
            assert!(loopback_endpoint(endpoint).is_err(), "{endpoint}");
        }
        assert!(loopback_endpoint("http://[::1]:8000/").is_ok());
    }
    #[test]
    fn bounded_logs() {
        let mut logs = LogBuffer::default();
        for _ in 0..2100 {
            logs.push(&vec![b'x'; LINE_BYTES + 100]);
        }
        assert_eq!(logs.lines.len(), 2000);
        assert_eq!(logs.lines[0].len(), 8192);
    }
    #[tokio::test]
    async fn external_attach_identity_and_preservation() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let cancel = CancellationToken::new();
        let server = tokio::spawn(server(listener, "fixture-chat", cancel.clone()));
        let mut runtime = RuntimeSupervisor::new().unwrap();
        assert!(
            runtime
                .attach(&endpoint, "wrong", RuntimeRole::Chat)
                .await
                .is_err()
        );
        runtime
            .attach(&endpoint, "fixture-chat", RuntimeRole::Chat)
            .await
            .unwrap();
        assert_eq!(runtime.owned_pid(), None);
        runtime.stop().await.unwrap();
        assert!(
            runtime
                .client
                .get(format!("{endpoint}/health"))
                .send()
                .await
                .unwrap()
                .status()
                .is_success()
        );
        cancel.cancel();
        server.await.unwrap();
    }
    #[cfg(unix)]
    #[tokio::test]
    async fn probe_rejects_version_and_missing_controls() {
        let root = temp();
        let binary = fake(&root, "exit 0");
        probe_executable(&binary, &root).await.unwrap();
        fs::write(&binary, "#!/bin/sh\necho rapid-mlx 9.0.0\n").unwrap();
        assert!(probe_executable(&binary, &root).await.is_err());
        fs::write(&binary,"#!/bin/sh\nif [ \"$1\" = --version ]; then echo rapid-mlx 1.0.2; else echo --host; fi\n").unwrap();
        assert!(probe_executable(&binary, &root).await.is_err());
        fs::remove_dir_all(root).unwrap();
    }
    #[cfg(unix)]
    #[tokio::test]
    async fn early_exit_and_occupied_port() {
        let root = temp();
        let model = local_model(&root);
        let binary = fake(&root, "exit 7");
        let mut config = RuntimeConfig::new(
            binary,
            model,
            "fixture-chat".into(),
            RuntimeRole::Chat,
            root.clone(),
        );
        config.app_state_root = Some(root.join("app-state"));
        let socket = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        config.port = socket.local_addr().unwrap().port();
        let mut runtime = RuntimeSupervisor::new().unwrap();
        assert!(
            runtime
                .start(config.clone())
                .await
                .unwrap_err()
                .to_string()
                .contains("occupied")
        );
        assert!(runtime.owned_pid().is_none());
        drop(socket);
        assert!(
            runtime
                .start(config)
                .await
                .unwrap_err()
                .to_string()
                .contains("exited")
        );
        assert!(runtime.owned_pid().is_none());
        fs::remove_dir_all(root).unwrap();
    }
    #[cfg(unix)]
    #[tokio::test]
    async fn startup_timeout_cleans_owned_child() {
        let root = temp();
        let model = local_model(&root);
        let binary = fake(&root, "exec /bin/sleep 30");
        let mut config = RuntimeConfig::new(
            binary,
            model,
            "fixture-chat".into(),
            RuntimeRole::Chat,
            root.clone(),
        );
        config.app_state_root = Some(root.join("app-state"));
        let socket = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        config.port = socket.local_addr().unwrap().port();
        drop(socket);
        config.startup_timeout_secs = 1;
        let mut runtime = RuntimeSupervisor::new().unwrap();
        assert!(
            runtime
                .start(config)
                .await
                .unwrap_err()
                .to_string()
                .contains("deadline")
        );
        assert!(runtime.owned_pid().is_none());
        assert!(matches!(runtime.state(), RuntimeState::Failed { .. }));
        fs::remove_dir_all(root).unwrap();
    }
    #[cfg(unix)]
    #[tokio::test]
    async fn owned_ready_arguments_environment_and_shutdown() {
        let root = temp();
        let model = local_model(&root);
        let binary = fake(
            &root,
            "printf '%s\\n' \"$@\" > \"$HF_HUB_CACHE/args\"\nprintf '%s\\n' \"$HF_HUB_OFFLINE\" \"$RAPID_MLX_TELEMETRY\" \"$RAPID_MLX_DISABLE_VERSION_CHECK\" > \"$HF_HUB_CACHE/env\"\nexec /bin/sleep 30",
        );
        let mut config = RuntimeConfig::new(
            binary,
            model,
            "fixture-chat".into(),
            RuntimeRole::Chat,
            root.clone(),
        );
        config.app_state_root = Some(root.join("app-state"));
        let socket = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = socket.local_addr().unwrap();
        config.port = addr.port();
        drop(socket);
        let cancel = CancellationToken::new();
        let c = cancel.clone();
        let marker = root.join("args");
        let service = tokio::spawn(async move {
            while !marker.exists() {
                sleep(Duration::from_millis(10)).await;
            }
            server(
                tokio::net::TcpListener::bind(addr).await.unwrap(),
                "fixture-chat",
                c,
            )
            .await;
        });
        let registry = ModelUseRegistry::default();
        let mut runtime = RuntimeSupervisor::with_registry(registry.clone()).unwrap();
        runtime.start(config).await.unwrap();
        assert!(matches!(
            runtime.state(),
            RuntimeState::Ready {
                external: false,
                ..
            }
        ));
        let pid = runtime.owned_pid().unwrap();
        let args = fs::read_to_string(root.join("args")).unwrap();
        assert!(args.contains("--disable-model-downloads\n"));
        assert!(args.contains(&format!("--watchdog-ppid\n{}\n", std::process::id())));
        assert!(args.contains("--disable-disk-caches\n"));
        assert_eq!(fs::read_to_string(root.join("env")).unwrap(), "1\n0\n1\n");
        let ops = ModelOperations {
            executable: "unused".into(),
            cache_root: root.clone(),
            uses: registry,
        };
        assert!(ops.removal_preview("fixture/chat").is_err());
        runtime.set_inference_active(true);
        assert!(runtime.stop().await.is_err());
        runtime.set_inference_active(false);
        runtime.stop().await.unwrap();
        assert!(
            Command::new("/bin/kill")
                .args(["-0", &pid.to_string()])
                .stderr(Stdio::null())
                .status()
                .await
                .unwrap()
                .code()
                != Some(0)
        );
        assert!(ops.removal_preview("fixture/chat").is_ok());
        cancel.cancel();
        service.await.unwrap();
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn removal_scope_confirmation_and_active_locks() {
        let root = temp();
        local_model(&root);
        let second = root.join("models--fixture--chat/snapshots/bbbbbbbb");
        fs::create_dir_all(second).unwrap();
        let registry = ModelUseRegistry::default();
        let ops = ModelOperations {
            executable: "unused".into(),
            cache_root: root.clone(),
            uses: registry.clone(),
        };
        let preview = ops.removal_preview("fixture/chat").unwrap();
        assert_eq!(preview.revisions, vec!["aaaaaaaa", "bbbbbbbb"]);
        assert!(ops.removal_preview("../other").is_err());
        let key = repository_use_key(&root, "fixture/chat").unwrap();
        let lease = registry.acquire(&key, "transcribing").unwrap();
        assert!(ops.removal_preview("fixture/chat").is_err());
        drop(lease);
        let exclusive = registry.exclusive(&key).unwrap();
        assert!(registry.acquire(&key, "downloading").is_err());
        drop(exclusive);
        fs::remove_dir_all(root).unwrap();
    }
    #[cfg(unix)]
    #[tokio::test]
    async fn stale_preview_and_confirmation_never_dispatch() {
        let root = temp();
        local_model(&root);
        let binary = fake(&root, "touch \"$HF_HUB_CACHE/dispatched\"");
        let ops = ModelOperations {
            executable: binary,
            cache_root: root.clone(),
            uses: ModelUseRegistry::default(),
        };
        let preview = ops.removal_preview("fixture/chat").unwrap();
        assert!(ops.remove_confirmed(&preview, false).await.is_err());
        fs::create_dir_all(root.join("models--fixture--chat/snapshots/new")).unwrap();
        assert!(ops.remove_confirmed(&preview, true).await.is_err());
        assert!(!root.join("dispatched").exists());
        fs::remove_dir_all(root).unwrap();
    }
    #[cfg(unix)]
    #[tokio::test]
    async fn cancelled_pull_and_incomplete_success_remain_not_ready() {
        let root = temp();
        let binary = fake(&root, "if [ \"$1\" = pull ]; then exec /bin/sleep 30; fi");
        let ops = ModelOperations {
            executable: binary,
            cache_root: root.clone(),
            uses: ModelUseRegistry::default(),
        };
        let cancel = CancellationToken::new();
        cancel.cancel();
        let result = ops.pull("fixture/chat", cancel).await.unwrap();
        assert!(result.cancelled && !result.success);
        assert!(
            ops.uses
                .reasons(&repository_use_key(&root, "fixture/chat").unwrap())
                .is_empty()
        );
        fake(&root, "exit 0");
        let result = ops
            .pull("fixture/chat", CancellationToken::new())
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.logs.iter().any(|s| s.contains("validation failed")));
        fs::remove_dir_all(root).unwrap();
    }
    #[tokio::test]
    async fn authenticated_external_check_retains_only_memory_credential() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let service = tokio::spawn(async move {
            loop {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut bytes = [0; 4096];
                let n = socket.read(&mut bytes).await.unwrap();
                let request = String::from_utf8_lossy(&bytes[..n]);
                let authorized = request
                    .to_lowercase()
                    .contains("authorization: bearer fixture-secret");
                let body = if !authorized {
                    "{}".into()
                } else if request.starts_with("GET /v1/models ") {
                    serde_json::json!({"data":[{"id":"fixture-chat"}]}).to_string()
                } else if request.starts_with("GET /openapi.json ") {
                    include_str!("../../../fixtures/runtime/openapi.json").into()
                } else {
                    include_str!("../../../fixtures/runtime/health.json").into()
                };
                let status = if authorized {
                    "200 OK"
                } else {
                    "401 Unauthorized"
                };
                socket.write_all(format!("HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
            }
        });
        let mut runtime = RuntimeSupervisor::new().unwrap();
        assert!(
            runtime
                .attach(&endpoint, "fixture-chat", RuntimeRole::Chat)
                .await
                .is_err()
        );
        runtime
            .attach_with_credential(
                &endpoint,
                "fixture-chat",
                RuntimeRole::Chat,
                Some("fixture-secret"),
            )
            .await
            .unwrap();
        assert!(matches!(
            runtime.check().await.unwrap(),
            RuntimeState::Ready { external: true, .. }
        ));
        assert!(
            !runtime
                .logs()
                .iter()
                .any(|line| line.contains("fixture-secret"))
        );
        runtime.stop().await.unwrap();
        assert!(runtime.credential.is_none());
        service.abort();
    }
    #[test]
    fn asr_role_uses_audio_metadata_without_chat_tokenizer() {
        let root = temp();
        let path = local_model(&root);
        fs::write(path.join("config.json"), r#"{"model_type":"parakeet"}"#).unwrap();
        fs::remove_file(path.join("tokenizer.json")).unwrap();
        let config = RuntimeConfig::new(
            "unused".into(),
            path.clone(),
            "fixture-asr".into(),
            RuntimeRole::Transcription,
            root.clone(),
        );
        assert!(config.arguments().is_ok());
        fs::write(path.join("config.json"), r#"{"model_type":"whisper"}"#).unwrap();
        fs::remove_file(path.join("model.safetensors")).unwrap();
        fs::write(path.join("weights.npz"), b"synthetic npz weights").unwrap();
        let arguments = config.arguments().unwrap();
        assert_eq!(arguments[1], path.to_str().unwrap());
        assert!(
            arguments
                .iter()
                .any(|arg| arg == "--disable-model-downloads")
        );
        fs::write(path.join("config.json"), r#"{"model_type":"kokoro"}"#).unwrap();
        assert!(config.arguments().is_err());
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn removal_rejects_hash_collision_with_dataset() {
        let root = temp();
        local_model(&root);
        fs::create_dir_all(root.join("datasets--fixture--chat/snapshots/aaaaaaaa")).unwrap();
        let ops = ModelOperations {
            executable: "unused".into(),
            cache_root: root.clone(),
            uses: ModelUseRegistry::default(),
        };
        assert!(
            ops.removal_preview("fixture/chat")
                .unwrap_err()
                .to_string()
                .contains("retained repository")
        );
        fs::remove_dir_all(root).unwrap();
    }
    #[cfg(unix)]
    #[tokio::test]
    async fn owned_shutdown_removes_descendant_group() {
        let root = temp();
        let binary = fake(
            &root,
            "/bin/sleep 30 &\necho $! > \"$HF_HUB_CACHE/descendant\"\nwait",
        );
        let mut child = command(&binary, &root, true).arg("serve").spawn().unwrap();
        let marker = root.join("descendant");
        for _ in 0..100 {
            if marker.exists() {
                break;
            }
            sleep(Duration::from_millis(10)).await;
        }
        let pid = fs::read_to_string(&marker).unwrap();
        terminate(&mut child).await.unwrap();
        let status = Command::new("/bin/kill")
            .args(["-0", pid.trim()])
            .stderr(Stdio::null())
            .status()
            .await
            .unwrap();
        assert!(!status.success());
        fs::remove_dir_all(root).unwrap();
    }
    #[cfg(unix)]
    #[tokio::test]
    async fn successful_pull_and_confirmed_remove_use_selected_cache() {
        let root = temp();
        let model = local_model(&root);
        let binary = fake(
            &root,
            "printf '%s\\n' \"$@\" > \"$HF_HUB_CACHE/operation-args\"\nif [ \"$1\" = rm ]; then /bin/rm -rf \"$HF_HUB_CACHE/models--fixture--chat\"; fi",
        );
        let ops = ModelOperations {
            executable: binary,
            cache_root: root.clone(),
            uses: ModelUseRegistry::default(),
        };
        let pulled = ops
            .pull("fixture/chat", CancellationToken::new())
            .await
            .unwrap();
        assert!(pulled.success);
        let retained = root.join("datasets--fixture--chat/snapshots/other");
        fs::create_dir_all(&retained).unwrap();
        let source = root.join("custom-source");
        fs::create_dir_all(&source).unwrap();
        fs::write(source.join("keep"), b"source").unwrap();
        let session = root.join("sessions.sqlite3");
        fs::write(&session, b"session").unwrap();
        let preview = ops.removal_preview("fixture/chat").unwrap();
        let result = ops.remove_confirmed(&preview, true).await.unwrap();
        assert!(result.success);
        assert!(result.actual_reclaimed_bytes.is_some());
        assert!(!model.exists());
        assert_eq!(
            fs::read_to_string(root.join("operation-args")).unwrap(),
            "rm\nfixture/chat\n--yes\n"
        );
        assert!(retained.exists());
        assert_eq!(fs::read(source.join("keep")).unwrap(), b"source");
        assert_eq!(fs::read(session).unwrap(), b"session");
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn fixture_manifest_covers_all_runtime_evidence() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/runtime");
        let manifest: Value =
            serde_json::from_slice(&fs::read(root.join("provenance.json")).unwrap()).unwrap();
        let fixtures = manifest["fixtures"].as_object().unwrap();
        for file in fs::read_dir(root).unwrap() {
            let file = file.unwrap();
            let name = file.file_name();
            let name = name.to_str().unwrap();
            if name != "provenance.json" {
                assert!(fixtures.contains_key(name), "missing provenance: {name}");
            }
        }
    }
    #[tokio::test]
    async fn health_http_success_without_ready_is_not_readiness() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let service = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = [0; 2048];
            let received = socket.read(&mut request).await.unwrap();
            assert!(received > 0);
            let body =
                r#"{"status":"healthy","ready":false,"model_loaded":true,"model_type":"llm"}"#;
            socket
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
        });
        let mut runtime = RuntimeSupervisor::new().unwrap();
        assert!(
            runtime
                .attach(&endpoint, "fixture-chat", RuntimeRole::Chat)
                .await
                .unwrap_err()
                .to_string()
                .contains("warmup")
        );
        assert_eq!(runtime.state(), &RuntimeState::Stopped);
        service.await.unwrap();
    }
    #[cfg(unix)]
    #[tokio::test]
    async fn alias_pull_preserves_reference_and_rejects_repo_mismatch_before_dispatch() {
        let root = temp();
        let model = local_model(&root);
        let variant = model.join("4bit");
        fs::create_dir_all(&variant).unwrap();
        for name in ["config.json", "model.safetensors", "tokenizer.json"] {
            fs::rename(model.join(name), variant.join(name)).unwrap();
        }
        fs::write(
            root.join("available.json"),
            include_bytes!("../../../fixtures/runtime/available.json"),
        )
        .unwrap();
        fs::write(root.join("cached.json"), r#"{"cached":[{"repo":"fixture/chat","alias":"fixture-chat","state":"ok","size_bytes":4,"external":false,"subfolder":"4bit"}]}"#).unwrap();
        let binary = fake(
            &root,
            r#"if [ "$1" = models ]; then
  if [ "$2" = --cached ]; then cat "$HF_HUB_CACHE/cached.json"; else cat "$HF_HUB_CACHE/available.json"; fi
  exit
fi
printf '%s\n' "$@" > "$HF_HUB_CACHE/dispatched"
if [ "$1" = pull ]; then /bin/sleep 0.1; fi"#,
        );
        let registry = ModelUseRegistry::default();
        let ops = ModelOperations {
            executable: binary,
            cache_root: root.clone(),
            uses: registry.clone(),
        };
        assert!(
            ops.pull_reference("fixture-chat", "fixture/wrong", CancellationToken::new())
                .await
                .is_err()
        );
        assert!(
            ops.pull_reference("unknown", "fixture/chat", CancellationToken::new())
                .await
                .is_err()
        );
        assert!(!root.join("dispatched").exists());
        let pull_ops = ops.clone();
        let task = tokio::spawn(async move {
            pull_ops
                .pull_reference("fixture-chat", "fixture/chat", CancellationToken::new())
                .await
        });
        for _ in 0..100 {
            if root.join("dispatched").exists() {
                break;
            }
            sleep(Duration::from_millis(5)).await;
        }
        let key = repository_use_key(&root, "fixture/chat").unwrap();
        assert_eq!(registry.reasons(&key), vec!["downloading"]);
        assert_eq!(
            fs::read_to_string(root.join("dispatched")).unwrap(),
            "pull\nfixture-chat\n"
        );
        assert!(task.await.unwrap().unwrap().success);
        assert!(registry.reasons(&key).is_empty());
        let sibling = model.join("8bit");
        fs::create_dir_all(&sibling).unwrap();
        for name in ["config.json", "model.safetensors", "tokenizer.json"] {
            fs::copy(variant.join(name), sibling.join(name)).unwrap();
        }
        fs::remove_file(variant.join("tokenizer.json")).unwrap();
        let result = ops
            .pull_reference("fixture-chat", "fixture/chat", CancellationToken::new())
            .await
            .unwrap();
        assert!(!result.success);
        assert!(
            result
                .logs
                .iter()
                .any(|line| line.contains("sibling is not a substitute"))
        );
        fs::remove_dir_all(root).unwrap();
    }
    #[tokio::test]
    async fn advertised_models_is_authenticated_and_independent_of_chat_routes() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let service = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = [0; 4096];
            let count = socket.read(&mut request).await.unwrap();
            let request = String::from_utf8_lossy(&request[..count]);
            assert!(request.starts_with("GET /v1/models HTTP/1.1"));
            assert!(
                request
                    .to_ascii_lowercase()
                    .contains("authorization: bearer fixture-secret")
            );
            let body =
                r#"{"data":[{"id":"fixture-asr"},{"id":"fixture-asr"},{"id":"second-asr"}]}"#;
            socket
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
        });
        assert_eq!(
            advertised_models(&endpoint, Some("fixture-secret"))
                .await
                .unwrap(),
            vec!["fixture-asr", "second-asr"]
        );
        service.await.unwrap();
        assert!(
            advertised_models("http://192.168.1.1:8000", None)
                .await
                .is_err()
        );
    }
    #[tokio::test]
    async fn advertised_models_rejects_malformed_and_oversized_responses() {
        for body in [r#"{"data":[{}]}"#.to_owned(), "x".repeat(HTTP_LIMIT + 1)] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let endpoint = format!("http://{}", listener.local_addr().unwrap());
            let service = tokio::spawn(async move {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut request = [0; 4096];
                let count = socket.read(&mut request).await.unwrap();
                assert!(count > 0);
                let _ = socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await;
            });
            assert!(advertised_models(&endpoint, None).await.is_err());
            service.await.unwrap();
        }
    }
    #[cfg(unix)]
    async fn assert_failed_pull_preserves_complete_cache(message: &str, exit_code: i32) {
        let root = temp();
        let existing = local_model(&root);
        let config = existing.join("config.json");
        let weights = existing.join("model.safetensors");
        let config_bytes = fs::read(&config).unwrap();
        let config_metadata = fs::metadata(&config).unwrap();
        let weights_bytes = fs::read(&weights).unwrap();
        let weights_metadata = fs::metadata(&weights).unwrap();
        fs::write(root.join("failure-message"), message).unwrap();
        let binary = fake(
            &root,
            &format!(
                r#"if [ "$1" = pull ]; then
  mkdir -p "$HF_HUB_CACHE/models--fixture--failed/blobs"
  printf 'partial transfer' > "$HF_HUB_CACHE/models--fixture--failed/blobs/fixture.incomplete"
  cat "$HF_HUB_CACHE/failure-message" >&2
  exit {exit_code}
fi
exit 99"#
            ),
        );
        let registry = ModelUseRegistry::default();
        let ops = ModelOperations {
            executable: binary,
            cache_root: root.clone(),
            uses: registry.clone(),
        };
        let result = ops
            .pull("fixture/failed", CancellationToken::new())
            .await
            .unwrap();
        assert!(!result.success);
        assert!(!result.cancelled);
        assert_eq!(result.exit_code, Some(exit_code));
        assert!(result.logs.iter().any(|line| line.contains(message)));
        assert!(
            registry
                .reasons(&repository_use_key(&root, "fixture/failed").unwrap())
                .is_empty()
        );
        assert_eq!(fs::read(&config).unwrap(), config_bytes);
        assert_eq!(
            fs::metadata(&config).unwrap().modified().unwrap(),
            config_metadata.modified().unwrap()
        );
        assert_eq!(fs::read(&weights).unwrap(), weights_bytes);
        assert_eq!(
            fs::metadata(&weights).unwrap().modified().unwrap(),
            weights_metadata.modified().unwrap()
        );
        let report = crate::catalog::discover(&root, &[]);
        assert!(
            report
                .models
                .iter()
                .any(|model| model.repository.as_deref() == Some("fixture/chat")
                    && model.can_attempt_load())
        );
        assert!(
            !report.models.iter().any(|model| model.repository.as_deref()
                == Some("fixture/failed")
                && model.can_attempt_load())
        );
        assert_eq!(
            fs::read(root.join("models--fixture--failed/blobs/fixture.incomplete")).unwrap(),
            b"partial transfer"
        );
        fs::remove_dir_all(root).unwrap();
    }
    #[cfg(unix)]
    #[tokio::test]
    async fn pull_network_failure_preserves_complete_sibling_and_partial_state() {
        assert_failed_pull_preserves_complete_cache(
            "Network transfer failed: connection reset by peer",
            23,
        )
        .await;
    }
    #[cfg(unix)]
    #[tokio::test]
    async fn pull_enospc_failure_preserves_complete_sibling_and_partial_state() {
        assert_failed_pull_preserves_complete_cache(
            "OSError: [Errno 28] No space left on device",
            28,
        )
        .await;
    }
}
