//! Real stdio MCP + synthetic SSE + application actor/SQLite integration.
//! Explicitly build the SDK-native example before running the ignored test.
use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

struct IsolatedHome(PathBuf);
impl IsolatedHome {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("rapid-mcp-turn-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn paths(&self) -> AppPaths {
        AppPaths::from_home(&self.0).unwrap()
    }
    fn worker(&self) -> Worker {
        Worker::new(self.paths()).unwrap()
    }
}
impl Drop for IsolatedHome {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn fixture_executable() -> PathBuf {
    std::env::var_os("RAPID_MCP_FIXTURE_BIN")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            std::env::current_exe()
                .unwrap()
                .parent()
                .unwrap()
                .parent()
                .unwrap()
                .join("examples")
                .join("mcp_fixture")
        })
        .canonicalize()
        .expect("build mcp_fixture first or set RAPID_MCP_FIXTURE_BIN to its executable")
}
fn verified_capabilities() -> chat::Capabilities {
    chat::Capabilities {
        runtime_version: Some("1.0.2".into()),
        schema_version: Some("synthetic".into()),
        model_ids: vec!["actor-fixture".into()],
        schema: serde_json::from_str(include_str!("../../../fixtures/chat/openapi.json")).unwrap(),
        models: json!({"data":[{"id":"actor-fixture","capabilities":["tools"]}]}),
        cancellation: false,
    }
}
fn call_response(name: &str, arguments: Value, call_id: &str) -> String {
    format!(
        "data: {}\n\ndata: [DONE]\n\n",
        json!({
            "id":format!("response-{call_id}"),"model":"actor-fixture",
            "choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":call_id,"type":"function","function":{"name":name,"arguments":arguments.to_string()}}]},"finish_reason":"tool_calls"}],
            "usage":{"prompt_tokens":2,"completion_tokens":3,"total_tokens":5}
        })
    )
}
fn answer_response() -> String {
    format!(
        "data: {}\n\ndata: [DONE]\n\n",
        json!({
            "id":"response-answer","model":"actor-fixture",
            "choices":[{"index":0,"delta":{"content":"Echo confirmed by the fixture."},"finish_reason":"stop"}],
            "usage":{"prompt_tokens":7,"completion_tokens":4,"total_tokens":11}
        })
    )
}
async fn read_request(socket: &mut tokio::net::TcpStream) -> Value {
    let mut bytes = Vec::new();
    let mut buffer = [0; 8192];
    loop {
        let count = socket.read(&mut buffer).await.unwrap();
        assert!(count > 0, "synthetic HTTP request ended early");
        bytes.extend_from_slice(&buffer[..count]);
        assert!(
            bytes.len() <= 1024 * 1024,
            "synthetic request unexpectedly large"
        );
        if let Some(end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
            let headers = String::from_utf8_lossy(&bytes[..end]);
            assert!(headers.starts_with("POST /v1/chat/completions "));
            let length: usize = headers
                .lines()
                .find_map(|line| {
                    line.to_ascii_lowercase()
                        .strip_prefix("content-length:")
                        .map(|n| n.trim().parse().unwrap())
                })
                .unwrap();
            if bytes.len() >= end + 4 + length {
                return serde_json::from_slice(&bytes[end + 4..end + 4 + length]).unwrap();
            }
        }
    }
}
struct SyntheticInference {
    endpoint: String,
    count: Arc<AtomicUsize>,
    requests: Arc<StdMutex<Vec<Value>>>,
    task: tokio::task::JoinHandle<()>,
}
impl SyntheticInference {
    async fn start(responses: Vec<String>) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let count = Arc::new(AtomicUsize::new(0));
        let requests = Arc::new(StdMutex::new(Vec::new()));
        let observed = count.clone();
        let captured = requests.clone();
        let task = tokio::spawn(async move {
            loop {
                let (mut socket, _) = listener.accept().await.unwrap();
                let request = read_request(&mut socket).await;
                captured.lock().unwrap().push(request);
                let index = observed.fetch_add(1, Ordering::SeqCst);
                let body = responses
                    .get(index)
                    .cloned()
                    .unwrap_or_else(answer_response);
                // Keep listening beyond the scripted exchange so unexpected replay is observable.
                socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await.unwrap();
                socket.shutdown().await.unwrap();
            }
        });
        Self {
            endpoint,
            count,
            requests,
            task,
        }
    }
}
impl Drop for SyntheticInference {
    fn drop(&mut self) {
        self.task.abort();
    }
}
async fn marker(path: &Path) -> String {
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
    .expect("fixture lifecycle marker must arrive")
}
#[derive(Default)]
struct TurnAudit {
    models: Vec<String>,
    tool_starts: usize,
    tool_ends: usize,
}

