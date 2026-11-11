//! Actor/SQLite integration using injected homes and synthetic turn events only.
use super::*;
use std::fs;
use tokio::sync::oneshot;

struct Home(PathBuf);
impl Home {
    fn new() -> Self {
        let path =
            std::env::temp_dir().join(format!("rapid-ui-actor-test-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn paths(&self) -> AppPaths {
        AppPaths::from_home(&self.0).unwrap()
    }
    fn worker(&self) -> Worker {
        Worker::new(self.paths()).unwrap()
    }
}
impl Drop for Home {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn capabilities() -> chat::Capabilities {
    chat::Capabilities {
        runtime_version: Some("1.0.2".into()),
        schema_version: Some("synthetic".into()),
        model_ids: vec!["actor-fixture".into()],
        schema: serde_json::from_str(include_str!("../../../fixtures/chat/openapi.json")).unwrap(),
        models: json!({"data":[{"id":"actor-fixture"}]}),
        cancellation: false,
    }
}
async fn create_session(worker: &mut Worker) -> String {
    worker.command(Command::NewSession).await.unwrap();
    let id = worker.sessions[0].id.clone();
    worker
        .command(Command::SaveSession {
            id: id.clone(),
            draft: "retained draft".into(),
            model: "actor-fixture".into(),
            generation: Generation::initial(),
        })
        .await
        .unwrap();
    id
}
/// Start through the actual command path; stop the synthetic turn at its first commit
/// barrier, before HTTP. Subsequent actor events have test-owned ack receivers.
async fn pending(worker: &mut Worker) -> (String, String, domain::ModelRequest) {
    worker.capabilities = Some(capabilities());
    let id = create_session(worker).await;
    worker
        .command(Command::Send {
            id: id.clone(),
            text: "synthetic question".into(),
        })
        .await
        .unwrap();
    let event = tokio::time::timeout(Duration::from_secs(3), worker.turn_rx.recv())
        .await
        .unwrap()
        .unwrap();
    let (attempt, request) = match event {
        TurnEvent::ModelStart {
            attempt, request, ..
        } => (attempt, request),
        _ => panic!("first event must commit request intent"),
    };
    for task in worker.task_handles.drain(..) {
        task.abort();
        let _ = task.await;
    }
    (id, attempt, request)
}
async fn ack(
    worker: &mut Worker,
    event: impl FnOnce(oneshot::Sender<std::result::Result<(), String>>) -> TurnEvent,
) -> std::result::Result<(), String> {
    let (sender, receiver) = oneshot::channel();
    worker.turn_event(event(sender));
    tokio::time::timeout(Duration::from_secs(2), receiver)
        .await
        .expect("actor must answer the persistence barrier")
        .expect("actor must not drop the ack")
}
async fn model_start(
    worker: &mut Worker,
    session: &str,
    attempt: &str,
    request: domain::ModelRequest,
) -> std::result::Result<(), String> {
    ack(worker, |ack| TurnEvent::ModelStart {
        session: session.into(),
        attempt: attempt.into(),
        request,
        ack,
    })
    .await
}
fn outcome(usage: Option<Value>) -> chat::ChatOutcome {
    chat::ChatOutcome {
        request: json!({"model":"actor-fixture"}),
        request_id: Some("server-fixture".into()),
        model: Some("actor-fixture".into()),
        content: "answer".into(),
        reasoning: "reasoning".into(),
        tool_calls: vec![],
        usage,
        finish_reason: Some("stop".into()),
        elapsed_ms: 2000,
        first_output_ms: Some(25),
        status: chat::ChatStatus::Completed,
        error: None,
        server_cancel_confirmed: false,
    }
}
fn delta(
    worker: &mut Worker,
    session: &str,
    attempt: &str,
    request_id: &str,
    event: chat::ChatEvent,
) {
    worker.turn_event(TurnEvent::Delta {
        session: session.into(),
        attempt: attempt.into(),
        request_id: request_id.into(),
        event,
    });
}

#[tokio::test]
async fn sessions_preferences_and_private_root_restore_from_another_cwd() {
    let home = Home::new();
    let mut worker = home.worker();
    let id = create_session(&mut worker).await;
    worker
        .command(Command::Rename {
            id: id.clone(),
            title: "persisted actor session".into(),
        })
        .await
        .unwrap();
    let mut preferences = Preferences::initial();
    preferences.chat_endpoint = "http://127.0.0.1:8123".into();
    preferences.asr_endpoint = "http://127.0.0.1:8124".into();
    preferences.asr_model = "missing-asr@revision".into();
    preferences.retain_audio = true;
    worker
        .command(Command::Preferences(preferences))
        .await
        .unwrap();
    worker
        .command(Command::Credential {
            role: "chat".into(),
            value: "memory-only-synthetic-secret".into(),
        })
        .await
        .unwrap();
    worker.shutdown().await;
    drop(worker);
    let other = home.0.join("different-working-directory");
    fs::create_dir(&other).unwrap();
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "backend::tests::restore_from_alternate_working_directory_child",
            "--ignored",
            "--nocapture",
        ])
        .current_dir(&other)
        .env("RAPID_UI_ACTOR_TEST_HOME", &home.0)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "child failed: {} {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!other.join(".rapid-mlx-ui").exists());
    let restored = home.worker();
    assert_eq!(restored.sessions[0].id, id);
    assert!(restored.credentials.is_empty());
    let bytes = fs::read(home.paths().database).unwrap();
    assert!(
        !bytes
            .windows(b"memory-only-synthetic-secret".len())
            .any(|s| s == b"memory-only-synthetic-secret")
    );
}
#[tokio::test]
#[ignore = "invoked by the isolated-home parent test with a different child cwd"]
async fn restore_from_alternate_working_directory_child() {
    let home = PathBuf::from(
        std::env::var_os("RAPID_UI_ACTOR_TEST_HOME").expect("parent-injected home required"),
    );
    let worker = Worker::new(AppPaths::from_home(&home).unwrap()).unwrap();
    assert_eq!(worker.paths.root, home.join(".rapid-mlx-ui"));
    assert_eq!(worker.sessions.len(), 1);
    assert_eq!(worker.sessions[0].title, "persisted actor session");
    assert_eq!(worker.sessions[0].draft, "retained draft");
    assert_eq!(worker.sessions[0].model, "actor-fixture");
    assert_eq!(worker.preferences.chat_endpoint, "http://127.0.0.1:8123");
    assert_eq!(worker.preferences.asr_model, "missing-asr@revision");
    assert!(worker.preferences.retain_audio);
    assert!(worker.active.is_none());
    assert!(worker.credentials.is_empty());
}

#[tokio::test]
async fn pending_turn_and_model_request_are_durable_before_http_dispatch() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let home = Home::new();
    let mut worker = home.worker();
    worker.capabilities = Some(capabilities());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    worker.preferences.chat_endpoint = format!("http://{}", listener.local_addr().unwrap());
    let id = create_session(&mut worker).await;
    worker
        .command(Command::Send {
            id: id.clone(),
            text: "barrier prompt".into(),
        })
        .await
        .unwrap();
    let event = tokio::time::timeout(Duration::from_secs(3), worker.turn_rx.recv())
        .await
        .unwrap()
        .unwrap();
    let (attempt, request_id) = match &event {
        TurnEvent::ModelStart {
            attempt, request, ..
        } => (attempt.clone(), request.id.clone()),
        _ => panic!("request must ask for commit before dispatch"),
    };
    let pending = worker.store.load_session(&id).unwrap().unwrap();
    assert_eq!(
        pending.message(&attempt).unwrap().status,
        MessageStatus::Pending
    );
    assert_eq!(pending.messages[0].content, "barrier prompt");
    assert_eq!(
        pending.message(&attempt).unwrap().model_requests[0].id,
        request_id
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(50), listener.accept())
            .await
            .is_err(),
        "HTTP must not dispatch before request persistence ack"
    );
    worker.turn_event(event);
    let durable = worker.store.load_session(&id).unwrap().unwrap();
    assert_eq!(
        durable.message(&attempt).unwrap().status,
        MessageStatus::Streaming
    );
    assert!(
        !durable.message(&attempt).unwrap().model_requests[0]
            .request
            .is_null()
    );
    let (mut socket, _) = tokio::time::timeout(Duration::from_secs(3), listener.accept())
        .await
        .unwrap()
        .unwrap();
    let mut received = vec![0; 8192];
    let length = socket.read(&mut received).await.unwrap();
    assert!(length > 0);
    let body = format!(
        "data: {}\n\ndata: [DONE]\n\n",
        json!({"id":"fixture","model":"actor-fixture","choices":[{"delta":{"content":"fixture answer"},"finish_reason":"stop"}],"usage":{"prompt_tokens":2,"completion_tokens":3,"total_tokens":5}})
    );
    socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
    drop(socket);
    loop {
        let event = tokio::time::timeout(Duration::from_secs(3), worker.turn_rx.recv())
            .await
            .unwrap()
            .unwrap();
        let done = matches!(event, TurnEvent::Finished { .. });
        worker.turn_event(event);
        if done {
            break;
        }
    }
    let saved = worker.store.load_session(&id).unwrap().unwrap();
    assert_eq!(
        saved.message(&attempt).unwrap().status,
        MessageStatus::Completed
    );
    assert_eq!(saved.message(&attempt).unwrap().content, "fixture answer");
    assert_eq!(saved.usage_totals().total_tokens, 5);
    assert!(worker.active.is_none());
    worker.shutdown().await;
}

