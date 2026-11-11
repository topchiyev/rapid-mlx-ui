//! Opt-in real-model verification. Uses existing weights, an ephemeral port and isolated state.
use anyhow::{Context, Result, ensure};
use rapid_mlx_core::{
    catalog::CacheRoots,
    chat,
    paths::AppPaths,
    runtime::{self, RuntimeConfig, RuntimeRole, RuntimeSupervisor},
};
use serde_json::json;
use std::{path::PathBuf, time::Duration};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

#[tokio::main]
async fn main() -> Result<()> {
    let model = PathBuf::from(std::env::args_os().nth(1).context(
        "Usage: cargo run -p rapid-mlx-ui --bin runtime-smoke -- <existing-local-model-folder>",
    )?);
    ensure!(model.is_dir(), "Choose an existing local model directory");
    let home = std::env::temp_dir().join(format!("rapid-mlx-ui-smoke-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&home)?;
    let paths = AppPaths::from_home(&home)?;
    let cache = CacheRoots::from_env(None)?.resolve();
    let executable = runtime::discover_executable(None)?;
    let mut supervisor = RuntimeSupervisor::new()?;
    let result = async {
        let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
        let port = listener.local_addr()?.port(); drop(listener);
        let mut config = RuntimeConfig::new(executable, model, "rapid-mlx-ui-smoke".into(), RuntimeRole::Chat, cache);
        config.port = port; config.startup_timeout_secs = 300; config.app_state_root = Some(paths.root.clone());
        println!("Starting test-owned offline runtime on loopback port {port}…");
        supervisor.start(config).await?;
        let endpoint = supervisor.endpoint().context("Runtime has no endpoint")?.to_owned();
        let caps = chat::negotiate_for_runtime(&endpoint, None, Some("1.0.2")).await?;
        println!("Ready; model identity and protocol verified.");
        let (tx, mut rx) = mpsc::channel(128);
        let body = json!({"model":"rapid-mlx-ui-smoke", "messages":[{"role":"user","content":"Reply with exactly: rapid-mlx-ui-smoke-ok"}], "temperature":0.0,"max_tokens":48});
        chat::validate_request(&body, &caps, &chat::EffortProfile::Unknown, false)?;
        let cancel = CancellationToken::new();
        let request = chat::stream_chat_with_capabilities(&endpoint, None, body, tx, cancel, Some(&caps));
        tokio::pin!(request);
        let outcome = tokio::time::timeout(Duration::from_secs(180), async {
            loop { tokio::select! { outcome = &mut request => break outcome, Some(_) = rx.recv() => {} } }
        }).await.context("Smoke chat timed out")??;
        ensure!(outcome.status == chat::ChatStatus::Completed, "Chat failed: {:?}", outcome.error);
        ensure!(!outcome.content.trim().is_empty(), "No answer content received");
        println!("CHAT PASS: {}", outcome.content.trim());
        println!("Reported usage: {}", outcome.usage.as_ref().map(|v| v.to_string()).unwrap_or_else(|| "unavailable".into()));
        println!("Client elapsed={}ms; first output={:?}ms", outcome.elapsed_ms, outcome.first_output_ms);
        let (tx, mut rx) = mpsc::channel(128);
        let cancel = CancellationToken::new();
        let body = json!({"model":"rapid-mlx-ui-smoke", "messages":[{"role":"user","content":"Count from 1 to 1000, one number per line."}], "temperature":0.0,"max_tokens":256});
        let request = chat::stream_chat_with_capabilities(&endpoint, None, body, tx, cancel.clone(), Some(&caps));
        tokio::pin!(request);
        let outcome = tokio::time::timeout(Duration::from_secs(120), async {
            loop { tokio::select! { biased; event = rx.recv() => { match event { Some(chat::ChatEvent::Content(text) | chat::ChatEvent::Reasoning(text)) if !text.is_empty() => cancel.cancel(), None => {}, _ => {} } }, outcome = &mut request => break outcome } }
        }).await.context("Cancellation smoke timed out")??;
        ensure!(outcome.status == chat::ChatStatus::Cancelled, "Cancellation did not end as cancelled: {:?}", outcome.status);
        println!("CANCEL PASS: partial answer={} bytes; confirmed server stop={}", outcome.content.len(), outcome.server_cancel_confirmed);
        Ok::<(), anyhow::Error>(())
    }.await;
    let stopped = supervisor.stop().await;
    println!(
        "Owned runtime cleanup: {}",
        if stopped.is_ok() {
            "stopped and reaped"
        } else {
            "failed"
        }
    );
    let _ = std::fs::remove_dir_all(&home);
    result?;
    stopped?;
    Ok(())
}