/// Dispatch actual runner events through the actor, including every SQLite ack.
async fn drive_turn(
    worker: &mut Worker,
    session: &str,
    attempt: &str,
    http: &SyntheticInference,
    baseline: usize,
    cancel_wait: Option<&Path>,
) -> TurnAudit {
    let mut audit = TurnAudit::default();
    loop {
        let event = tokio::time::timeout(Duration::from_secs(5), worker.turn_rx.recv())
            .await
            .unwrap()
            .expect("turn runner must send a terminal event");
        let finished = matches!(event, TurnEvent::Finished { .. });
        let mut model = None;
        let mut tool = None;
        let mut end = None;
        match &event {
            TurnEvent::ModelStart {
                session: actual_session,
                attempt: actual_attempt,
                request,
                ..
            } => {
                assert_eq!(actual_session, session);
                assert_eq!(actual_attempt, attempt);
                // Holding this real ack prevents HTTP dispatch, including continuation.
                tokio::time::sleep(Duration::from_millis(30)).await;
                assert_eq!(
                    http.count.load(Ordering::SeqCst),
                    baseline + audit.models.len()
                );
                let before = worker.store.load_session(session).unwrap().unwrap();
                if audit.models.is_empty() {
                    assert!(
                        before.message(attempt).unwrap().model_requests[0]
                            .request
                            .is_null()
                    );
                }
                model = Some(request.clone());
            }
            TurnEvent::ToolStart { record, .. } => {
                let before = worker.store.load_session(session).unwrap().unwrap();
                assert!(
                    before
                        .message(attempt)
                        .unwrap()
                        .tools
                        .iter()
                        .all(|t| t.id != record.id)
                );
                tool = Some(record.clone());
                audit.tool_starts += 1;
                if let Some(root) = cancel_wait {
                    assert!(
                        !root.join("wait_started").exists(),
                        "MCP must not dispatch before tool-intent commit ack"
                    );
                }
            }
            TurnEvent::ToolEnd { record, .. } => {
                let before = worker.store.load_session(session).unwrap().unwrap();
                assert!(
                    before
                        .message(attempt)
                        .unwrap()
                        .tools
                        .iter()
                        .find(|t| t.id == record.id)
                        .unwrap()
                        .status
                        .is_active()
                );
                tokio::time::sleep(Duration::from_millis(20)).await;
                assert_eq!(
                    http.count.load(Ordering::SeqCst),
                    baseline + audit.models.len(),
                    "continuation must wait for tool-result commit ack"
                );
                end = Some(record.clone());
                audit.tool_ends += 1;
            }
            TurnEvent::Finished { error, .. } => {
                assert!(error.is_none(), "synthetic turn failed: {error:?}");
            }
            _ => {}
        }
        worker.turn_event(event);
        assert!(
            worker.error.is_none(),
            "actor failed persistence: {:?}",
            worker.error
        );
        if let Some(request) = model {
            let committed = worker.store.load_session(session).unwrap().unwrap();
            let step = committed
                .message(attempt)
                .unwrap()
                .model_requests
                .iter()
                .find(|r| r.id == request.id)
                .unwrap();
            assert_eq!(step.request, request.request);
            assert!(step.status.is_active());
            assert_eq!(
                committed.message(attempt).unwrap().status,
                MessageStatus::Streaming
            );
            audit.models.push(request.id);
        }
        if let Some(record) = tool {
            let committed = worker.store.load_session(session).unwrap().unwrap();
            assert_eq!(committed.message(attempt).unwrap().tools[0], record);
            if let Some(root) = cancel_wait {
                assert_eq!(marker(&root.join("wait_started")).await, "1");
                worker.command(Command::Cancel).await.unwrap();
            }
        }
        if let Some(record) = end {
            let committed = worker.store.load_session(session).unwrap().unwrap();
            assert_eq!(committed.message(attempt).unwrap().tools[0], record);
        }
        if finished {
            break;
        }
    }
    audit
}