#[tokio::test]
async fn model_and_tool_commit_acks_persist_intent_results_and_step_accounting() {
    let home = Home::new();
    let mut worker = home.worker();
    let (id, attempt, request) = pending(&mut worker).await;
    let request_id = request.id.clone();
    model_start(&mut worker, &id, &attempt, request)
        .await
        .unwrap();
    delta(
        &mut worker,
        &id,
        &attempt,
        &request_id,
        chat::ChatEvent::Usage(json!({"prompt_tokens":7,"completion_tokens":1,"total_tokens":8})),
    );
    delta(
        &mut worker,
        &id,
        &attempt,
        &request_id,
        chat::ChatEvent::Usage(json!({"prompt_tokens":7,"completion_tokens":3,"total_tokens":10})),
    );
    ack(&mut worker, |ack| TurnEvent::ModelEnd {
        session: id.clone(),
        attempt: attempt.clone(),
        request_id: request_id.clone(),
        outcome: outcome(Some(
            json!({"prompt_tokens":7,"completion_tokens":3,"total_tokens":10}),
        )),
        ack,
    })
    .await
    .unwrap();
    let saved = worker.store.load_session(&id).unwrap().unwrap();
    let step = &saved.message(&attempt).unwrap().model_requests[0];
    assert_eq!(step.status, MessageStatus::Completed);
    assert_eq!(step.usage.total_tokens, Some(10));
    assert_eq!(step.client_elapsed_ms, Some(2000));
    assert_eq!(step.client_first_output_ms, Some(25));
    let mut call =
        domain::ToolRecord::new("call-fixture", "os", "read", json!({"path":"synthetic"}));
    call.model_request_id = Some(request_id);
    call.status = domain::ToolStatus::Running;
    ack(&mut worker, |ack| TurnEvent::ToolStart {
        session: id.clone(),
        attempt: attempt.clone(),
        record: call.clone(),
        ack,
    })
    .await
    .unwrap();
    assert_eq!(
        worker
            .store
            .load_session(&id)
            .unwrap()
            .unwrap()
            .message(&attempt)
            .unwrap()
            .tools[0]
            .status,
        domain::ToolStatus::Running
    );
    call.status = domain::ToolStatus::Completed;
    call.result = json!({"text":"fixture result"});
    ack(&mut worker, |ack| TurnEvent::ToolEnd {
        session: id.clone(),
        attempt: attempt.clone(),
        record: call.clone(),
        ack,
    })
    .await
    .unwrap();
    assert_eq!(
        worker
            .store
            .load_session(&id)
            .unwrap()
            .unwrap()
            .message(&attempt)
            .unwrap()
            .tools[0],
        call
    );
    let mut continuation =
        domain::ModelRequest::new("actor-fixture", domain::GenerationSettings::default());
    continuation.status = MessageStatus::Streaming;
    continuation.request = json!({"model":"actor-fixture","continuation":true});
    let next_id = continuation.id.clone();
    model_start(&mut worker, &id, &attempt, continuation)
        .await
        .unwrap();
    ack(&mut worker, |ack| TurnEvent::ModelEnd {
        session: id.clone(),
        attempt: attempt.clone(),
        request_id: next_id,
        outcome: outcome(None),
        ack,
    })
    .await
    .unwrap();
    worker.turn_event(TurnEvent::Finished {
        session: id.clone(),
        attempt: attempt.clone(),
        status: MessageStatus::Completed,
        error: None,
    });
    let totals = worker
        .store
        .load_session(&id)
        .unwrap()
        .unwrap()
        .usage_totals();
    assert_eq!(totals.requests, 2);
    assert_eq!(totals.total_tokens, 10);
    assert_eq!(totals.unknown_requests, 1);
    assert!(worker.snapshot().sessions[0].usage.contains("partial"));
    worker.shutdown().await;
    drop(worker);
    let reopened = home.worker();
    assert_eq!(reopened.sessions[0].usage_totals(), totals);
    assert_eq!(
        reopened.sessions[0].message(&attempt).unwrap().tools[0],
        call
    );
}

