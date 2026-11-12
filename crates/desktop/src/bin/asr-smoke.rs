//! Opt-in speech verification through the production ASR client; no model downloads.
use anyhow::{Context, Result, ensure};
use rapid_mlx_core::{
    audio::{AudioLimits, TranscriptionClient, TranscriptionOutcome},
    catalog::CacheRoots,
    paths::AppPaths,
    runtime::{self, RuntimeConfig, RuntimeRole, RuntimeSupervisor},
};
use std::{path::PathBuf, time::Duration};
use tokio_util::sync::CancellationToken;

#[tokio::main]
async fn main() -> Result<()> {
    let mut args = std::env::args_os().skip(1);
    let usage = "Usage: asr-smoke <existing-local-ASR-folder> <speech.wav> [expected text]";
    let model = PathBuf::from(args.next().context(usage)?);
    let wav = PathBuf::from(args.next().context(usage)?);
    let expected = args
        .next()
        .map(|text| text.into_string())
        .transpose()
        .map_err(|_| anyhow::anyhow!("Expected text must be UTF-8"))?;
    ensure!(args.next().is_none(), "{usage}");
    ensure!(model.is_dir(), "Choose an existing local ASR model folder");
    rapid_mlx_core::audio::validate_audio(&wav, AudioLimits::default())?;
    let paths = AppPaths::discover()?;
    let root = paths
        .cache
        .join(format!("asr-smoke-{}", uuid::Uuid::new_v4()));
    let mut builder = std::fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(&root)?;
    let mut supervisor = RuntimeSupervisor::new()?;
    let result: Result<()> = async {
        let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
        let port = listener.local_addr()?.port();
        drop(listener);
        let mut config = RuntimeConfig::new(
            runtime::discover_executable(None)?,
            model,
            "rapid-mlx-ui-asr-smoke".into(),
            RuntimeRole::Transcription,
            CacheRoots::from_env(None)?.resolve(),
        );
        config.port = port;
        config.app_state_root = Some(root.clone());
        println!("Starting test-owned offline ASR runtime…");
        supervisor.start(config).await?;
        let client = TranscriptionClient::new(supervisor.endpoint().context("No ASR endpoint")?)?;
        let outcome = tokio::time::timeout(
            Duration::from_secs(180),
            client.transcribe(
                &wav,
                "rapid-mlx-ui-asr-smoke",
                None,
                AudioLimits::default(),
                &CancellationToken::new(),
            ),
        )
        .await
        .context("ASR smoke timed out")??;
        let TranscriptionOutcome::Completed(transcript) = outcome else {
            anyhow::bail!("ASR smoke was cancelled");
        };
        ensure!(
            !transcript.text.trim().is_empty(),
            "No recognized text; use a non-silent speech WAV sample"
        );
        if let Some(expected) = expected {
            ensure!(
                transcript.text.trim().eq_ignore_ascii_case(expected.trim()),
                "Recognized text did not match the expected test sentence"
            );
        }
        println!(
            "ASR PASS: {} text characters; language={:?}; duration={:?} seconds; elapsed={} ms",
            transcript.text.chars().count(),
            transcript.detected_language,
            transcript.audio_duration_seconds,
            transcript.client_elapsed_ms
        );
        Ok(())
    }
    .await;
    let stopped = supervisor.stop().await;
    if stopped.is_ok() {
        std::fs::remove_dir_all(&root)?;
        println!("Owned ASR runtime stopped and reaped; temporary state removed.");
    } else {
        eprintln!(
            "Owned runtime cleanup failed; state retained at {}",
            root.display()
        );
    }
    result?;
    stopped?;
    Ok(())
}