#[cfg(unix)]
#[tokio::test]
#[ignore = "First cargo build -p rapid-mlx-core --example mcp_fixture; then run this test explicitly"]
async fn stdio_mcp_turn_commits_results_usage_and_cancelled_outcomes_without_replay() {
    let home = IsolatedHome::new();
    let mut worker = home.worker();
    let markers = worker.paths.root.join("mcp-fixture-markers");
    std::fs::create_dir(&markers).unwrap();
    let server_id = uuid::Uuid::new_v4().to_string();
    worker
        .command(Command::SaveMcp(McpForm {
            id: server_id.clone(),
            name: "Integrated Rust stdio fixture".into(),
            transport: "stdio".into(),
            target: fixture_executable().to_string_lossy().into_owned(),
            arguments: serde_json::to_string(&vec![
                "--lifecycle-dir".into(),
                markers.to_string_lossy().into_owned(),
            ])
            .unwrap(),
            environment: "{}".into(),
            enabled: true,
            ..Default::default()
        }))
        .await
        .unwrap();
    assert!(
        worker.mcp.connections().await.is_empty(),
        "registration persistence must not connect"
    );
    let registration = worker.registrations[0].clone();
    let connection = worker
        .mcp
        .connect(registration, mcp::McpCredentials::default())
        .await
        .unwrap();
    worker.connections = worker.mcp.connections().await;
    let echo = connection
        .tools
        .iter()
        .find(|t| t.name == "echo")
        .unwrap()
        .id
        .clone();
    let wait = connection
        .tools
        .iter()
        .find(|t| t.name == "wait")
        .unwrap()
        .id
        .clone();
    let http = SyntheticInference::start(vec![
        call_response(
            &echo,
            json!({"message":"through SQLite and Rust stdio ✓"}),
            "integrated-echo",
        ),
        answer_response(),
        call_response(&wait, json!({}), "integrated-wait"),
    ])
    .await;
    worker.preferences.chat_endpoint = http.endpoint.clone();
    worker
        .store
        .set_setting("preferences", &worker.preferences)
        .unwrap();
    worker.capabilities = Some(verified_capabilities());
    worker.command(Command::NewSession).await.unwrap();
    let session = worker.sessions[0].id.clone();
    worker
        .command(Command::SaveSession {
            id: session.clone(),
            draft: "fixture draft".into(),
            model: "actor-fixture".into(),
            generation: Generation::initial(),
        })
        .await
        .unwrap();
    for tool in [&echo, &wait] {
        worker
            .command(Command::SelectTool {
                session: session.clone(),
                server: server_id.clone(),
                tool: tool.clone(),
                enabled: true,
            })
            .await
            .unwrap();
    }
    worker
        .command(Command::Send {
            id: session.clone(),
            text: "Echo through the selected MCP provider.".into(),
        })
        .await
        .unwrap();
    let success_attempt = worker.sessions[0].active_head.clone().unwrap();
    let audit = drive_turn(&mut worker, &session, &success_attempt, &http, 0, None).await;
    assert_eq!(
        (audit.models.len(), audit.tool_starts, audit.tool_ends),
        (2, 1, 1)
    );
    assert_ne!(audit.models[0], audit.models[1]);
    assert!(worker.active.is_none());
    let completed = worker.store.load_session(&session).unwrap().unwrap();
    let response = completed.message(&success_attempt).unwrap();
    assert_eq!(response.status, MessageStatus::Completed);
    assert_eq!(response.model, "actor-fixture");
    assert_eq!(response.content, "Echo confirmed by the fixture.");
    assert_eq!(response.model_requests.len(), 2);
    let record = &response.tools[0];
    assert_eq!(record.id, "integrated-echo");
    assert_eq!(record.provider, server_id);
    assert_eq!(record.name, echo);
    assert_eq!(record.status, domain::ToolStatus::Completed);
    assert_eq!(
        record.model_request_id.as_deref(),
        Some(audit.models[0].as_str())
    );
    assert_eq!(record.policy["original_name"], "echo");
    assert_eq!(record.policy["model_visible_name"], echo);
    assert_eq!(
        record.result["content"][0]["text"],
        "through SQLite and Rust stdio ✓"
    );
    assert_eq!(completed.selected_mcp_servers, vec![server_id.clone()]);
    let totals = completed.usage_totals();
    assert_eq!(
        (
            totals.requests,
            totals.prompt_tokens,
            totals.completion_tokens,
            totals.total_tokens,
            totals.unknown_requests
        ),
        (2, 9, 7, 16, 0)
    );
    assert!(
        completed
            .eligible_history()
            .unwrap()
            .iter()
            .any(|m| m.tool_call_id.as_deref() == Some("integrated-echo"))
    );
    {
        let wire = http.requests.lock().unwrap();
        assert_eq!(wire.len(), 2);
        assert_eq!(wire[0]["model"], "actor-fixture");
        assert!(
            wire[0]["tools"]
                .as_array()
                .unwrap()
                .iter()
                .any(|t| t["function"]["name"] == echo)
        );
        let messages = wire[1]["messages"].as_array().unwrap();
        assert!(
            messages
                .iter()
                .any(|m| m["tool_calls"][0]["function"]["name"] == echo)
        );
        let result = messages
            .iter()
            .find(|m| m["tool_call_id"] == "integrated-echo")
            .unwrap();
        assert!(
            result["content"]
                .as_str()
                .unwrap()
                .contains("through SQLite and Rust stdio")
        );
    }
    // A second real model turn dispatches wait; cancellation must durably preserve uncertainty.
    worker
        .command(Command::Send {
            id: session.clone(),
            text: "Start the cancellable wait fixture.".into(),
        })
        .await
        .unwrap();
    let cancelled_attempt = worker.sessions[0].active_head.clone().unwrap();
    let audit = drive_turn(
        &mut worker,
        &session,
        &cancelled_attempt,
        &http,
        2,
        Some(&markers),
    )
    .await;
    assert_eq!(
        (audit.models.len(), audit.tool_starts, audit.tool_ends),
        (1, 1, 1)
    );
    assert_eq!(
        marker(&markers.join("cancellation_received")).await,
        "received"
    );
    let saved = worker.store.load_session(&session).unwrap().unwrap();
    let cancelled = saved.message(&cancelled_attempt).unwrap();
    assert_eq!(cancelled.status, MessageStatus::Cancelled);
    assert_eq!(cancelled.tools.len(), 1);
    assert_eq!(cancelled.tools[0].provider, server_id);
    assert_eq!(
        cancelled.tools[0].status,
        domain::ToolStatus::OutcomeUnknown
    );
    assert_eq!(cancelled.tools[0].result["outcome_unknown"], true);
    assert_eq!(saved.message(&success_attempt).unwrap(), response);
    assert_eq!(
        (
            saved.usage_totals().requests,
            saved.usage_totals().total_tokens
        ),
        (3, 21)
    );
    assert!(
        saved.eligible_history().is_err(),
        "cancelled output must not silently become eligible history"
    );
    assert_eq!(
        http.count.load(Ordering::SeqCst),
        3,
        "unknown MCP result must not trigger continuation"
    );
    assert_eq!(marker(&markers.join("wait_started")).await, "1");
    worker.mcp.disconnect(&server_id).await.unwrap();
    let pid = marker(&markers.join("pid")).await;
    assert_eq!(marker(&markers.join("graceful_exit")).await, pid);
    worker.shutdown().await;
    drop(worker);
    let mut restored = home.worker();
    assert_eq!(restored.registrations.len(), 1);
    assert_eq!(restored.registrations[0].id, server_id);
    assert!(restored.connections.is_empty());
    assert!(restored.mcp.connections().await.is_empty());
    assert!(restored.active.is_none());
    assert!(restored.task_handles.is_empty());
    assert!(restored.turn_rx.try_recv().is_err());
    assert_eq!(restored.preferences.chat_endpoint, http.endpoint);
    let reopened = restored.store.load_session(&session).unwrap().unwrap();
    assert_eq!(reopened, saved);
    assert_eq!(reopened.selected_mcp_servers, vec![server_id]);
    assert!(
        reopened.metadata["mcp_tools"]
            .as_array()
            .unwrap()
            .contains(&json!(echo))
    );
    assert!(
        reopened.metadata["mcp_tools"]
            .as_array()
            .unwrap()
            .contains(&json!(wait))
    );
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(
        http.count.load(Ordering::SeqCst),
        3,
        "restart must not replay inference or MCP work"
    );
    assert_eq!(
        marker(&markers.join("pid")).await,
        pid,
        "restart must not launch another fixture process"
    );
    assert_eq!(marker(&markers.join("wait_started")).await, "1");
    restored.shutdown().await;
}