#[tokio::test]
async fn global_active_blocks_second_chat_transcription_and_recording_before_io() {
    let home = Home::new();
    let mut worker = home.worker();
    let (first, _, _) = pending(&mut worker).await;
    let second = create_session(&mut worker).await;
    let error = worker
        .command(Command::Send {
            id: second.clone(),
            text: "blocked".into(),
        })
        .await
        .unwrap_err();
    assert!(error.to_string().contains("already active"));
    assert!(
        worker
            .command(Command::Transcribe(home.0.join("does-not-exist.wav")))
            .await
            .unwrap_err()
            .to_string()
            .contains("Finish active")
    );
    assert!(
        worker
            .command(Command::Record)
            .await
            .unwrap_err()
            .to_string()
            .contains("Finish active")
    );
    assert!(
        worker
            .store
            .load_session(&second)
            .unwrap()
            .unwrap()
            .messages
            .is_empty()
    );
    assert!(worker.jobs.is_empty());
    assert!(!worker.recording);
    assert!(worker.recorder.is_none());
    assert_eq!(
        worker.active.as_ref().unwrap().session.as_deref(),
        Some(first.as_str())
    );
    worker.shutdown().await;
}

#[tokio::test]
async fn cancelled_turn_discards_late_content_reasoning_and_usage() {
    let home = Home::new();
    let mut worker = home.worker();
    let (id, attempt, request) = pending(&mut worker).await;
    let request_id = request.id.clone();
    model_start(&mut worker, &id, &attempt, request)
        .await
        .unwrap();
    delta(
        &mut worker,
        &id,
        &attempt,
        &request_id,
        chat::ChatEvent::Content("retained".into()),
    );
    worker.checkpoint();
    worker.command(Command::Cancel).await.unwrap();
    for event in [
        chat::ChatEvent::Content("must not append".into()),
        chat::ChatEvent::Reasoning("late reasoning".into()),
        chat::ChatEvent::Usage(json!({"total_tokens":999})),
    ] {
        delta(&mut worker, &id, &attempt, &request_id, event);
    }
    worker.turn_event(TurnEvent::Finished {
        session: id.clone(),
        attempt: attempt.clone(),
        status: MessageStatus::Completed,
        error: None,
    });
    let saved = worker.store.load_session(&id).unwrap().unwrap();
    let message = saved.message(&attempt).unwrap();
    assert_eq!(message.content, "retained");
    assert!(message.reasoning.is_empty());
    assert_eq!(message.status, MessageStatus::Cancelled);
    assert_eq!(message.model_requests[0].usage.total_tokens, None);
    delta(
        &mut worker,
        &id,
        &attempt,
        &request_id,
        chat::ChatEvent::Content("after terminal".into()),
    );
    assert_eq!(worker.sessions[0].message(&attempt).unwrap(), message);
    worker.shutdown().await;
}

#[tokio::test]
async fn registration_and_actual_model_visible_tool_selection_survive_restart_without_connecting() {
    let home = Home::new();
    let mut worker = home.worker();
    let id = create_session(&mut worker).await;
    worker
        .command(Command::SaveMcp(McpForm {
            id: "d364234a-1806-4a20-a11f-f1dab6c96e62".into(),
            name: "fixture server".into(),
            transport: "http".into(),
            target: "https://example.invalid/mcp".into(),
            credential_env: "SYNTHETIC_TOKEN_REFERENCE".into(),
            enabled: true,
            ..Default::default()
        }))
        .await
        .unwrap();
    worker
        .command(Command::SelectTool {
            session: id.clone(),
            server: "d364234a-1806-4a20-a11f-f1dab6c96e62".into(),
            tool: "mcp_actual_model_visible_id".into(),
            enabled: true,
        })
        .await
        .unwrap();
    worker.shutdown().await;
    drop(worker);
    let reopened = home.worker();
    assert_eq!(reopened.registrations.len(), 1);
    assert!(reopened.connections.is_empty());
    assert_eq!(
        reopened.sessions[0].metadata["mcp_tools"],
        json!(["mcp_actual_model_visible_id"])
    );
    assert_eq!(
        reopened.snapshot().sessions[0].selected_tools,
        ["mcp_actual_model_visible_id"]
    );
    assert_eq!(
        reopened.sessions[0].selected_mcp_servers,
        ["d364234a-1806-4a20-a11f-f1dab6c96e62"]
    );
    assert!(reopened.task_handles.is_empty());
}

