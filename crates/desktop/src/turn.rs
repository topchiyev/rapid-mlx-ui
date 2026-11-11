//! One durable chat/tool turn. The actor owns storage, global inference exclusion and UI state.
use anyhow::{Context, Result, anyhow, ensure};
use rapid_mlx_core::{
    chat, domain,
    mcp::{McpErrorKind, McpManager, McpTool},
    tools,
};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet, HashSet},
    sync::Arc,
    time::Duration,
};
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

pub struct TurnRequest {
    pub session: domain::Session,
    pub attempt: String,
    pub endpoint: String,
    pub credential: Option<String>,
    pub capabilities: chat::Capabilities,
    pub effort_profile: chat::EffortProfile,
    pub tools_verified: bool,
    pub local_policy: Option<tools::ToolPolicy>,
    pub mcp: Arc<McpManager>,
    /// Model-visible MCP tool IDs, rather than registration IDs.
    pub selected_mcp: BTreeSet<String>,
}

pub enum TurnEvent {
    ModelStart {
        session: String,
        attempt: String,
        request: domain::ModelRequest,
        ack: oneshot::Sender<Result<(), String>>,
    },
    Delta {
        session: String,
        attempt: String,
        request_id: String,
        event: chat::ChatEvent,
    },
    ModelEnd {
        session: String,
        attempt: String,
        request_id: String,
        outcome: chat::ChatOutcome,
        ack: oneshot::Sender<Result<(), String>>,
    },
    ToolStart {
        session: String,
        attempt: String,
        record: domain::ToolRecord,
        ack: oneshot::Sender<Result<(), String>>,
    },
    ToolEnd {
        session: String,
        attempt: String,
        record: domain::ToolRecord,
        ack: oneshot::Sender<Result<(), String>>,
    },
    Finished {
        session: String,
        attempt: String,
        status: domain::MessageStatus,
        error: Option<String>,
    },
}

/// Await each persistence acknowledgement before advancing. Never retry a model or tool step.
/// ModelRequest.request is {wire_request, effort}; only wire_request is sent to HTTP.
pub async fn run(req: TurnRequest, tx: mpsc::Sender<TurnEvent>, cancel: CancellationToken) {
    let token = cancel.child_token();
    let result = run_inner(&req, &tx, &token).await;
    let (status, error) = match result {
        Ok(status) => (status, None),
        Err(error) => (domain::MessageStatus::Failed, Some(error.to_string())),
    };
    token.cancel();
    let _ = tx
        .send(TurnEvent::Finished {
            session: req.session.id.clone(),
            attempt: req.attempt.clone(),
            status,
            error,
        })
        .await;
}

async fn committed(
    tx: &mpsc::Sender<TurnEvent>,
    event: TurnEvent,
    ack: oneshot::Receiver<Result<(), String>>,
) -> Result<()> {
    tx.send(event)
        .await
        .map_err(|_| anyhow!("Turn event receiver closed before persistence"))?;
    tokio::time::timeout(Duration::from_secs(30), ack)
        .await
        .context("Persistence acknowledgement timed out; dispatch stopped")?
        .context("Persistence acknowledgement was dropped; dispatch stopped")?
        .map_err(|message| anyhow!("Persistence failed; dispatch stopped: {message}"))
}

fn history_message(message: domain::HistoryMessage) -> Result<Value> {
    let mut wire = json!({"role": message.role.as_str(), "content": message.content});
    if !message.tool_calls.is_empty() {
        wire["tool_calls"] = Value::Array(message.tool_calls.iter().map(|call| {
            let name = call.policy.get("model_visible_name").and_then(Value::as_str).unwrap_or(&call.name);
            Ok(json!({"id":call.id,"type":"function","function":{"name":name,"arguments":serde_json::to_string(&call.arguments)?}}))
        }).collect::<Result<_>>()?);
    }
    if let Some(id) = message.tool_call_id {
        wire["tool_call_id"] = json!(id);
    }
    Ok(wire)
}