#[cfg(unix)]
#[tokio::test]
#[ignore = "First cargo build -p rapid-mlx-core --example mcp_fixture; then run this test explicitly"]
async fn shutdown_drains_active_stdio_tool_outcome_before_disconnect_and_restart() {
    let home = IsolatedHome::new();
    let mut worker = home.worker();
    let markers = worker.paths.root.join("shutdown-mcp-markers");
    std::fs::create_dir(&markers).unwrap();
    let server_id = uuid::Uuid::new_v4().to_string();
    worker
        .command(Command::SaveMcp(McpForm {
            id: server_id.clone(),
            name: "Shutdown fixture".into(),
            transport: "stdio".into(),
            target: fixture_executable().to_string_lossy().into_owned(),
            arguments: serde_json::to_string(&vec![
                "--lifecycle-dir".into(),
                markers.to_string_lossy().into_owned(),
            ])
            .unwrap(),
            environment: "{}".into(),
            enabled: true,
            ..Default::default()
        }))
        .await
        .unwrap();
    let connection = worker
        .mcp
        .connect(
            worker.registrations[0].clone(),
            mcp::McpCredentials::default(),
        )
        .await
        .unwrap();
    let wait = connection
        .tools
        .iter()
        .find(|tool| tool.name == "wait")
        .unwrap()
        .id
        .clone();
    let http =
        SyntheticInference::start(vec![call_response(&wait, json!({}), "shutdown-wait")]).await;
    worker.preferences.chat_endpoint = http.endpoint.clone();
    worker
        .store
        .set_setting("preferences", &worker.preferences)
        .unwrap();
    worker.capabilities = Some(verified_capabilities());
    worker.command(Command::NewSession).await.unwrap();
    let session = worker.sessions[0].id.clone();
    worker
        .command(Command::SaveSession {
            id: session.clone(),
            draft: String::new(),
            model: "actor-fixture".into(),
            generation: Generation::initial(),
        })
        .await
        .unwrap();
    worker
        .command(Command::SelectTool {
            session: session.clone(),
            server: server_id.clone(),
            tool: wait,
            enabled: true,
        })
        .await
        .unwrap();
    worker
        .command(Command::Send {
            id: session.clone(),
            text: "Start wait; shutdown will cancel it.".into(),
        })
        .await
        .unwrap();
    let attempt = worker.sessions[0].active_head.clone().unwrap();
    loop {
        let event = tokio::time::timeout(Duration::from_secs(5), worker.turn_rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(
            !matches!(event, TurnEvent::Finished { .. }),
            "turn ended before entering the real MCP tool"
        );
        let tool_started = matches!(event, TurnEvent::ToolStart { .. });
        worker.turn_event(event);
        assert!(worker.error.is_none());
        if tool_started {
            break;
        }
    }
    assert_eq!(marker(&markers.join("wait_started")).await, "1");
    let pid = marker(&markers.join("pid")).await;
    let active = worker.store.load_session(&session).unwrap().unwrap();
    assert!(
        active.message(&attempt).unwrap().tools[0]
            .status
            .is_active()
    );
    assert!(worker.active.is_some());
    // Do not pump events here: shutdown itself must cancel, drain/ack outcomes,
    // and only then stop the owned stdio process.
    tokio::time::timeout(Duration::from_secs(20), worker.shutdown())
        .await
        .unwrap();
    assert!(worker.error.is_none());
    assert!(worker.active.is_none());
    assert!(worker.task_handles.is_empty());
    assert!(worker.turn_rx.try_recv().is_err());
    let saved = worker.store.load_session(&session).unwrap().unwrap();
    let cancelled = saved.message(&attempt).unwrap();
    assert_eq!(
        cancelled.status,
        MessageStatus::Cancelled,
        "cooperative completion should not fall back to interrupted recovery"
    );
    assert_eq!(cancelled.model_requests.len(), 1);
    assert_eq!(cancelled.tools[0].id, "shutdown-wait");
    assert_eq!(cancelled.tools[0].provider, server_id);
    assert_eq!(
        cancelled.tools[0].status,
        domain::ToolStatus::OutcomeUnknown
    );
    // A populated terminal result proves ToolEnd was committed through its ack;
    // recover_interrupted alone would leave the original empty result.
    assert_eq!(cancelled.tools[0].result["outcome_unknown"], true);
    assert_eq!(cancelled.tools[0].result["call_id"], "shutdown-wait");
    assert_eq!(
        marker(&markers.join("cancellation_received")).await,
        "received"
    );
    assert_eq!(marker(&markers.join("graceful_exit")).await, pid);
    let alive = tokio::process::Command::new("/bin/kill")
        .args(["-0", &pid])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .await
        .unwrap()
        .success();
    assert!(!alive, "shutdown must reap its owned MCP fixture process");
    assert_eq!(http.count.load(Ordering::SeqCst), 1);
    drop(worker);
    let mut restored = home.worker();
    assert_eq!(
        restored.store.load_session(&session).unwrap().unwrap(),
        saved
    );
    assert!(restored.mcp.connections().await.is_empty());
    assert!(restored.task_handles.is_empty());
    assert!(restored.active.is_none());
    assert!(restored.turn_rx.try_recv().is_err());
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(
        http.count.load(Ordering::SeqCst),
        1,
        "shutdown recovery must not replay model/tool work"
    );
    assert_eq!(marker(&markers.join("wait_started")).await, "1");
    assert_eq!(marker(&markers.join("pid")).await, pid);
    restored.shutdown().await;
}