#[tokio::test]
async fn catalog_refresh_does_not_substitute_or_erase_missing_model_references() {
    let home = Home::new();
    let mut worker = home.worker();
    let (id, attempt, request) = pending(&mut worker).await;
    model_start(&mut worker, &id, &attempt, request)
        .await
        .unwrap();
    worker.turn_event(TurnEvent::Finished {
        session: id.clone(),
        attempt: attempt.clone(),
        status: MessageStatus::Failed,
        error: Some("fixture failure".into()),
    });
    worker.preferences.asr_model = "missing-asr@rev".into();
    worker
        .store
        .set_setting("preferences", &worker.preferences)
        .unwrap();
    worker
        .event(Event::Catalog {
            available: vec![],
            local: catalog::CatalogReport {
                models: vec![],
                errors: vec![],
            },
            error: None,
        })
        .await
        .unwrap();
    assert!(worker.models.is_empty());
    assert_eq!(worker.sessions[0].model, "actor-fixture");
    assert_eq!(
        worker.sessions[0].message(&attempt).unwrap().model,
        "actor-fixture"
    );
    assert_eq!(worker.snapshot().sessions[0].model, "actor-fixture");
    worker.shutdown().await;
    drop(worker);
    let reopened = home.worker();
    assert_eq!(reopened.sessions[0].model, "actor-fixture");
    assert_eq!(reopened.preferences.asr_model, "missing-asr@rev");
}

#[tokio::test]
async fn each_commit_barrier_returns_error_instead_of_false_ack_when_sqlite_cannot_journal() {
    for stage in ["model-start", "model-end", "tool-start", "tool-end"] {
        let home = Home::new();
        let mut worker = home.worker();
        let (id, attempt, request) = pending(&mut worker).await;
        let request_id = request.id.clone();
        let mut call = domain::ToolRecord::new(
            "fixture-call",
            "os",
            "write",
            json!({"path":"not-executed"}),
        );
        call.model_request_id = Some(request_id.clone());
        call.status = domain::ToolStatus::Running;
        if stage != "model-start" {
            model_start(&mut worker, &id, &attempt, request.clone())
                .await
                .unwrap();
        }
        if stage == "tool-end" {
            ack(&mut worker, |ack| TurnEvent::ToolStart {
                session: id.clone(),
                attempt: attempt.clone(),
                record: call.clone(),
                ack,
            })
            .await
            .unwrap();
        }
        let before = worker.store.load_session(&id).unwrap().unwrap();
        // A directory where SQLite must create its rollback journal deterministically
        // produces a real write failure without permission/root-user assumptions.
        let journal = worker.paths.root.join("state.sqlite3-journal");
        fs::create_dir(&journal).unwrap();
        let result = match stage {
            "model-start" => model_start(&mut worker, &id, &attempt, request).await,
            "model-end" => {
                ack(&mut worker, |ack| TurnEvent::ModelEnd {
                    session: id.clone(),
                    attempt: attempt.clone(),
                    request_id: request_id.clone(),
                    outcome: outcome(Some(json!({"total_tokens":8}))),
                    ack,
                })
                .await
            }
            "tool-start" => {
                ack(&mut worker, |ack| TurnEvent::ToolStart {
                    session: id.clone(),
                    attempt: attempt.clone(),
                    record: call.clone(),
                    ack,
                })
                .await
            }
            "tool-end" => {
                call.status = domain::ToolStatus::Completed;
                call.result = json!({"ok":true});
                ack(&mut worker, |ack| TurnEvent::ToolEnd {
                    session: id.clone(),
                    attempt: attempt.clone(),
                    record: call,
                    ack,
                })
                .await
            }
            _ => unreachable!(),
        };
        assert!(
            result.is_err(),
            "{stage} must not acknowledge an uncommitted write"
        );
        fs::remove_dir(&journal).unwrap();
        assert_eq!(
            worker.store.load_session(&id).unwrap().unwrap(),
            before,
            "{stage} changed durable state despite failure"
        );
        worker.shutdown().await;
    }
}

#[tokio::test]
async fn stale_finished_event_does_not_clear_a_newer_active_session() {
    let home = Home::new();
    let mut worker = home.worker();
    let (old_session, old_attempt, _) = pending(&mut worker).await;
    worker.turn_event(TurnEvent::Finished {
        session: old_session.clone(),
        attempt: old_attempt.clone(),
        status: MessageStatus::Failed,
        error: Some("first finished".into()),
    });
    let (new_session, _, _) = pending(&mut worker).await;
    worker.turn_event(TurnEvent::Finished {
        session: old_session,
        attempt: old_attempt,
        status: MessageStatus::Failed,
        error: Some("late duplicate".into()),
    });
    assert_eq!(
        worker.active.as_ref().and_then(|a| a.session.as_deref()),
        Some(new_session.as_str()),
        "late terminal event must not release the current global inference guard"
    );
    worker.shutdown().await;
}

#[tokio::test]
async fn model_end_without_usage_preserves_the_last_reported_cumulative_update() {
    let home = Home::new();
    let mut worker = home.worker();
    let (id, attempt, request) = pending(&mut worker).await;
    let request_id = request.id.clone();
    model_start(&mut worker, &id, &attempt, request)
        .await
        .unwrap();
    delta(
        &mut worker,
        &id,
        &attempt,
        &request_id,
        chat::ChatEvent::Usage(json!({"prompt_tokens":5,"completion_tokens":2,"total_tokens":7})),
    );
    ack(&mut worker, |ack| TurnEvent::ModelEnd {
        session: id.clone(),
        attempt: attempt.clone(),
        request_id: request_id.clone(),
        outcome: outcome(None),
        ack,
    })
    .await
    .unwrap();
    let saved = worker.store.load_session(&id).unwrap().unwrap();
    assert_eq!(
        saved.message(&attempt).unwrap().model_requests[0]
            .usage
            .total_tokens,
        Some(7),
        "an absent final usage payload must not erase a reported cumulative observation"
    );
    worker.shutdown().await;
}