async fn run_inner(
    req: &TurnRequest,
    tx: &mpsc::Sender<TurnEvent>,
    cancel: &CancellationToken,
) -> Result<domain::MessageStatus> {
    chat::validate_endpoint(&req.endpoint)?;
    let attempt = req.session.message(&req.attempt)?;
    ensure!(
        attempt.role == domain::Role::Assistant && attempt.status == domain::MessageStatus::Pending,
        "Turn requires a committed pending assistant attempt"
    );
    ensure!(
        attempt.tools.is_empty()
            && attempt
                .model_requests
                .iter()
                .all(|r| r.status == domain::MessageStatus::Pending && r.request.is_null()),
        "Previously dispatched work cannot be resumed or replayed automatically"
    );
    ensure!(
        attempt.model_requests.len() <= 1,
        "Pending attempt has multiple placeholder model requests"
    );
    let settings = attempt.settings.clone();
    settings.validate()?;
    let mut messages = Vec::new();
    if !settings.system_prompt.is_empty() {
        messages.push(json!({"role":"system","content":settings.system_prompt}));
    }
    for message in req.session.request_history(&req.attempt)? {
        messages.push(history_message(message)?);
    }
    let executor = req
        .local_policy
        .as_ref()
        .filter(|policy| req.tools_verified && !policy.enabled.is_empty())
        .cloned()
        .map(tools::ToolExecutor::new)
        .transpose()?;
    let mcp_tools = if req.tools_verified {
        req.mcp.selected_tools(&req.selected_mcp).await
    } else {
        vec![]
    };
    let mcp_by_id: BTreeMap<_, _> = mcp_tools
        .into_iter()
        .map(|tool| (tool.id.clone(), tool))
        .collect();
    let mut definitions = if req.tools_verified {
        executor
            .as_ref()
            .map_or_else(Vec::new, tools::ToolExecutor::definitions)
    } else {
        vec![]
    };
    definitions.extend(mcp_by_id.values().map(McpTool::openai_definition));
    let advertised: BTreeSet<String> = definitions
        .iter()
        .filter_map(|d| {
            d.pointer("/function/name")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .collect();
    ensure!(
        advertised.len() == definitions.len(),
        "Tool provider names collide; no request sent"
    );
    let mut seen_calls = HashSet::new();
    for cycle in 0..chat::MAX_TOOL_CYCLES {
        if cancel.is_cancelled() {
            return Ok(domain::MessageStatus::Cancelled);
        }
        let mut wire = json!({"model":attempt.model,"messages":messages,"temperature":settings.temperature,"top_p":settings.top_p,"max_tokens":settings.max_tokens});
        if !definitions.is_empty() {
            wire["tools"] = json!(definitions);
        }
        let effort = if let Some(requested) = &settings.effort {
            let mapping = chat::effort_mapping(
                req.capabilities.runtime_version.as_deref().unwrap_or(""),
                &req.effort_profile,
                requested,
            )
            .context("Selected reasoning effort is not verified for this model")?;
            wire["reasoning_effort"] = json!(requested);
            serde_json::to_value(mapping)?
        } else {
            Value::Null
        };
        chat::validate_request(
            &wire,
            &req.capabilities,
            &req.effort_profile,
            req.tools_verified,
        )?;
        let wire = chat::prepare_request(wire)?;
        let mut request = domain::ModelRequest::new(&attempt.model, settings.clone());
        // begin_turn creates one pending placeholder: reuse its UUID rather than count
        // an unsent placeholder as another unknown-usage model request.
        if cycle == 0
            && let Some(initial) = attempt.model_requests.first()
        {
            request.id.clone_from(&initial.id);
        }
        request.request = json!({"wire_request":wire,"effort":effort});
        let request_id = request.id.clone();
        let (ack_tx, ack_rx) = oneshot::channel();
        committed(
            tx,
            TurnEvent::ModelStart {
                session: req.session.id.clone(),
                attempt: req.attempt.clone(),
                request,
                ack: ack_tx,
            },
            ack_rx,
        )
        .await?;
        // A stop while waiting for the intent ack creates a cancelled model outcome,
        // with no HTTP dispatch, so the committed step still gets terminal metadata.
        let outcome = stream_step(req, tx, &request_id, wire, cancel).await?;
        let calls = outcome.tool_calls.clone();
        let status = outcome.status.clone();
        let error = outcome.error.clone();
        let content = outcome.content.clone();
        let (ack_tx, ack_rx) = oneshot::channel();
        committed(
            tx,
            TurnEvent::ModelEnd {
                session: req.session.id.clone(),
                attempt: req.attempt.clone(),
                request_id: request_id.clone(),
                outcome,
                ack: ack_tx,
            },
            ack_rx,
        )
        .await?;
        if status == chat::ChatStatus::Cancelled || cancel.is_cancelled() {
            return Ok(domain::MessageStatus::Cancelled);
        }
        if status == chat::ChatStatus::Failed {
            return Err(anyhow!(
                error.unwrap_or_else(|| "Model request failed".into())
            ));
        }
        if calls.is_empty() {
            return Ok(domain::MessageStatus::Completed);
        }
        // Reject duplicate IDs across model steps before executing any call in this step.
        for call in &calls {
            ensure!(
                seen_calls.insert(call.id.clone()),
                "Model reused a tool call ID; no repeated execution allowed"
            );
        }
        messages.push(json!({"role":"assistant","content":content,"tool_calls":calls.iter().map(|call| json!({"id":call.id,"type":"function","function":{"name":call.name,"arguments":call.arguments.to_string()}})).collect::<Vec<_>>()}));
        for call in calls {
            if cancel.is_cancelled() {
                return Ok(domain::MessageStatus::Cancelled);
            }
            let remote = mcp_by_id.get(&call.name);
            let enabled = req.tools_verified && advertised.contains(&call.name);
            let provider = remote.map_or_else(
                || {
                    if advertised.contains(&call.name) {
                        "os"
                    } else {
                        "unavailable"
                    }
                },
                |tool| tool.server_id.as_str(),
            );
            let mut record =
                domain::ToolRecord::new(&call.id, provider, &call.name, call.arguments.clone());
            record.model_request_id = Some(request_id.clone());
            record.policy = json!({"model_visible_name":call.name,"original_name":remote.map_or(call.name.as_str(), |tool| tool.name.as_str()),"selected":enabled,"local_policy":if remote.is_none() { req.local_policy.as_ref().map(serde_json::to_value).transpose()? } else { None }});
            let (ack_tx, ack_rx) = oneshot::channel();
            committed(
                tx,
                TurnEvent::ToolStart {
                    session: req.session.id.clone(),
                    attempt: req.attempt.clone(),
                    record: record.clone(),
                    ack: ack_tx,
                },
                ack_rx,
            )
            .await?;
            let mut uncertain = false;
            if cancel.is_cancelled() {
                record.status = domain::ToolStatus::Cancelled;
                record.result = json!({"call_id":call.id,"error":"Cancelled before dispatch","outcome_unknown":false});
            } else if !enabled {
                record.status = domain::ToolStatus::Denied;
                record.result = json!({"call_id":call.id,"error":"Tool is disabled, not selected, or model tool support is unverified","denied":true});
            } else if remote.is_some() {
                match req
                    .mcp
                    .call_selected_tool(
                        &req.selected_mcp,
                        &call.name,
                        call.arguments,
                        cancel.clone(),
                    )
                    .await
                {
                    Ok(result) => {
                        record.result = serde_json::to_value(result)?;
                        record.status = if record
                            .result
                            .get("isError")
                            .and_then(Value::as_bool)
                            .unwrap_or(false)
                        {
                            domain::ToolStatus::Failed
                        } else {
                            domain::ToolStatus::Completed
                        };
                    }
                    Err(error) => {
                        uncertain = error.outcome_unknown;
                        record.status = if uncertain {
                            domain::ToolStatus::OutcomeUnknown
                        } else if error.kind == McpErrorKind::Cancelled {
                            domain::ToolStatus::Cancelled
                        } else if matches!(
                            error.kind,
                            McpErrorKind::NotSelected
                                | McpErrorKind::Disabled
                                | McpErrorKind::InvalidArguments
                        ) {
                            domain::ToolStatus::Denied
                        } else {
                            domain::ToolStatus::Failed
                        };
                        record.result = json!({"call_id":call.id,"error":error.message,"kind":error.kind,"outcome_unknown":uncertain});
                    }
                }
            } else if let Some(executor) = &executor {
                let result = executor.execute(call.clone(), cancel.clone()).await;
                record.status = match result.status {
                    tools::ToolStatus::Completed => domain::ToolStatus::Completed,
                    tools::ToolStatus::Denied => domain::ToolStatus::Denied,
                    tools::ToolStatus::Cancelled => domain::ToolStatus::Cancelled,
                    tools::ToolStatus::Failed | tools::ToolStatus::TimedOut => {
                        domain::ToolStatus::Failed
                    }
                };
                uncertain = result.side_effects_possible
                    && matches!(
                        result.status,
                        tools::ToolStatus::Cancelled
                            | tools::ToolStatus::Failed
                            | tools::ToolStatus::TimedOut
                    );
                if uncertain {
                    record.status = domain::ToolStatus::OutcomeUnknown;
                }
                record.result = serde_json::to_value(result)?;
            } else {
                unreachable!("Only advertised local tools are enabled");
            }
            let result = record.result.clone();
            let (ack_tx, ack_rx) = oneshot::channel();
            committed(
                tx,
                TurnEvent::ToolEnd {
                    session: req.session.id.clone(),
                    attempt: req.attempt.clone(),
                    record,
                    ack: ack_tx,
                },
                ack_rx,
            )
            .await?;
            if cancel.is_cancelled() {
                return Ok(domain::MessageStatus::Cancelled);
            }
            if uncertain {
                return Err(anyhow!(
                    "Tool outcome is unknown; no continuation or automatic retry was sent"
                ));
            }
            messages
                .push(json!({"role":"tool","tool_call_id":call.id,"content":result.to_string()}));
        }
    }
    Err(anyhow!(
        "Turn reached the eight-cycle model/tool limit; completed results were preserved"
    ))
}

async fn stream_step(
    req: &TurnRequest,
    tx: &mpsc::Sender<TurnEvent>,
    request_id: &str,
    wire: Value,
    cancel: &CancellationToken,
) -> Result<chat::ChatOutcome> {
    let (events_tx, mut events_rx) = mpsc::channel(32);
    let stream = chat::stream_chat_with_capabilities(
        &req.endpoint,
        req.credential.as_deref(),
        wire,
        events_tx,
        cancel.clone(),
        Some(&req.capabilities),
    );
    tokio::pin!(stream);
    let mut channel_open = true;
    loop {
        tokio::select! {
            // Drain emitted deltas before ModelEnd so the actor sees a lossless ordered stream.
            biased;
            event = events_rx.recv(), if channel_open => {
                if let Some(event) = event {
                    if cancel.is_cancelled() { continue; }
                    let event = TurnEvent::Delta { session: req.session.id.clone(), attempt: req.attempt.clone(), request_id: request_id.to_owned(), event };
                    tokio::select! { biased;
                        _ = cancel.cancelled() => {},
                        sent = tx.send(event) => { if sent.is_err() { cancel.cancel(); } }
                    }
                } else { channel_open = false; }
            }
            outcome = &mut stream => {
                let outcome = outcome?;
                while let Ok(event) = events_rx.try_recv() {
                    if !cancel.is_cancelled() {
                        tokio::select! { biased;
                            _ = cancel.cancelled() => {},
                            result = tx.send(TurnEvent::Delta { session: req.session.id.clone(), attempt: req.attempt.clone(), request_id: request_id.to_owned(), event }) => { result.map_err(|_| anyhow!("Turn event receiver closed"))?; }
                        }
                    }
                }
                return Ok(outcome);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    struct Directory(PathBuf);
    impl Directory {
        fn new() -> Self {
            let path =
                std::env::temp_dir().join(format!("rapid-ui-turn-test-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for Directory {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    fn request(endpoint: String, root: &Directory) -> TurnRequest {
        let mut session = domain::Session::new("fixture", "local-test");
        session.settings.system_prompt = "exact system snapshot".into();
        let attempt = session.begin_turn("read the fixture").unwrap();
        // Changing future settings must not affect this attempt.
        session.settings.system_prompt = "future system prompt".into();
        let local_policy = tools::ToolPolicy {
            roots: vec![root.0.clone()],
            working_dir: root.0.clone(),
            enabled: ["os_read".to_owned(), "os_create".to_owned()]
                .into_iter()
                .collect(),
            allow_mutations: true,
            ..tools::ToolPolicy::default()
        };
        TurnRequest {
            session,
            attempt,
            endpoint,
            credential: None,
            capabilities: chat::Capabilities {
                runtime_version: Some("1.0.2".into()),
                schema_version: Some("0.6.0".into()),
                model_ids: vec!["local-test".into()],
                schema: serde_json::from_str(include_str!("../../../fixtures/chat/openapi.json"))
                    .unwrap(),
                models: json!({"data":[{"id":"local-test"}]}),
                cancellation: false,
            },
            effort_profile: chat::EffortProfile::Unknown,
            tools_verified: true,
            local_policy: Some(local_policy),
            mcp: Arc::new(McpManager::new()),
            selected_mcp: BTreeSet::new(),
        }
    }
    fn tools_response(name: &str, args: &str) -> String {
        let chunk = json!({"id":"chatcmpl-turn-fixture","model":"local-test","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call-fixture","type":"function","function":{"name":name,"arguments":args}}]},"finish_reason":"tool_calls"}]});
        format!(
            "data: {chunk}\n\ndata: {{\"choices\":[],\"usage\":{{\"prompt_tokens\":2,\"completion_tokens\":3}}}}\n\ndata: [DONE]\n\n"
        )
    }
    fn answer_response() -> String {
        format!(
            "data: {}\n\ndata: [DONE]\n\n",
            json!({"id":"chatcmpl-answer-fixture","model":"local-test","choices":[{"index":0,"delta":{"content":"done"},"finish_reason":"stop"}]})
        )
    }
    async fn read_request(socket: &mut tokio::net::TcpStream) -> Value {
        let mut bytes = vec![];
        let mut buffer = [0; 8192];
        loop {
            let size = socket.read(&mut buffer).await.unwrap();
            assert!(size > 0);
            bytes.extend_from_slice(&buffer[..size]);
            if let Some(end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                let head = String::from_utf8_lossy(&bytes[..end]);
                let length = head
                    .lines()
                    .find_map(|l| {
                        l.to_ascii_lowercase()
                            .strip_prefix("content-length:")
                            .map(|n| n.trim().parse::<usize>().unwrap())
                    })
                    .unwrap();
                if bytes.len() >= end + 4 + length {
                    return serde_json::from_slice(&bytes[end + 4..end + 4 + length]).unwrap();
                }
            }
        }
    }
    async fn server(
        responses: Vec<String>,
    ) -> (String, mpsc::Receiver<Value>, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let (tx, rx) = mpsc::channel(16);
        let task = tokio::spawn(async move {
            for body in responses {
                let (mut socket, _) = listener.accept().await.unwrap();
                tx.send(read_request(&mut socket).await).await.unwrap();
                socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
            }
        });
        (endpoint, rx, task)
    }
    #[derive(Default)]
    struct Audit {
        starts: Vec<domain::ModelRequest>,
        ends: Vec<chat::ChatOutcome>,
        tool_starts: Vec<domain::ToolRecord>,
        tool_ends: Vec<domain::ToolRecord>,
        status: Option<domain::MessageStatus>,
        error: Option<String>,
    }
    async fn actor(
        mut rx: mpsc::Receiver<TurnEvent>,
        session: String,
        attempt: String,
        reject: Option<&str>,
    ) -> Audit {
        let mut audit = Audit::default();
        while let Some(event) = rx.recv().await {
            let (actual_session, actual_attempt) = match &event {
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
            assert_eq!(actual_session, &session);
            assert_eq!(actual_attempt, &attempt);
            let ack = |name| {
                if reject == Some(name) {
                    Err("fixture disk full".into())
                } else {
                    Ok(())
                }
            };
            match event {
                TurnEvent::ModelStart {
                    request,
                    ack: reply,
                    ..
                } => {
                    audit.starts.push(request);
                    let _ = reply.send(ack("model_start"));
                }
                TurnEvent::ModelEnd {
                    request_id,
                    outcome,
                    ack: reply,
                    ..
                } => {
                    assert_eq!(request_id, audit.starts.last().unwrap().id);
                    audit.ends.push(outcome);
                    let _ = reply.send(ack("model_end"));
                }
                TurnEvent::ToolStart {
                    record, ack: reply, ..
                } => {
                    assert!(
                        audit
                            .ends
                            .last()
                            .is_some_and(|o| o.status == chat::ChatStatus::Completed)
                    );
                    audit.tool_starts.push(record);
                    let _ = reply.send(ack("tool_start"));
                }
                TurnEvent::ToolEnd {
                    record, ack: reply, ..
                } => {
                    assert_eq!(audit.tool_starts.last().unwrap().id, record.id);
                    audit.tool_ends.push(record);
                    let _ = reply.send(ack("tool_end"));
                }
                TurnEvent::Delta {
                    request_id, event, ..
                } => {
                    assert_eq!(request_id, audit.starts.last().unwrap().id);
                    match event {
                        chat::ChatEvent::Content(text) | chat::ChatEvent::Reasoning(text) => {
                            assert!(!text.is_empty())
                        }
                        _ => {}
                    }
                }
                TurnEvent::Finished { status, error, .. } => {
                    audit.status = Some(status);
                    audit.error = error;
                    break;
                }
            }
        }
        audit
    }
    async fn execute(req: TurnRequest, reject: Option<&'static str>) -> Audit {
        let (tx, rx) = mpsc::channel(4);
        let session = req.session.id.clone();
        let attempt = req.attempt.clone();
        let worker = tokio::spawn(actor(rx, session, attempt, reject));
        tokio::time::timeout(
            Duration::from_secs(5),
            run(req, tx, CancellationToken::new()),
        )
        .await
        .unwrap();
        worker.await.unwrap()
    }
    #[tokio::test]
    async fn complete_tool_roundtrip_preserves_history_settings_and_ids() {
        let root = Directory::new();
        std::fs::write(root.0.join("fixture.txt"), "fixture text").unwrap();
        let (endpoint, mut requests, server) = server(vec![
            tools_response("os_read", r#"{"path":"fixture.txt"}"#),
            answer_response(),
        ])
        .await;
        let mut req = request(endpoint, &root);
        req.effort_profile =
            chat::EffortProfile::NativeLevels(vec!["low".into(), "medium".into(), "xhigh".into()]);
        let committed_attempt = req.session.message_mut(&req.attempt).unwrap();
        committed_attempt.settings.effort = Some("high".into());
        committed_attempt.model_requests[0].settings = committed_attempt.settings.clone();
        let initial_id = req.session.message(&req.attempt).unwrap().model_requests[0]
            .id
            .clone();
        let audit = execute(req, None).await;
        assert_eq!(audit.status, Some(domain::MessageStatus::Completed));
        assert_eq!(audit.starts.len(), 2);
        assert_eq!(audit.tool_ends[0].status, domain::ToolStatus::Completed);
        assert_eq!(audit.starts[0].id, initial_id);
        assert_ne!(audit.starts[0].id, audit.starts[1].id);
        assert_eq!(
            audit.tool_ends[0].model_request_id.as_deref(),
            Some(initial_id.as_str())
        );
        let first = requests.recv().await.unwrap();
        let second = requests.recv().await.unwrap();
        assert_eq!(first["messages"][0]["content"], "exact system snapshot");
        assert_eq!(first["messages"][1]["content"], "read the fixture");
        assert_eq!(audit.starts[0].request["wire_request"], first);
        assert!(first.get("effort").is_none());
        assert_eq!(first["reasoning_effort"], "high");
        assert!(first.get("chat_template_kwargs").is_none());
        assert_eq!(audit.starts[0].request["effort"]["requested"], "high");
        assert_eq!(
            audit.starts[0].request["effort"]["normalization"],
            json!({"chat_template_kwargs":{"reasoning_effort":"xhigh"}})
        );
        assert_eq!(second["messages"][2]["tool_calls"][0]["id"], "call-fixture");
        assert_eq!(
            second["messages"][2]["tool_calls"][0]["function"]["name"],
            "os_read"
        );
        assert_eq!(second["messages"][3]["tool_call_id"], "call-fixture");
        assert!(
            second["messages"][3]["content"]
                .as_str()
                .unwrap()
                .contains("fixture text")
        );
        server.await.unwrap();
    }
    #[tokio::test]
    async fn disabled_tool_returns_matched_denial_without_write() {
        let root = Directory::new();
        let (endpoint, mut requests, server) = server(vec![
            tools_response("os_write", r#"{"path":"never.txt","content":"no"}"#),
            answer_response(),
        ])
        .await;
        let audit = execute(request(endpoint, &root), None).await;
        assert_eq!(audit.tool_ends[0].status, domain::ToolStatus::Denied);
        assert!(!root.0.join("never.txt").exists());
        let _ = requests.recv().await;
        let second = requests.recv().await.unwrap();
        assert_eq!(second["messages"][3]["tool_call_id"], "call-fixture");
        server.await.unwrap();
    }
    #[tokio::test]
    async fn partial_tool_arguments_never_emit_tool_intent() {
        let root = Directory::new();
        let (endpoint, mut requests, server) = server(vec![tools_response(
            "os_create",
            r#"{"path":"never.txt","content":"#,
        )])
        .await;
        let audit = execute(request(endpoint, &root), None).await;
        assert_eq!(audit.status, Some(domain::MessageStatus::Failed));
        assert!(audit.tool_starts.is_empty());
        assert!(!root.0.join("never.txt").exists());
        assert!(requests.recv().await.is_some());
        server.await.unwrap();
    }
    #[tokio::test]
    async fn cancellation_while_tool_intent_commits_prevents_side_effects() {
        let root = Directory::new();
        let (endpoint, mut requests, server) = server(vec![tools_response(
            "os_create",
            r#"{"path":"never.txt","content":"no"}"#,
        )])
        .await;
        let req = request(endpoint, &root);
        let cancel = CancellationToken::new();
        let token = cancel.clone();
        let (tx, mut rx) = mpsc::channel(4);
        let actor = tokio::spawn(async move {
            let mut end = None;
            while let Some(event) = rx.recv().await {
                match event {
                    TurnEvent::ModelStart { ack, .. } | TurnEvent::ModelEnd { ack, .. } => {
                        ack.send(Ok(())).unwrap();
                    }
                    TurnEvent::ToolStart { ack, .. } => {
                        token.cancel();
                        ack.send(Ok(())).unwrap();
                    }
                    TurnEvent::ToolEnd { record, ack, .. } => {
                        end = Some(record);
                        ack.send(Ok(())).unwrap();
                    }
                    TurnEvent::Finished { status, .. } => {
                        assert_eq!(status, domain::MessageStatus::Cancelled);
                        return end.unwrap();
                    }
                    _ => {}
                }
            }
            panic!("missing terminal event")
        });
        tokio::time::timeout(Duration::from_secs(3), run(req, tx, cancel))
            .await
            .unwrap();
        let record = actor.await.unwrap();
        assert_eq!(record.status, domain::ToolStatus::Cancelled);
        assert!(!root.0.join("never.txt").exists());
        assert!(requests.recv().await.is_some());
        server.await.unwrap();
    }
    #[cfg(unix)]
    #[tokio::test]
    async fn uncertain_mutating_command_stops_without_continuation_or_retry() {
        let root = Directory::new();
        let (endpoint, mut requests, server) = server(vec![tools_response(
            "os_command",
            r#"{"executable":"/bin/sh","args":["-c","exit 7"],"cwd":"."}"#,
        )])
        .await;
        let mut req = request(endpoint, &root);
        let policy = req.local_policy.as_mut().unwrap();
        policy.enabled.insert("os_command".into());
        policy.commands.push(tools::CommandGrant {
            executable: "/bin/sh".into(),
            args_prefix: vec!["-c".into(), "exit 7".into()],
            allow_extra_args: false,
            mutates: true,
        });
        let audit = execute(req, None).await;
        assert_eq!(audit.status, Some(domain::MessageStatus::Failed));
        assert_eq!(audit.starts.len(), 1);
        assert_eq!(
            audit.tool_ends[0].status,
            domain::ToolStatus::OutcomeUnknown
        );
        assert!(audit.error.as_deref().unwrap().contains("unknown"));
        assert!(requests.recv().await.is_some());
        server.await.unwrap();
    }
    #[tokio::test]
    async fn cycle_limit_preserves_eight_results_without_ninth_request() {
        let root = Directory::new();
        std::fs::write(root.0.join("fixture.txt"), "fixture").unwrap();
        let replies = (0..chat::MAX_TOOL_CYCLES)
            .map(|step| {
                tools_response("os_read", r#"{"path":"fixture.txt"}"#)
                    .replace("call-fixture", &format!("call-{step}"))
            })
            .collect();
        let (endpoint, mut requests, server) = server(replies).await;
        let audit = execute(request(endpoint, &root), None).await;
        assert_eq!(audit.status, Some(domain::MessageStatus::Failed));
        assert!(audit.error.as_deref().unwrap().contains("eight-cycle"));
        assert_eq!(audit.starts.len(), 8);
        assert_eq!(audit.tool_ends.len(), 8);
        for _ in 0..8 {
            assert!(requests.recv().await.is_some());
        }
        server.await.unwrap();
        assert!(requests.recv().await.is_none());
    }
    #[tokio::test]
    async fn persisted_original_mcp_names_replay_the_model_visible_alias() {
        let root = Directory::new();
        let (endpoint, mut requests, server) = server(vec![answer_response()]).await;
        let mut req = request(endpoint, &root);
        let prior = req.attempt.clone();
        let previous = req.session.message_mut(&prior).unwrap();
        previous.status = domain::MessageStatus::Completed;
        previous.content = "previous answer".into();
        previous.reasoning = "never replay this reasoning".into();
        previous.model_requests[0].status = domain::MessageStatus::Completed;
        let mut record = domain::ToolRecord::new(
            "prior-call",
            "server-one",
            "original_read",
            json!({"path":"a"}),
        );
        record.status = domain::ToolStatus::Completed;
        record.model_request_id = Some(previous.model_requests[0].id.clone());
        record.policy =
            json!({"model_visible_name":"mcp_visible_alias","original_name":"original_read"});
        record.result = json!({"content":"prior result"});
        previous.tools.push(record);
        req.attempt = req.session.begin_turn("new question").unwrap();
        let audit = execute(req, None).await;
        assert_eq!(audit.status, Some(domain::MessageStatus::Completed));
        let wire = requests.recv().await.unwrap();
        let messages = wire["messages"].as_array().unwrap();
        let call = messages
            .iter()
            .find(|m| m.get("tool_calls").is_some())
            .unwrap();
        assert_eq!(
            call["tool_calls"][0]["function"]["name"],
            "mcp_visible_alias"
        );
        assert_eq!(call["tool_calls"][0]["id"], "prior-call");
        assert!(!wire.to_string().contains("never replay this reasoning"));
        server.await.unwrap();
    }
    #[tokio::test]
    async fn failed_model_intent_ack_prevents_any_http_dispatch() {
        let root = Directory::new();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let audit = execute(request(endpoint, &root), Some("model_start")).await;
        assert_eq!(audit.status, Some(domain::MessageStatus::Failed));
        assert!(audit.ends.is_empty());
        assert!(
            tokio::time::timeout(Duration::from_millis(30), listener.accept())
                .await
                .is_err()
        );
    }
    #[tokio::test]
    async fn failed_outcome_or_tool_ack_blocks_next_dispatch() {
        for reject in ["model_end", "tool_start", "tool_end"] {
            let root = Directory::new();
            let (endpoint, mut requests, server) = server(vec![tools_response(
                "os_create",
                r#"{"path":"created.txt","content":"saved"}"#,
            )])
            .await;
            let audit = execute(request(endpoint, &root), Some(reject)).await;
            assert_eq!(audit.status, Some(domain::MessageStatus::Failed));
            assert_eq!(audit.starts.len(), 1);
            assert_eq!(root.0.join("created.txt").exists(), reject == "tool_end");
            assert!(
                audit
                    .error
                    .as_deref()
                    .unwrap()
                    .contains("Persistence failed")
            );
            assert!(requests.recv().await.is_some());
            server.await.unwrap();
        }
    }
}
