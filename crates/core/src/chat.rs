//! Loopback-only Chat Completions transport. No persistence or automatic retries.
use anyhow::{Context, Result, anyhow, ensure};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    net::IpAddr,
    time::{Duration, Instant},
};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

pub const MAX_STREAM_BYTES: usize = 16 * 1024 * 1024;
pub const MAX_FRAME_BYTES: usize = 1024 * 1024;
pub const MAX_TOOL_CYCLES: usize = 8;

/// Numeric loopback only: avoids DNS rebinding and accidental proxy routing.
pub fn validate_endpoint(endpoint: &str) -> Result<reqwest::Url> {
    let mut url = reqwest::Url::parse(endpoint).context("Invalid inference endpoint")?;
    ensure!(
        matches!(url.scheme(), "http" | "https"),
        "Inference endpoint must use HTTP(S)"
    );
    ensure!(
        url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none(),
        "Endpoint cannot contain credentials, query or fragment"
    );
    let host = url.host_str().unwrap_or_default().trim_matches(['[', ']']);
    let ip: IpAddr = host
        .parse()
        .context("Use a numeric loopback address (127.0.0.1 or [::1])")?;
    ensure!(ip.is_loopback(), "Inference endpoint must be loopback");
    ensure!(
        matches!(url.path(), "" | "/" | "/v1" | "/v1/"),
        "Endpoint must be a server origin or /v1"
    );
    url.set_path("/");
    Ok(url)
}
fn client() -> Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(5))
        .build()?)
}
fn auth(builder: reqwest::RequestBuilder, credential: Option<&str>) -> reqwest::RequestBuilder {
    match credential {
        Some(secret) => builder.bearer_auth(secret),
        None => builder,
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: Value,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum ChatEvent {
    Identity {
        id: Option<String>,
        model: Option<String>,
    },
    Content(String),
    Reasoning(String),
    Usage(Value),
    ToolCalls(Vec<ToolCall>),
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ChatStatus {
    Completed,
    Cancelled,
    Failed,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatOutcome {
    /// Exact serialized request; credentials are never included.
    pub request: Value,
    pub request_id: Option<String>,
    pub model: Option<String>,
    pub content: String,
    pub reasoning: String,
    pub tool_calls: Vec<ToolCall>,
    /// Latest cumulative usage replaces earlier reports; absent counts stay unknown.
    pub usage: Option<Value>,
    pub finish_reason: Option<String>,
    pub elapsed_ms: u64,
    pub first_output_ms: Option<u64>,
    pub status: ChatStatus,
    pub error: Option<String>,
    pub server_cancel_confirmed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Capabilities {
    pub runtime_version: Option<String>,
    pub schema_version: Option<String>,
    pub model_ids: Vec<String>,
    pub schema: Value,
    pub models: Value,
    pub cancellation: bool,
}
impl Capabilities {
    pub fn has_request_field(&self, field: &str) -> bool {
        self.schema
            .pointer("/components/schemas/ChatCompletionRequest/properties")
            .and_then(|p| p.get(field))
            .is_some()
    }
}
async fn bounded_body(response: reqwest::Response, limit: usize) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        ensure!(
            bytes.len() + chunk.len() <= limit,
            "Runtime body exceeds limit"
        );
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}
async fn bounded_json(response: reqwest::Response) -> Result<Value> {
    ensure!(
        response.status().is_success(),
        "Runtime HTTP {}",
        response.status()
    );
    Ok(serde_json::from_slice(
        &bounded_body(response, 4 * MAX_FRAME_BYTES).await?,
    )?)
}
/// Probe only on explicit connect. Optional controls still require per-model evidence.
pub async fn negotiate(endpoint: &str, credential: Option<&str>) -> Result<Capabilities> {
    negotiate_for_runtime(endpoint, credential, None).await
}
/// `runtime_version` is the separately probed executable version. OpenAPI info.version
/// in rapid-mlx 1.0.2 is 0.6.0, so it cannot establish package compatibility.
pub async fn negotiate_for_runtime(
    endpoint: &str,
    credential: Option<&str>,
    runtime_version: Option<&str>,
) -> Result<Capabilities> {
    let base = validate_endpoint(endpoint)?;
    let client = client()?;
    let schema = bounded_json(
        auth(client.get(base.join("openapi.json")?), credential)
            .timeout(Duration::from_secs(10))
            .send()
            .await?,
    )
    .await?;
    for (path, method) in [
        ("/health", "get"),
        ("/v1/models", "get"),
        ("/v1/chat/completions", "post"),
    ] {
        ensure!(
            schema
                .get("paths")
                .and_then(|p| p.get(path))
                .and_then(|p| p.get(method))
                .is_some(),
            "Required schema route missing: {path}"
        );
    }
    let health = auth(client.get(base.join("health")?), credential)
        .timeout(Duration::from_secs(10))
        .send()
        .await?;
    ensure!(
        health.status().is_success(),
        "Runtime health HTTP {}",
        health.status()
    );
    let models = bounded_json(
        auth(client.get(base.join("v1/models")?), credential)
            .timeout(Duration::from_secs(10))
            .send()
            .await?,
    )
    .await?;
    let model_ids = models
        .get("data")
        .and_then(Value::as_array)
        .context("Invalid models envelope")?
        .iter()
        .map(|m| {
            m.get("id")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .context("Missing served model id")
        })
        .collect::<Result<Vec<_>>>()?;
    let schema_version = schema
        .pointer("/info/version")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let runtime_version = runtime_version.map(str::to_owned);
    let cancellation = runtime_version.as_deref() == Some("1.0.2")
        && schema
            .pointer("/paths/~1v1~1requests~1{request_id}~1cancel/post")
            .is_some();
    Ok(Capabilities {
        runtime_version,
        schema_version,
        model_ids,
        schema,
        models,
        cancellation,
    })
}

/// Evidence must be established for the actual served tokenizer/template, not inferred from its name.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum EffortProfile {
    Unknown,
    ThinkingBudget,
    NativeLevels(Vec<String>),
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EffortMapping {
    pub requested: String,
    pub fields: Value,
    pub normalization: Value,
}
/// Mirrors 1.0.2 api/models.py + service/helpers.py + utils/chat_template.py.
pub fn effort_mapping(
    version: &str,
    profile: &EffortProfile,
    effort: &str,
) -> Option<EffortMapping> {
    let ladder = ["none", "minimal", "low", "medium", "high", "xhigh"];
    let rank = ladder.iter().position(|x| *x == effort)?;
    if version != "1.0.2" || matches!(profile, EffortProfile::Unknown) {
        return None;
    }
    let normalization = if effort == "none" {
        json!({"chat_template_kwargs": {"enable_thinking": false}})
    } else if let EffortProfile::NativeLevels(levels) = profile {
        let native = levels
            .iter()
            .filter_map(|level| {
                ladder
                    .iter()
                    .position(|x| {
                        *x == if level == "max" {
                            "xhigh"
                        } else {
                            level.as_str()
                        }
                    })
                    .map(|r| (r, level))
            })
            .min_by_key(|(r, _)| (r.abs_diff(rank), std::cmp::Reverse(*r)))?
            .1;
        json!({"chat_template_kwargs": {"reasoning_effort": native}})
    } else {
        json!({"reasoning_max_tokens": ([0, 256, 512, 2048, 8192, 24000][rank])})
    };
    Some(EffortMapping {
        requested: effort.to_owned(),
        fields: json!({"reasoning_effort": effort}),
        normalization,
    })
}
/// Validate against the explicitly connected server. Unknown optional controls are rejected.
pub fn validate_request(
    request: &Value,
    caps: &Capabilities,
    profile: &EffortProfile,
    tools_verified: bool,
) -> Result<()> {
    let object = request
        .as_object()
        .context("Chat request must be an object")?;
    let model = request
        .get("model")
        .and_then(Value::as_str)
        .context("Select a model")?;
    ensure!(
        caps.model_ids.iter().any(|m| m == model),
        "Selected model is not served"
    );
    let messages = request
        .get("messages")
        .and_then(Value::as_array)
        .context("Messages required")?;
    ensure!(!messages.is_empty(), "Messages cannot be empty");
    ensure!(
        messages.iter().any(|m| m
            .get("content")
            .and_then(Value::as_str)
            .is_some_and(|s| !s.trim().is_empty())),
        "Non-empty input required"
    );
    for (key, max) in [("temperature", 2.0), ("top_p", 1.0)] {
        if let Some(value) = object.get(key) {
            ensure!(caps.has_request_field(key), "Unsupported setting: {key}");
            let number = value.as_f64().context("Sampling value must be numeric")?;
            ensure!(
                number.is_finite()
                    && number >= 0.0
                    && number <= max
                    && (key != "top_p" || number > 0.0),
                "Invalid {key}"
            );
        }
    }
    for key in ["max_tokens", "max_completion_tokens"] {
        if let Some(value) = object.get(key) {
            ensure!(
                caps.has_request_field(key) && value.as_u64().is_some_and(|n| n > 0),
                "Invalid or unsupported output limit"
            );
        }
    }
    ensure!(
        ![
            "reasoning_max_tokens",
            "enable_thinking",
            "chat_template_kwargs"
        ]
        .iter()
        .any(|k| object.contains_key(*k)),
        "Native reasoning overrides are not supported by this validated request builder"
    );
    if let Some(effort) = object.get("reasoning_effort") {
        ensure!(
            caps.has_request_field("reasoning_effort")
                && effort
                    .as_str()
                    .and_then(|e| effort_mapping(
                        caps.runtime_version.as_deref().unwrap_or(""),
                        profile,
                        e
                    ))
                    .is_some(),
            "Reasoning effort unavailable for this model"
        );
    }
    if object.contains_key("tools") {
        ensure!(
            tools_verified && caps.has_request_field("tools"),
            "Tool support is unverified"
        );
    }
    ensure!(
        request.get("n").is_none_or(|n| n == 1),
        "Only one completion is supported"
    );
    Ok(())
}

/// Byte-oriented SSE decoder: UTF-8 is decoded only after a full event is assembled.
#[derive(Default)]
pub struct SseDecoder {
    pending: Vec<u8>,
    data: Vec<String>,
    event: Option<String>,
    frame_bytes: usize,
    after_cr: bool,
}
#[derive(Debug, PartialEq)]
pub struct SseFrame {
    pub event: Option<String>,
    pub data: String,
}
impl SseDecoder {
    pub fn push(&mut self, bytes: &[u8]) -> Result<Vec<SseFrame>> {
        let mut frames = Vec::new();
        for &byte in bytes {
            if self.after_cr {
                self.after_cr = false;
                if byte == b'\n' {
                    continue;
                }
            }
            if byte == b'\r' || byte == b'\n' {
                self.after_cr = byte == b'\r';
                if let Some(frame) = self.line()? {
                    frames.push(frame);
                }
            } else {
                self.pending.push(byte);
                ensure!(
                    self.pending.len() + self.frame_bytes <= MAX_FRAME_BYTES,
                    "SSE frame exceeds limit"
                );
            }
        }
        Ok(frames)
    }
    fn line(&mut self) -> Result<Option<SseFrame>> {
        let line =
            String::from_utf8(std::mem::take(&mut self.pending)).context("Invalid SSE UTF-8")?;
        if line.is_empty() {
            self.frame_bytes = 0;
            let event = self.event.take();
            return Ok(if self.data.is_empty() {
                None
            } else {
                Some(SseFrame {
                    event,
                    data: std::mem::take(&mut self.data).join("\n"),
                })
            });
        }
        self.frame_bytes += line.len();
        ensure!(
            self.frame_bytes <= MAX_FRAME_BYTES,
            "SSE frame exceeds limit"
        );
        if line.starts_with(':') {
            return Ok(None);
        }
        let (field, value) = line.split_once(':').unwrap_or((&line, ""));
        let value = value.strip_prefix(' ').unwrap_or(value);
        match field {
            "data" => self.data.push(value.to_owned()),
            "event" => self.event = Some(value.to_owned()),
            _ => {}
        }
        Ok(None)
    }
    pub fn finish(&self) -> Result<()> {
        ensure!(
            self.pending.is_empty() && self.data.is_empty(),
            "Disconnected within an SSE frame"
        );
        Ok(())
    }
}
#[derive(Default)]
struct PartialCall {
    id: String,
    name: String,
    arguments: String,
}
struct Assembly {
    calls: BTreeMap<u64, PartialCall>,
    bytes: usize,
    done: bool,
    finished: bool,
}
impl Assembly {
    fn frame(&mut self, frame: SseFrame, out: &mut ChatOutcome) -> Result<Vec<ChatEvent>> {
        ensure!(!self.done, "Data after terminal marker");
        if frame.data.trim() == "[DONE]" {
            self.done = true;
            ensure!(self.finished, "DONE without finish reason");
            return Ok(vec![]);
        }
        let value: Value = serde_json::from_str(&frame.data).context("Malformed SSE JSON")?;
        ensure!(
            frame.event.as_deref() != Some("error") && value.get("error").is_none(),
            "Runtime returned a streaming error: {}",
            value.get("error").unwrap_or(&value)
        );
        let mut events = vec![];
        let id = value.get("id").and_then(Value::as_str);
        let model = value.get("model").and_then(Value::as_str);
        if let Some(id) = id {
            ensure!(
                out.request_id.as_deref().is_none_or(|old| old == id),
                "Request identity changed"
            );
            out.request_id = Some(id.to_owned());
        }
        if let Some(model) = model {
            ensure!(
                out.model.as_deref().is_none_or(|old| old == model),
                "Response model differs from requested model"
            );
        }
        if id.is_some() || model.is_some() {
            events.push(ChatEvent::Identity {
                id: out.request_id.clone(),
                model: out.model.clone(),
            });
        }
        if let Some(usage) = value.get("usage").filter(|v| !v.is_null()) {
            out.usage = Some(usage.clone());
            events.push(ChatEvent::Usage(usage.clone()));
        }
        let choices = value
            .get("choices")
            .and_then(Value::as_array)
            .context("Missing choices array")?;
        ensure!(choices.len() <= 1, "Multiple choices unsupported");
        for choice in choices {
            ensure!(
                choice.get("index").and_then(Value::as_u64).unwrap_or(0) == 0,
                "Unexpected choice index"
            );
            let delta = choice.get("delta").context("Missing delta")?;
            ensure!(delta.is_object(), "Delta must be an object");
            for (key, target, reasoning) in [
                ("content", &mut out.content, false),
                ("reasoning_content", &mut out.reasoning, true),
            ] {
                if let Some(value) = delta.get(key).filter(|v| !v.is_null()) {
                    let text = value.as_str().context("Delta must be text")?;
                    ensure!(
                        !self.finished || text.is_empty(),
                        "Content after finish reason"
                    );
                    self.bytes += text.len();
                    ensure!(self.bytes <= MAX_STREAM_BYTES, "Chat output exceeds limit");
                    target.push_str(text);
                    if !text.is_empty() {
                        events.push(if reasoning {
                            ChatEvent::Reasoning(text.to_owned())
                        } else {
                            ChatEvent::Content(text.to_owned())
                        });
                    }
                }
            }
            if let Some(calls) = delta.get("tool_calls") {
                ensure!(!self.finished, "Tool delta after finish reason");
                for call in calls.as_array().context("Invalid tool deltas")? {
                    let index = call
                        .get("index")
                        .and_then(Value::as_u64)
                        .context("Tool index required")?;
                    ensure!(index < 128, "Too many tool calls");
                    let partial = self.calls.entry(index).or_default();
                    if let Some(id) = call.get("id").and_then(Value::as_str) {
                        ensure!(
                            partial.id.is_empty() || partial.id == id,
                            "Tool identity changed"
                        );
                        partial.id = id.to_owned();
                    }
                    if let Some(kind) = call.get("type") {
                        ensure!(kind == "function", "Unsupported tool type");
                    }
                    if let Some(function) = call.get("function") {
                        for (key, target) in [
                            ("name", &mut partial.name),
                            ("arguments", &mut partial.arguments),
                        ] {
                            if let Some(value) = function.get(key) {
                                let text = value.as_str().context("Tool fragment must be text")?;
                                self.bytes += text.len();
                                ensure!(
                                    self.bytes <= MAX_STREAM_BYTES,
                                    "Tool output exceeds limit"
                                );
                                target.push_str(text);
                            }
                        }
                    }
                }
            }
            if let Some(reason) = choice.get("finish_reason").filter(|v| !v.is_null()) {
                let reason = reason.as_str().context("Invalid finish reason")?;
                ensure!(!self.finished, "Duplicate finish reason");
                out.finish_reason = Some(reason.to_owned());
                self.finished = true;
            }
        }
        Ok(events)
    }
    fn complete(&self, out: &mut ChatOutcome) -> Result<()> {
        ensure!(
            self.done && self.finished,
            "Stream disconnected before DONE"
        );
        ensure!(
            matches!(out.finish_reason.as_deref(), Some("stop" | "tool_calls")),
            "Generation ended incompletely: {}",
            out.finish_reason.as_deref().unwrap_or("unknown")
        );
        if !self.calls.is_empty() {
            ensure!(
                out.finish_reason.as_deref() == Some("tool_calls"),
                "Tool calls without tool finish reason"
            );
        }
        let mut ids = std::collections::HashSet::new();
        let mut calls = vec![];
        for partial in self.calls.values() {
            ensure!(
                !partial.id.is_empty() && !partial.name.is_empty() && ids.insert(&partial.id),
                "Missing or duplicate tool identity"
            );
            let arguments: Value =
                serde_json::from_str(&partial.arguments).context("Incomplete tool arguments")?;
            ensure!(arguments.is_object(), "Tool arguments must be an object");
            calls.push(ToolCall {
                id: partial.id.clone(),
                name: partial.name.clone(),
                arguments,
            });
        }
        out.tool_calls = calls;
        Ok(())
    }
}

/// Build the immutable wire snapshot before the parent commits durable intent.
/// Calling this again is idempotent; credentials are always a separate argument.
pub fn prepare_request(mut request: Value) -> Result<Value> {
    let object = request
        .as_object_mut()
        .context("Request must be an object")?;
    object.insert("stream".to_owned(), json!(true));
    object.insert("stream_options".to_owned(), json!({"include_usage": true}));
    ensure!(
        request
            .get("model")
            .and_then(Value::as_str)
            .is_some_and(|m| !m.is_empty()),
        "Model required"
    );
    Ok(request)
}

/// Simple transport entry point. The caller validates capabilities and commits intent first.
/// Operational failures are returned as a terminal outcome preserving partial content.
pub async fn stream_chat(
    endpoint: &str,
    credential: Option<&str>,
    request: Value,
    events: mpsc::Sender<ChatEvent>,
    cancel: CancellationToken,
) -> Result<ChatOutcome> {
    stream_chat_with_capabilities(endpoint, credential, request, events, cancel, None).await
}
pub async fn stream_chat_with_capabilities(
    endpoint: &str,
    credential: Option<&str>,
    request: Value,
    events: mpsc::Sender<ChatEvent>,
    cancel: CancellationToken,
    capabilities: Option<&Capabilities>,
) -> Result<ChatOutcome> {
    let base = validate_endpoint(endpoint)?;
    let request = prepare_request(request)?;
    let mut out = ChatOutcome {
        model: request
            .get("model")
            .and_then(Value::as_str)
            .map(str::to_owned),
        request,
        request_id: None,
        content: String::new(),
        reasoning: String::new(),
        tool_calls: vec![],
        usage: None,
        finish_reason: None,
        elapsed_ms: 0,
        first_output_ms: None,
        status: ChatStatus::Failed,
        error: None,
        server_cancel_confirmed: false,
    };
    let client = client()?;
    let start = Instant::now();
    let mut assembly = Assembly {
        calls: BTreeMap::new(),
        bytes: 0,
        done: false,
        finished: false,
    };
    let operation = async {
        let response = auth(client.post(base.join("v1/chat/completions")?), credential)
            .json(&out.request)
            .send()
            .await
            .context("Cannot connect to local runtime")?;
        if !response.status().is_success() {
            let status = response.status();
            let detail = bounded_body(response, 16 * 1024)
                .await
                .map(|body| String::from_utf8_lossy(&body).into_owned())
                .unwrap_or_else(|_| "Error body unavailable or exceeds limit".to_owned());
            return Err(anyhow!(
                "Chat HTTP {status} (check model, settings and credentials): {detail}"
            ));
        }
        ensure!(
            response
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|h| h.to_str().ok())
                .is_some_and(|s| s.split(';').next() == Some("text/event-stream")),
            "Expected SSE response"
        );
        let mut stream = response.bytes_stream();
        let mut decoder = SseDecoder::default();
        let mut wire_bytes = 0;
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.context("Chat stream disconnected")?;
            wire_bytes += chunk.len();
            ensure!(
                wire_bytes <= 2 * MAX_STREAM_BYTES,
                "Stream exceeds wire limit"
            );
            for byte in chunk.iter() {
                for frame in decoder.push(std::slice::from_ref(byte))? {
                    for event in assembly.frame(frame, &mut out)? {
                        if out.first_output_ms.is_none()
                            && matches!(event, ChatEvent::Content(_) | ChatEvent::Reasoning(_))
                        {
                            out.first_output_ms = Some(start.elapsed().as_millis() as u64);
                        }
                        events
                            .send(event)
                            .await
                            .map_err(|_| anyhow!("Chat event receiver closed"))?;
                    }
                }
            }
            if assembly.done {
                break;
            }
        }
        decoder.finish()?;
        assembly.complete(&mut out)?;
        if !out.tool_calls.is_empty() {
            events
                .send(ChatEvent::ToolCalls(out.tool_calls.clone()))
                .await
                .map_err(|_| anyhow!("Chat event receiver closed"))?;
        }
        Ok::<_, anyhow::Error>(())
    };
    tokio::select! {
        biased;
        _ = cancel.cancelled() => { out.status = ChatStatus::Cancelled; },
        result = tokio::time::timeout(Duration::from_secs(3600), operation) => {
            match result { Ok(Ok(())) => out.status = ChatStatus::Completed, Ok(Err(error)) => out.error = Some(error.to_string()), Err(_) => out.error = Some("Chat deadline exceeded".to_owned()) }
        }
    }
    // Local stream is already dropped before the best-effort explicit cancellation.
    if out.status == ChatStatus::Cancelled
        && capabilities.is_some_and(|c| c.cancellation)
        && let Some(id) = &out.request_id
    {
        let mut url = base.clone();
        url.path_segments_mut()
            .map_err(|_| anyhow!("Invalid base URL"))?
            .extend(["v1", "requests", id, "cancel"]);
        if let Ok(response) = auth(client.post(url), credential)
            .timeout(Duration::from_secs(2))
            .send()
            .await
            && let Ok(value) = bounded_json(response).await
        {
            out.server_cancel_confirmed = value.get("id").and_then(Value::as_str) == Some(id)
                && value.get("cancelled") == Some(&json!(true));
        }
    }
    if out.status != ChatStatus::Completed {
        out.tool_calls.clear();
    }
    if let Some(secret) = credential.filter(|secret| !secret.is_empty())
        && let Some(error) = &mut out.error
    {
        *error = error.replace(secret, "[redacted]");
    }
    out.elapsed_ms = start.elapsed().as_millis() as u64;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    fn outcome() -> ChatOutcome {
        ChatOutcome {
            request: json!({"model":"local-test"}),
            request_id: None,
            model: Some("local-test".into()),
            content: String::new(),
            reasoning: String::new(),
            tool_calls: vec![],
            usage: None,
            finish_reason: None,
            elapsed_ms: 0,
            first_output_ms: None,
            status: ChatStatus::Failed,
            error: None,
            server_cancel_confirmed: false,
        }
    }
    fn decode_chunks(bytes: &[u8], width: usize) -> Result<ChatOutcome> {
        let mut decoder = SseDecoder::default();
        let mut out = outcome();
        let mut assembly = Assembly {
            calls: BTreeMap::new(),
            bytes: 0,
            done: false,
            finished: false,
        };
        for chunk in bytes.chunks(width) {
            for frame in decoder.push(chunk)? {
                assembly.frame(frame, &mut out)?;
            }
        }
        decoder.finish()?;
        assembly.complete(&mut out)?;
        Ok(out)
    }
    #[test]
    fn every_chunk_boundary_preserves_unicode_tools_and_usage_tail() {
        let fixture = include_bytes!("../../../fixtures/chat/answer-tools.sse");
        for width in 1..fixture.len() {
            let out = decode_chunks(fixture, width).unwrap();
            assert_eq!(out.content, "Hello 🌍");
            assert_eq!(out.reasoning, "考える");
            assert_eq!(
                out.tool_calls,
                vec![ToolCall {
                    id: "call-1".into(),
                    name: "os_read".into(),
                    arguments: json!({"path":"file.txt"})
                }]
            );
            assert_eq!(out.usage.unwrap()["completion_tokens"], 8);
        }
    }
    #[test]
    fn multiline_comments_and_line_endings() {
        let fixture = include_str!("../../../fixtures/chat/multiline.sse");
        for text in [
            fixture.to_owned(),
            fixture.replace('\n', "\r\n"),
            fixture.replace('\n', "\r"),
        ] {
            assert_eq!(decode_chunks(text.as_bytes(), 1).unwrap().content, "hello");
        }
    }
    #[test]
    fn malformed_partial_and_early_disconnect_fail() {
        for bytes in [
            b"data: nope\n\n".as_slice(),
            b"data: [DONE]\n\n",
            b"data: {\"choices\":[]}\n\n",
            b"data: {",
            include_bytes!("../../../fixtures/chat/partial.sse"),
        ] {
            assert!(decode_chunks(bytes, 1).is_err());
        }
    }
    #[test]
    fn errors_and_output_bounds() {
        assert!(
            decode_chunks(
                b"event: error\ndata: {\"error\":{\"message\":\"failed\"}}\n\n",
                1
            )
            .is_err()
        );
        assert!(
            SseDecoder::default()
                .push(&vec![b'x'; MAX_FRAME_BYTES + 1])
                .is_err()
        );
        assert!(SseDecoder::default().push(b"data: \xff\n\n").is_err());
    }
    #[test]
    fn endpoint_rejects_remote_credentials_redirect_base_and_dns() {
        for endpoint in [
            "http://example.com",
            "http://localhost",
            "http://127.0.0.1.evil.test",
            "http://192.168.1.1",
            "http://user:secret@127.0.0.1",
            "http://127.0.0.1/path",
            "file:///tmp",
            "http://127.0.0.1?token=x",
        ] {
            assert!(validate_endpoint(endpoint).is_err(), "{endpoint}");
        }
        for endpoint in [
            "http://127.0.0.1:8000",
            "http://[::1]:8000/v1/",
            "http://127.20.3.4",
        ] {
            assert!(validate_endpoint(endpoint).is_ok(), "{endpoint}");
        }
    }
    #[test]
    fn effort_requires_evidence_and_preserves_normalization() {
        assert!(effort_mapping("1.0.2", &EffortProfile::Unknown, "high").is_none());
        assert!(effort_mapping("future", &EffortProfile::ThinkingBudget, "high").is_none());
        assert_eq!(
            effort_mapping("1.0.2", &EffortProfile::ThinkingBudget, "high")
                .unwrap()
                .normalization,
            json!({"reasoning_max_tokens":8192})
        );
        let native =
            EffortProfile::NativeLevels(vec!["xhigh".into(), "medium".into(), "low".into()]);
        assert_eq!(
            effort_mapping("1.0.2", &native, "high")
                .unwrap()
                .normalization,
            json!({"chat_template_kwargs":{"reasoning_effort":"xhigh"}})
        );
    }
    fn capabilities() -> Capabilities {
        Capabilities {
            runtime_version: Some("1.0.2".into()),
            schema_version: Some("0.6.0".into()),
            model_ids: vec!["local-test".into()],
            schema: serde_json::from_str(include_str!("../../../fixtures/chat/openapi.json"))
                .unwrap(),
            models: json!({"data":[{"id":"local-test"}]}),
            cancellation: true,
        }
    }
    #[test]
    fn validates_real_schema_constraints_and_unknown_controls() {
        let caps = capabilities();
        let mut request =
            json!({"model":"local-test","messages":[{"role":"user","content":"hi"}],"top_p":1.0});
        assert!(validate_request(&request, &caps, &EffortProfile::Unknown, false).is_ok());
        request["top_p"] = json!(0);
        assert!(validate_request(&request, &caps, &EffortProfile::Unknown, false).is_err());
        request["top_p"] = json!(1);
        request["reasoning_effort"] = json!("high");
        assert!(validate_request(&request, &caps, &EffortProfile::Unknown, false).is_err());
        assert!(validate_request(&request, &caps, &EffortProfile::ThinkingBudget, false).is_ok());
        request["model"] = json!("different");
        assert!(validate_request(&request, &caps, &EffortProfile::ThinkingBudget, false).is_err());
    }
    #[tokio::test]
    async fn negotiation_does_not_confuse_api_and_runtime_versions() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            for _ in 0..6 {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut buffer = [0; 8192];
                let size = socket.read(&mut buffer).await.unwrap();
                let head = String::from_utf8_lossy(&buffer[..size]);
                assert!(
                    head.to_ascii_lowercase()
                        .contains("authorization: bearer ephemeral-test")
                );
                let path = head.split_whitespace().nth(1).unwrap();
                let body = match path {
                    "/openapi.json" => include_str!("../../../fixtures/chat/openapi.json"),
                    "/health" => "{\"status\":\"ok\"}",
                    "/v1/models" => "{\"data\":[{\"id\":\"local-test\"}]}",
                    _ => panic!("unexpected route"),
                };
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                socket.write_all(response.as_bytes()).await.unwrap();
            }
        });
        let unknown = negotiate(&endpoint, Some("ephemeral-test")).await.unwrap();
        assert_eq!(unknown.schema_version.as_deref(), Some("0.6.0"));
        assert!(unknown.runtime_version.is_none());
        assert!(!unknown.cancellation);
        let known = negotiate_for_runtime(&endpoint, Some("ephemeral-test"), Some("1.0.2"))
            .await
            .unwrap();
        assert!(known.cancellation);
        assert_eq!(known.model_ids, vec!["local-test"]);
        task.await.unwrap();
    }
    #[tokio::test]
    async fn cancellation_uses_public_id_and_checks_confirmation() {
        for confirmed in [true, false] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let endpoint = format!("http://{}", listener.local_addr().unwrap());
            let task = tokio::spawn(async move {
                let (mut streaming, _) = listener.accept().await.unwrap();
                let mut buffer = [0; 8192];
                let _ = streaming.read(&mut buffer).await;
                streaming.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\ndata: {\"id\":\"chatcmpl-cancel-test\",\"choices\":[{\"delta\":{\"content\":\"partial\"},\"finish_reason\":null}]}\n\n").await.unwrap();
                let (mut cancelling, _) = listener.accept().await.unwrap();
                let size = cancelling.read(&mut buffer).await.unwrap();
                assert!(
                    String::from_utf8_lossy(&buffer[..size])
                        .starts_with("POST /v1/requests/chatcmpl-cancel-test/cancel ")
                );
                let body = if confirmed {
                    include_str!("../../../fixtures/chat/cancel-supported.json")
                } else {
                    include_str!("../../../fixtures/chat/cancel-unavailable.json")
                };
                let status = if confirmed { "200 OK" } else { "404 Not Found" };
                cancelling.write_all(format!("HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await.unwrap();
            });
            let (tx, mut rx) = mpsc::channel(4);
            let cancel = CancellationToken::new();
            let token = cancel.clone();
            let watcher = tokio::spawn(async move {
                while let Some(event) = rx.recv().await {
                    if matches!(event, ChatEvent::Content(_)) {
                        token.cancel();
                        break;
                    }
                }
            });
            let out = tokio::time::timeout(
                Duration::from_secs(3),
                stream_chat_with_capabilities(
                    &endpoint,
                    None,
                    json!({"model":"local-test"}),
                    tx,
                    cancel,
                    Some(&capabilities()),
                ),
            )
            .await
            .unwrap()
            .unwrap();
            assert_eq!(out.status, ChatStatus::Cancelled);
            assert_eq!(out.content, "partial");
            assert_eq!(out.server_cancel_confirmed, confirmed);
            task.await.unwrap();
            watcher.await.unwrap();
        }
    }
    async fn serve(body: Vec<u8>, stall: bool) -> (String, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buffer = [0; 8192];
            let _ = socket.read(&mut buffer).await;
            socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n").await.unwrap();
            for byte in body {
                if socket.write_all(&[byte]).await.is_err() {
                    return;
                }
            }
            if stall {
                tokio::time::sleep(Duration::from_secs(10)).await;
            }
        });
        (endpoint, task)
    }
    #[tokio::test]
    async fn http_stream_keeps_partial_outcome_on_disconnect() {
        let (endpoint, task) = serve(
            b"data: {\"choices\":[{\"delta\":{\"content\":\"saved\"},\"finish_reason\":null}]}\n\n"
                .to_vec(),
            false,
        )
        .await;
        let (tx, mut rx) = mpsc::channel(64);
        let out = stream_chat(
            &endpoint,
            None,
            json!({"model":"local-test"}),
            tx,
            CancellationToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(out.status, ChatStatus::Failed);
        assert_eq!(out.content, "saved");
        assert!(out.tool_calls.is_empty());
        assert!(rx.recv().await.is_some());
        task.await.unwrap();
    }
    #[tokio::test]
    async fn rejected_http_preserves_reason_and_redacts_credential() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buffer = [0; 8192];
            let _ = socket.read(&mut buffer).await;
            let body = r#"{"error":"credential ephemeral-secret was rejected"}"#;
            socket.write_all(format!("HTTP/1.1 401 Unauthorized\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
        });
        let (tx, _rx) = mpsc::channel(4);
        let out = stream_chat(
            &endpoint,
            Some("ephemeral-secret"),
            json!({"model":"local-test"}),
            tx,
            CancellationToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(out.status, ChatStatus::Failed);
        let error = out.error.unwrap();
        assert!(error.contains("401"));
        assert!(error.contains("rejected"));
        assert!(!error.contains("ephemeral-secret"));
        task.await.unwrap();
    }
    #[tokio::test]
    async fn malformed_later_frame_preserves_received_content() {
        let mut body = b"data: {\"choices\":[{\"delta\":{\"content\":\"keep me\"},\"finish_reason\":null}]}\n\n".to_vec();
        body.extend_from_slice(b"data: \xff\n\n");
        let (endpoint, task) = serve(body, false).await;
        let (tx, _rx) = mpsc::channel(8);
        let out = stream_chat(
            &endpoint,
            None,
            json!({"model":"local-test"}),
            tx,
            CancellationToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(out.status, ChatStatus::Failed);
        assert_eq!(out.content, "keep me");
        task.await.unwrap();
    }
    #[tokio::test]
    async fn cancellation_interrupts_backpressured_delivery() {
        let (endpoint, task) = serve(
            include_bytes!("../../../fixtures/chat/answer-tools.sse").to_vec(),
            true,
        )
        .await;
        let (tx, _rx) = mpsc::channel(1);
        let cancel = CancellationToken::new();
        let token = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(30)).await;
            token.cancel();
        });
        let out = tokio::time::timeout(
            Duration::from_secs(1),
            stream_chat(&endpoint, None, json!({"model":"local-test"}), tx, cancel),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(out.status, ChatStatus::Cancelled);
        assert!(!out.server_cancel_confirmed);
        task.abort();
    }
}