#[tokio::test]
async fn pending_write_failure_preserves_draft_and_never_spawns_dispatch() {
    let home = Home::new();
    let mut worker = home.worker();
    worker.capabilities = Some(capabilities());
    let id = create_session(&mut worker).await;
    let before = worker.store.load_session(&id).unwrap().unwrap();
    let journal = worker.paths.root.join("state.sqlite3-journal");
    fs::create_dir(&journal).unwrap();
    let error = worker
        .command(Command::Send {
            id: id.clone(),
            text: "must not dispatch".into(),
        })
        .await;
    assert!(error.is_err());
    assert!(worker.active.is_none());
    assert!(worker.task_handles.is_empty());
    assert!(worker.turn_rx.try_recv().is_err());
    assert_eq!(worker.sessions[0], before);
    fs::remove_dir(journal).unwrap();
    assert_eq!(worker.store.load_session(&id).unwrap().unwrap(), before);
    worker.shutdown().await;
}

#[tokio::test]
async fn removing_registration_commits_session_selection_cleanup_before_restart() {
    let home = Home::new();
    let mut worker = home.worker();
    let id = create_session(&mut worker).await;
    let server = "d364234a-1806-4a20-a11f-f1dab6c96e62";
    worker
        .command(Command::SaveMcp(McpForm {
            id: server.into(),
            name: "removed fixture".into(),
            transport: "http".into(),
            target: "https://example.invalid/mcp".into(),
            enabled: true,
            ..Default::default()
        }))
        .await
        .unwrap();
    worker
        .command(Command::SelectTool {
            session: id.clone(),
            server: server.into(),
            tool: "mcp_actual_model_visible_id".into(),
            enabled: true,
        })
        .await
        .unwrap();
    worker
        .command(Command::Mcp {
            id: server.into(),
            action: "remove".into(),
        })
        .await
        .unwrap();
    assert!(worker.registrations.is_empty());
    assert!(worker.sessions[0].selected_mcp_servers.is_empty());
    assert!(
        worker
            .store
            .load_session(&id)
            .unwrap()
            .unwrap()
            .selected_mcp_servers
            .is_empty(),
        "removing a registration must persist its session selection cleanup before acknowledging the command"
    );
    worker.shutdown().await;
    drop(worker);
    let reopened = home.worker();
    assert!(reopened.registrations.is_empty());
    assert!(reopened.sessions[0].selected_mcp_servers.is_empty());
}

/// Keep preference-triggered scans confined to empty fixture folders. A missing explicit
/// executable prevents invoking an installed rapid-mlx catalog command during these tests.
fn isolated_preferences(home: &Home) -> Preferences {
    let cache = home.0.join("empty-model-cache");
    let models = home.0.join("empty-model-root");
    fs::create_dir_all(&cache).unwrap();
    fs::create_dir_all(&models).unwrap();
    Preferences {
        executable: home.0.join("missing-fixture-runtime").display().to_string(),
        cache: cache.display().to_string(),
        model_roots: vec![models],
        ..Preferences::initial()
    }
}
fn change_connection(preferences: &mut Preferences, field: &str, home: &Home) {
    match field {
        "chat endpoint" => preferences.chat_endpoint = "http://127.0.0.1:18123".into(),
        "ASR endpoint" => preferences.asr_endpoint = "http://127.0.0.1:18124".into(),
        "executable" => {
            preferences.executable = home.0.join("other-missing-runtime").display().to_string()
        }
        "cache" => {
            let path = home.0.join("other-empty-cache");
            fs::create_dir(&path).unwrap();
            preferences.cache = path.display().to_string();
        }
        "model roots" => {
            let path = home.0.join("other-empty-model-root");
            fs::create_dir(&path).unwrap();
            preferences.model_roots = vec![path];
        }
        _ => panic!("unknown test case"),
    }
}
fn seed_connection_state(worker: &mut Worker) {
    worker.capabilities = Some(capabilities());
    worker.effort_profile = chat::EffortProfile::ThinkingBudget;
    worker.chat_view.state = "Ready".into();
    worker.asr_view.state = "Ready".into();
    worker.models = vec![ModelView {
        id: "stale-served-model".into(),
        served: true,
        ..Default::default()
    }];
}

#[tokio::test]
async fn connection_preferences_invalidate_capabilities_effort_and_served_inventory_before_reuse() {
    for field in [
        "chat endpoint",
        "ASR endpoint",
        "executable",
        "cache",
        "model roots",
    ] {
        let home = Home::new();
        let mut worker = home.worker();
        worker.preferences = isolated_preferences(&home);
        worker
            .store
            .set_setting("preferences", &worker.preferences)
            .unwrap();
        let session = create_session(&mut worker).await;
        seed_connection_state(&mut worker);
        let mut changed = worker.preferences.clone();
        change_connection(&mut changed, field, &home);
        worker
            .command(Command::Preferences(changed.clone()))
            .await
            .unwrap();
        assert!(
            worker.capabilities.is_none(),
            "{field} left stale capability evidence usable"
        );
        assert!(matches!(
            worker.effort_profile,
            chat::EffortProfile::Unknown
        ));
        assert!(
            worker.models.is_empty(),
            "{field} retained stale served inventory"
        );
        let snapshot = worker.snapshot();
        assert!(!snapshot.can_send);
        assert!(snapshot.effort_levels.is_empty());
        assert!(
            snapshot.chat.state.contains("reconnect") && snapshot.asr.state.contains("reconnect")
        );
        let persisted: Preferences = worker.store.get_setting("preferences").unwrap().unwrap();
        assert_eq!(json!(persisted), json!(changed));
        assert!(
            worker
                .command(Command::Send {
                    id: session.clone(),
                    text: "must wait for reconnection".into()
                })
                .await
                .is_err()
        );
        assert!(
            worker
                .store
                .load_session(&session)
                .unwrap()
                .unwrap()
                .messages
                .is_empty()
        );
        worker.shutdown().await;
    }
}

#[tokio::test]
async fn connection_preferences_are_rejected_during_inference_or_runtime_transition_without_changing_saved_state()
 {
    for busy in ["inference", "runtime transition"] {
        for field in [
            "chat endpoint",
            "ASR endpoint",
            "executable",
            "cache",
            "model roots",
        ] {
            let home = Home::new();
            let mut worker = home.worker();
            worker.preferences = isolated_preferences(&home);
            worker
                .store
                .set_setting("preferences", &worker.preferences)
                .unwrap();
            if busy == "inference" {
                pending(&mut worker).await;
            } else {
                worker.runtime_busy.insert("chat".into());
            }
            seed_connection_state(&mut worker);
            let before = json!(worker.preferences);
            let old_capabilities = json!(worker.capabilities);
            let mut changed = worker.preferences.clone();
            change_connection(&mut changed, field, &home);
            let error = worker
                .command(Command::Preferences(changed))
                .await
                .unwrap_err();
            assert!(
                error
                    .to_string()
                    .contains("active inference and runtime changes"),
                "{busy}/{field}: {error}"
            );
            assert_eq!(
                json!(worker.preferences),
                before,
                "{busy}/{field} changed in-memory preferences despite rejection"
            );
            let saved: Preferences = worker.store.get_setting("preferences").unwrap().unwrap();
            assert_eq!(
                json!(saved),
                before,
                "{busy}/{field} changed durable preferences despite rejection"
            );
            assert_eq!(json!(worker.capabilities), old_capabilities);
            assert!(matches!(
                worker.effort_profile,
                chat::EffortProfile::ThinkingBudget
            ));
            assert_eq!(worker.models[0].id, "stale-served-model");
            if busy == "inference" {
                assert!(worker.active.is_some());
            } else {
                assert!(worker.runtime_busy.contains("chat"));
            }
            worker.shutdown().await;
        }
    }
}

#[tokio::test]
async fn display_only_preferences_remain_editable_during_inference_and_runtime_transition() {
    for busy in ["idle", "inference", "runtime transition"] {
        let home = Home::new();
        let mut worker = home.worker();
        worker.preferences = isolated_preferences(&home);
        worker
            .store
            .set_setting("preferences", &worker.preferences)
            .unwrap();
        if busy == "inference" {
            pending(&mut worker).await;
        } else if busy == "runtime transition" {
            worker.runtime_busy.insert("asr".into());
        }
        seed_connection_state(&mut worker);
        let old_capabilities = json!(worker.capabilities);
        let active_session = worker.active.as_ref().and_then(|a| a.session.clone());
        let mut changed = worker.preferences.clone();
        changed.window_size = Some([1440.0, 900.0]);
        worker
            .command(Command::Preferences(changed.clone()))
            .await
            .unwrap();
        let saved: Preferences = worker.store.get_setting("preferences").unwrap().unwrap();
        assert_eq!(json!(saved), json!(changed));
        assert_eq!(
            json!(worker.capabilities),
            old_capabilities,
            "display-only changes must not invalidate runtime evidence"
        );
        assert!(matches!(
            worker.effort_profile,
            chat::EffortProfile::ThinkingBudget
        ));
        assert_eq!(worker.chat_view.state, "Ready");
        assert_eq!(worker.asr_view.state, "Ready");
        assert_eq!(worker.models[0].id, "stale-served-model");
        assert_eq!(
            worker.active.as_ref().and_then(|a| a.session.clone()),
            active_session
        );
        assert!(
            worker.task_handles.is_empty(),
            "display-only changes must not launch catalog refresh work"
        );
        if busy == "runtime transition" {
            assert!(worker.runtime_busy.contains("asr"));
        }
        worker.shutdown().await;
    }
}

#[tokio::test]
async fn invalid_endpoint_preferences_preserve_last_valid_configuration_and_capabilities() {
    let home = Home::new();
    let mut worker = home.worker();
    worker.preferences = isolated_preferences(&home);
    worker
        .store
        .set_setting("preferences", &worker.preferences)
        .unwrap();
    seed_connection_state(&mut worker);
    let before = json!(worker.preferences);
    for endpoint in [
        "https://example.invalid",
        "http://192.0.2.1:8000",
        "not-a-url",
    ] {
        let mut changed = worker.preferences.clone();
        changed.chat_endpoint = endpoint.into();
        assert!(worker.command(Command::Preferences(changed)).await.is_err());
        assert_eq!(json!(worker.preferences), before);
        let persisted: Preferences = worker.store.get_setting("preferences").unwrap().unwrap();
        assert_eq!(json!(persisted), before);
        assert!(worker.capabilities.is_some());
        assert!(matches!(
            worker.effort_profile,
            chat::EffortProfile::ThinkingBudget
        ));
    }
    assert!(worker.task_handles.is_empty());
    worker.shutdown().await;
}

#[tokio::test]
async fn focused_session_and_window_size_survive_restart_and_reject_invalid_updates() {
    let home = Home::new();
    let mut worker = home.worker();
    let first = create_session(&mut worker).await;
    let second = create_session(&mut worker).await;
    assert_eq!(worker.snapshot().focused_session, second);
    worker
        .command(Command::FocusSession(first.clone()))
        .await
        .unwrap();
    worker
        .command(Command::WindowSize([1280.0, 820.0]))
        .await
        .unwrap();
    assert!(
        worker
            .command(Command::FocusSession("missing-session".into()))
            .await
            .is_err()
    );
    for size in [
        [0.0, 820.0],
        [-1.0, 820.0],
        [f32::NAN, 820.0],
        [1280.0, f32::INFINITY],
    ] {
        assert!(worker.command(Command::WindowSize(size)).await.is_err());
    }
    assert_eq!(worker.focused_session, first);
    assert_eq!(worker.preferences.window_size, Some([1280.0, 820.0]));
    let saved_focus: String = worker
        .store
        .get_setting("focused_session")
        .unwrap()
        .unwrap();
    let saved_preferences: Preferences = worker.store.get_setting("preferences").unwrap().unwrap();
    assert_eq!(saved_focus, first);
    assert_eq!(saved_preferences.window_size, Some([1280.0, 820.0]));
    worker.shutdown().await;
    drop(worker);
    let reopened = home.worker();
    assert_eq!(reopened.snapshot().focused_session, first);
    assert_eq!(
        reopened.snapshot().preferences.window_size,
        Some([1280.0, 820.0])
    );
    assert_eq!(reopened.sessions.len(), 2);
    assert!(
        reopened
            .sessions
            .iter()
            .all(|s| s.draft == "retained draft")
    );
}

#[tokio::test]
async fn deletion_preview_and_confirmation_reject_external_or_transitioning_runtimes_before_io() {
    for state in [
        "chat external",
        "ASR external",
        "chat changing",
        "ASR changing",
    ] {
        let home = Home::new();
        let mut worker = home.worker();
        worker.preferences = isolated_preferences(&home);
        let preview = runtime::RemovalPreview {
            repository: "fixture/unrelated-repository".into(),
            cache_root: PathBuf::from(&worker.preferences.cache),
            revisions: vec!["synthetic-revision".into()],
            estimated_reclaimable_bytes: Some(100),
        };
        worker.pending_removal = Some(preview.clone());
        match state {
            "chat external" => {
                worker.chat_view.external = true;
                worker.chat_view.model = "different/external-model".into();
            }
            "ASR external" => {
                worker.asr_view.external = true;
                worker.asr_view.model = "different/external-asr".into();
            }
            "chat changing" => {
                worker.runtime_busy.insert("chat".into());
            }
            "ASR changing" => {
                worker.runtime_busy.insert("asr".into());
            }
            _ => unreachable!(),
        }
        for command in [
            Command::PreviewDeletion(preview.repository.clone()),
            Command::DeleteModel(preview.repository.clone()),
        ] {
            let error = worker.command(command).await.unwrap_err().to_string();
            if state.contains("external") {
                assert!(
                    error.contains("Disconnect external inference services"),
                    "{state}: {error}"
                );
                assert!(error.contains("does not stop its process"));
            } else {
                assert!(
                    error.contains("Wait for runtime changes"),
                    "{state}: {error}"
                );
            }
            assert_eq!(
                worker.pending_removal.as_ref(),
                Some(&preview),
                "rejection must not consume the confirmation preview"
            );
            assert!(
                worker.task_handles.is_empty(),
                "guard must run before launching a cache scan or removal"
            );
            assert!(worker.removals.is_empty());
            assert!(worker.operations.is_empty());
            assert!(worker.event_rx.try_recv().is_err());
        }
        assert!(fs::read_dir(&preview.cache_root).unwrap().next().is_none());
        worker.shutdown().await;
    }
}

#[tokio::test]
async fn in_progress_repository_removal_blocks_every_runtime_action_before_io() {
    let home = Home::new();
    let mut worker = home.worker();
    worker.preferences = isolated_preferences(&home);
    worker.removals.insert("fixture/being-removed".into());
    for role in ["chat", "asr"] {
        for action in ["start", "stop", "connect", "disconnect"] {
            let error = worker
                .command(Command::Runtime {
                    role: role.into(),
                    action: action.into(),
                    model: "different/model".into(),
                })
                .await
                .unwrap_err()
                .to_string();
            assert!(
                error.contains("Wait for model removal to finish"),
                "{role}/{action}: {error}"
            );
            assert!(worker.runtime_busy.is_empty());
            assert!(worker.task_handles.is_empty());
            assert!(worker.event_rx.try_recv().is_err());
        }
    }
    assert!(worker.removals.contains("fixture/being-removed"));
    worker.shutdown().await;
}

#[tokio::test]
async fn completed_transcript_persists_retained_recording_or_cleared_staging_and_warns_without_losing_text()
 {
    for retention in ["retained", "deleted staging", "retain failed", "imported"] {
        let home = Home::new();
        let mut worker = home.worker();
        let stage = worker.paths.audio_tmp.join("synthetic-recording.wav");
        let imported = home.0.join("imported-original.wav");
        let retained = worker.paths.audio_retained.join("synthetic-retained.wav");
        fs::write(&imported, b"synthetic imported bytes").unwrap();
        let recording = match retention {
            "retained" => {
                fs::write(&retained, b"synthetic retained bytes").unwrap();
                Some(Ok(Some(retained.clone())))
            }
            "deleted staging" => {
                fs::write(&stage, b"staged bytes").unwrap();
                fs::remove_file(&stage).unwrap();
                Some(Ok(None))
            }
            "retain failed" => Some(Err(
                "Recording retention failed: synthetic disk failure".into()
            )),
            "imported" => None,
            _ => unreachable!(),
        };
        let source = if retention == "imported" {
            imported.display().to_string()
        } else {
            stage.display().to_string()
        };
        let job = Job {
            id: "transcription-fixture".into(),
            model: "actual-asr@revision".into(),
            revision: Some("revision".into()),
            text: String::new(),
            status: "transcribing".into(),
            elapsed_ms: None,
            transcript: None,
            source,
            language: Some("en".into()),
        };
        worker
            .store
            .save_record(&job_record(&job).unwrap())
            .unwrap();
        worker.jobs.push(job);
        let transcript = audio::Transcript {
            id: "transcript-result-fixture".into(),
            model: "actual-asr@revision".into(),
            language_requested: Some("en".into()),
            text: "Durable received transcript".into(),
            edited_text: None,
            detected_language: Some("en".into()),
            audio_duration_seconds: Some(2.5),
            client_elapsed_ms: 750,
            segments: Some(vec![json!({"text":"Durable received transcript"})]),
            status: audio::TranscriptionStatus::Completed,
            server_stop_confirmed: false,
        };
        worker
            .event(Event::Transcript {
                id: "transcription-fixture".into(),
                result: Ok(audio::TranscriptionOutcome::Completed(transcript.clone())),
                recording,
            })
            .await
            .unwrap();
        let expected_source = match retention {
            "retained" => retained.display().to_string(),
            "imported" => imported.display().to_string(),
            _ => String::new(),
        };
        let job = &worker.jobs[0];
        assert_eq!(job.source, expected_source);
        assert_eq!(job.text, transcript.text);
        assert_eq!(job.status, "completed");
        assert_eq!(job.elapsed_ms, Some(750));
        assert_eq!(job.transcript.as_ref(), Some(&transcript));
        if retention == "retain failed" {
            assert!(
                worker
                    .snapshot()
                    .error
                    .as_deref()
                    .is_some_and(|error| error.contains("Recording retention failed"))
            );
        } else {
            assert!(worker.error.is_none());
        }
        let record = worker
            .store
            .load_records("transcription")
            .unwrap()
            .pop()
            .unwrap();
        assert_eq!(record.status, Some(RecordStatus::Completed));
        let stored: Job = serde_json::from_value(record.value).unwrap();
        assert_eq!(stored.source, expected_source);
        assert_eq!(stored.text, transcript.text);
        assert_eq!(stored.transcript, Some(transcript.clone()));
        assert_eq!(fs::read(&imported).unwrap(), b"synthetic imported bytes");
        assert!(!stage.exists());
        if retention == "retained" {
            assert_eq!(fs::read(&retained).unwrap(), b"synthetic retained bytes");
        }
        worker.shutdown().await;
        drop(worker);
        let reopened = home.worker();
        assert_eq!(reopened.jobs[0].source, expected_source);
        assert_eq!(reopened.jobs[0].text, "Durable received transcript");
        assert_eq!(reopened.jobs[0].transcript.as_ref(), Some(&transcript));
        assert_eq!(reopened.snapshot().transcripts[0].status, "completed");
        assert!(
            reopened.task_handles.is_empty(),
            "restoring transcripts must not rerun inference"
        );
    }
}

#[tokio::test]
async fn catalog_failure_preserves_last_good_freshness_then_successful_empty_clears_saved_entries()
{
    let home = Home::new();
    let mut worker = home.worker();
    let session = create_session(&mut worker).await;
    let entry: catalog::AvailableModel = serde_json::from_value(json!({
        "alias":"synthetic-chat", "hf_path":"fixture/catalog-model",
        "modality":"text", "size_bytes":1024
    }))
    .unwrap();
    worker
        .event(Event::Catalog {
            available: vec![entry.clone()],
            local: catalog::CatalogReport {
                models: vec![],
                errors: vec![],
            },
            error: None,
        })
        .await
        .unwrap();
    let updated: String = worker
        .store
        .get_setting("catalog_updated")
        .unwrap()
        .unwrap();
    assert!(chrono::DateTime::parse_from_rfc3339(&updated).is_ok());
    assert!(
        worker
            .snapshot()
            .catalog_source
            .contains("Available: current runtime CLI catalog")
    );
    assert!(
        worker
            .snapshot()
            .catalog_source
            .contains(&format!("last refreshed {updated}"))
    );
    let saved: Vec<catalog::AvailableModel> = worker.store.get_setting("catalog").unwrap().unwrap();
    assert_eq!(json!(saved), json!([entry.clone()]));

    // Freshness is stored at second resolution: cross that boundary so accidentally
    // replacing the last-success timestamp on failure cannot pass unnoticed.
    tokio::time::sleep(Duration::from_millis(1100)).await;
    worker
        .event(Event::Catalog {
            available: vec![],
            local: catalog::CatalogReport {
                models: vec![],
                errors: vec![],
            },
            error: Some("synthetic catalog refresh failure".into()),
        })
        .await
        .unwrap();
    let failed_timestamp: String = worker
        .store
        .get_setting("catalog_updated")
        .unwrap()
        .unwrap();
    let failed_catalog: Vec<catalog::AvailableModel> =
        worker.store.get_setting("catalog").unwrap().unwrap();
    assert_eq!(failed_timestamp, updated);
    assert_eq!(json!(failed_catalog), json!([entry.clone()]));
    assert_eq!(json!(worker.available), json!([entry.clone()]));
    assert!(
        worker
            .snapshot()
            .catalog_source
            .contains("Available: cached runtime CLI catalog")
    );
    assert!(
        worker
            .snapshot()
            .catalog_source
            .contains(&format!("last refreshed {updated}"))
    );
    assert!(worker.models.iter().any(|m| m.id == "alias:synthetic-chat"));
    assert_eq!(worker.sessions[0].model, "actor-fixture");
    worker.shutdown().await;
    drop(worker);

    // A failed initial refresh after restart must recover the same saved catalog.
    let mut worker = home.worker();
    worker
        .event(Event::Catalog {
            available: vec![],
            local: catalog::CatalogReport {
                models: vec![],
                errors: vec![],
            },
            error: Some("synthetic offline restart".into()),
        })
        .await
        .unwrap();
    assert_eq!(json!(worker.available), json!([entry]));
    assert!(
        worker
            .snapshot()
            .catalog_source
            .contains("Available: cached runtime CLI catalog")
    );
    assert!(
        worker
            .snapshot()
            .catalog_source
            .contains(&format!("last refreshed {updated}"))
    );

    worker
        .event(Event::Catalog {
            available: vec![],
            local: catalog::CatalogReport {
                models: vec![],
                errors: vec![],
            },
            error: None,
        })
        .await
        .unwrap();
    let empty_catalog: Vec<catalog::AvailableModel> =
        worker.store.get_setting("catalog").unwrap().unwrap();
    let empty_timestamp: String = worker
        .store
        .get_setting("catalog_updated")
        .unwrap()
        .unwrap();
    assert!(empty_catalog.is_empty());
    assert!(worker.available.is_empty() && worker.models.is_empty());
    assert_ne!(empty_timestamp, updated);
    assert!(
        worker
            .snapshot()
            .catalog_source
            .contains("Available: current runtime CLI catalog")
    );
    assert!(
        worker
            .snapshot()
            .catalog_source
            .contains(&format!("last refreshed {empty_timestamp}"))
    );
    assert!(worker.error.is_none());
    assert_eq!(worker.sessions[0].model, "actor-fixture");
    assert_eq!(
        worker.store.load_session(&session).unwrap().unwrap().model,
        "actor-fixture"
    );
    worker.shutdown().await;
    drop(worker);
    let reopened = home.worker();
    assert!(
        reopened
            .store
            .get_setting::<Vec<catalog::AvailableModel>>("catalog")
            .unwrap()
            .unwrap()
            .is_empty()
    );
    assert_eq!(reopened.sessions[0].model, "actor-fixture");
}
