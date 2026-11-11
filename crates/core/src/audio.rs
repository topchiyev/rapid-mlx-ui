//! Local-only transcription and explicit microphone capture. No model acquisition or persistence.
use anyhow::{Context, Result, bail, ensure};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use serde::{Deserialize, Serialize};
use std::{
    fs::{File, OpenOptions},
    io::Read,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, mpsc},
    time::Instant,
};
use tokio_util::sync::CancellationToken;

pub const MAX_AUDIO_BYTES: u64 = 25 * 1024 * 1024;
/// File-picker extensions supported by this adapter, not the larger runtime format set.
pub const SUPPORTED_AUDIO_EXTENSIONS: &[&str] = &["wav"];
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct AudioLimits {
    pub max_bytes: u64,
    pub max_duration_seconds: f64,
}
impl Default for AudioLimits {
    fn default() -> Self {
        Self {
            max_bytes: MAX_AUDIO_BYTES,
            max_duration_seconds: 1800.0,
        }
    }
}
impl AudioLimits {
    fn validate(self) -> Result<()> {
        ensure!(
            self.max_bytes > 44 && self.max_bytes <= MAX_AUDIO_BYTES,
            "Audio size limit must be 45..=26214400 bytes"
        );
        ensure!(
            self.max_duration_seconds.is_finite() && self.max_duration_seconds > 0.0,
            "Audio duration limit must be positive"
        );
        Ok(())
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct AudioInfo {
    pub bytes: u64,
    pub duration_seconds: f64,
    pub sample_rate_hz: u32,
    pub channels: u16,
    pub format: String,
}

fn inspect_wav(bytes: &[u8], limits: AudioLimits) -> Result<AudioInfo> {
    limits.validate()?;
    ensure!(
        bytes.len() as u64 <= limits.max_bytes,
        "Audio exceeds configured size limit"
    );
    ensure!(
        bytes.len() >= 44 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WAVE",
        "Unsupported or malformed audio: choose a PCM/float WAV file"
    );
    let mut reader =
        hound::WavReader::new(std::io::Cursor::new(bytes)).context("Malformed WAV audio")?;
    let spec = reader.spec();
    ensure!(
        spec.sample_rate > 0 && spec.channels > 0,
        "Invalid WAV sample rate or channels"
    );
    ensure!(
        matches!(
            (spec.sample_format, spec.bits_per_sample),
            (hound::SampleFormat::Int, 8 | 16 | 24 | 32) | (hound::SampleFormat::Float, 32)
        ),
        "Unsupported WAV encoding; use PCM 8/16/24/32-bit or float32"
    );
    let expected = reader.len();
    ensure!(
        expected > 0 && expected.is_multiple_of(u32::from(spec.channels)),
        "Audio is empty or contains an incomplete frame"
    );
    let duration = reader.duration() as f64 / f64::from(spec.sample_rate);
    ensure!(
        duration <= limits.max_duration_seconds,
        "Audio exceeds configured duration limit"
    );
    // Decode all bounded samples to reject truncated/corrupt payloads before upload.
    let actual = match spec.sample_format {
        hound::SampleFormat::Int => reader
            .samples::<i32>()
            .collect::<std::result::Result<Vec<_>, _>>()?
            .len(),
        hound::SampleFormat::Float => {
            let samples = reader
                .samples::<f32>()
                .collect::<std::result::Result<Vec<_>, _>>()?;
            ensure!(
                samples.iter().all(|s| s.is_finite()),
                "Audio contains non-finite samples"
            );
            samples.len()
        }
    };
    ensure!(actual == expected as usize, "WAV sample data is truncated");
    Ok(AudioInfo {
        bytes: bytes.len() as u64,
        duration_seconds: duration,
        sample_rate_hz: spec.sample_rate,
        channels: spec.channels,
        format: "wav".into(),
    })
}
fn read_audio(path: &Path, limits: AudioLimits) -> Result<(Vec<u8>, AudioInfo)> {
    limits.validate()?;
    let file = File::open(path).context("Cannot open selected audio")?;
    ensure!(
        file.metadata()?.is_file(),
        "Selected audio must be a regular file"
    );
    ensure!(
        file.metadata()?.len() <= limits.max_bytes,
        "Audio exceeds configured size limit"
    );
    let mut bytes = Vec::new();
    file.take(limits.max_bytes + 1).read_to_end(&mut bytes)?;
    let info = inspect_wav(&bytes, limits)?;
    Ok((bytes, info))
}
/// Blocking bounded validation; invoke on a worker. Imported originals are only read.
pub fn validate_audio(path: &Path, limits: AudioLimits) -> Result<AudioInfo> {
    Ok(read_audio(path, limits)?.1)
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TranscriptionStatus {
    Loading,
    Transcribing,
    Completed,
    Failed,
    Cancelled,
    Interrupted,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Transcript {
    pub id: String,
    pub model: String,
    pub language_requested: Option<String>,
    pub text: String,
    pub edited_text: Option<String>,
    pub detected_language: Option<String>,
    pub audio_duration_seconds: Option<f64>,
    pub client_elapsed_ms: u64,
    pub segments: Option<Vec<serde_json::Value>>,
    pub status: TranscriptionStatus,
    pub server_stop_confirmed: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub enum TranscriptionOutcome {
    Completed(Transcript),
    Cancelled { server_outcome_unknown: bool },
}
#[derive(Deserialize)]
struct WireTranscript {
    text: String,
    language: Option<String>,
    duration: Option<f64>,
    segments: Option<Vec<serde_json::Value>>,
}

#[derive(Clone)]
pub struct TranscriptionClient {
    client: reqwest::Client,
    endpoint: reqwest::Url,
    authorization: Option<reqwest::header::HeaderValue>,
}
impl std::fmt::Debug for TranscriptionClient {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TranscriptionClient")
            .field("endpoint", &self.endpoint)
            .field("credential", &"[REDACTED]")
            .finish()
    }
}
impl TranscriptionClient {
    /// Literal loopback HTTP only; redirects and proxies disabled to preserve locality.
    pub fn new(endpoint: &str) -> Result<Self> {
        Self::with_credential(endpoint, None)
    }
    /// Current-run bearer credential for an external local service. Never serialized or logged.
    pub fn with_credential(endpoint: &str, credential: Option<String>) -> Result<Self> {
        let authorization = credential
            .map(|credential| {
                ensure!(
                    !credential.trim().is_empty(),
                    "ASR credential must not be empty"
                );
                let mut header =
                    reqwest::header::HeaderValue::from_str(&format!("Bearer {credential}"))
                        .map_err(|_| anyhow::anyhow!("Invalid ASR bearer credential"))?;
                header.set_sensitive(true);
                Ok::<_, anyhow::Error>(header)
            })
            .transpose()?;
        let mut endpoint = reqwest::Url::parse(endpoint)?;
        let loopback = endpoint
            .host_str()
            .and_then(|host| {
                host.trim_matches(['[', ']'])
                    .parse::<std::net::IpAddr>()
                    .ok()
            })
            .is_some_and(|ip| ip.is_loopback());
        ensure!(
            endpoint.scheme() == "http"
                && loopback
                && endpoint.username().is_empty()
                && endpoint.password().is_none(),
            "Transcription endpoint must be literal loopback HTTP without credentials"
        );
        endpoint.set_path("/v1/audio/transcriptions");
        endpoint.set_query(None);
        endpoint.set_fragment(None);
        Ok(Self {
            client: reqwest::Client::builder()
                .no_proxy()
                .redirect(reqwest::redirect::Policy::none())
                .timeout(std::time::Duration::from_secs(1800))
                .build()?,
            endpoint,
            authorization,
        })
    }
    /// Caller verifies role readiness, exact served/local model identity, auxiliary assets,
    /// offline runtime controls, and acquires the shared chat/ASR inference guard first.
    /// No temperature field: installed route does not accept it despite the schema model.
    pub async fn transcribe(
        &self,
        path: &Path,
        model: &str,
        language: Option<&str>,
        limits: AudioLimits,
        cancel: &CancellationToken,
    ) -> Result<TranscriptionOutcome> {
        ensure!(!model.trim().is_empty(), "Select a ready local ASR model");
        if cancel.is_cancelled() {
            return Ok(TranscriptionOutcome::Cancelled {
                server_outcome_unknown: false,
            });
        }
        let path = path.to_owned();
        let prepared = tokio::task::spawn_blocking(move || read_audio(&path, limits));
        let (bytes, _) = tokio::select! { biased; _ = cancel.cancelled() => return Ok(TranscriptionOutcome::Cancelled { server_outcome_unknown: false }), result = prepared => result?? };
        let mut form = reqwest::multipart::Form::new()
            .text("model", model.to_owned())
            .text("response_format", "verbose_json")
            .part(
                "file",
                reqwest::multipart::Part::bytes(bytes)
                    .file_name("audio.wav")
                    .mime_str("audio/wav")?,
            );
        if let Some(language) = language {
            ensure!(!language.trim().is_empty(), "Language must not be empty");
            form = form.text("language", language.to_owned());
        }
        let start = Instant::now();
        let request = async {
            let mut request = self.client.post(self.endpoint.clone()).multipart(form);
            if let Some(authorization) = &self.authorization {
                request = request.header(reqwest::header::AUTHORIZATION, authorization.clone());
            }
            let mut response = request.send().await.map_err(reqwest::Error::without_url)?;
            let status = response.status();
            let mut body = Vec::new();
            while let Some(chunk) = response.chunk().await? {
                ensure!(
                    body.len() + chunk.len() <= 2 * 1024 * 1024,
                    "Transcription response exceeds 2 MiB limit"
                );
                body.extend_from_slice(&chunk);
            }
            if !status.is_success() {
                if status == reqwest::StatusCode::UNAUTHORIZED
                    || status == reqwest::StatusCode::FORBIDDEN
                {
                    bail!(
                        "Local ASR authentication rejected ({status}); check the current-run credential"
                    );
                }
                // Preserve dependency/asset diagnostics for the UI, redacting echoed credentials.
                let mut diagnostic = String::from_utf8_lossy(&body).into_owned();
                if let Some(header) = &self.authorization
                    && let Ok(header) = header.to_str()
                    && let Some(secret) = header.strip_prefix("Bearer ")
                {
                    diagnostic = diagnostic.replace(secret, "[REDACTED]");
                }
                bail!("Local ASR returned {status}: {diagnostic}");
            }
            let result: WireTranscript =
                serde_json::from_slice(&body).context("Malformed ASR response")?;
            ensure!(
                result.duration.is_none_or(|v| v.is_finite() && v >= 0.0),
                "Invalid ASR duration"
            );
            Ok(TranscriptionOutcome::Completed(Transcript {
                id: uuid::Uuid::new_v4().to_string(),
                model: model.into(),
                language_requested: language.map(str::to_owned),
                text: result.text,
                edited_text: None,
                detected_language: result.language,
                audio_duration_seconds: result.duration,
                segments: result.segments,
                client_elapsed_ms: start.elapsed().as_millis().min(u64::MAX as u128) as u64,
                status: TranscriptionStatus::Completed,
                server_stop_confirmed: false,
            }))
        };
        tokio::select! { biased; _ = cancel.cancelled() => Ok(TranscriptionOutcome::Cancelled { server_outcome_unknown: true }), result = request => result }
    }
}

/// RAII ownership applies only to newly created staging files, never imported paths.
pub struct StagedAudio {
    path: PathBuf,
    state_root: PathBuf,
    cleanup: bool,
}
impl StagedAudio {
    pub fn path(&self) -> &Path {
        &self.path
    }
    pub fn retain(mut self, state_root: &Path) -> Result<PathBuf> {
        ensure!(
            state_root.canonicalize()? == self.state_root,
            "Recording retention must use its original state root"
        );
        ensure!(
            self.has_owned_parent(),
            "Recording staging path changed; refusing retention"
        );
        let dir = private_audio_dir(state_root, "retained")?;
        let destination = dir.join(self.path.file_name().context("Missing recording name")?);
        std::fs::rename(&self.path, &destination)?;
        self.cleanup = false;
        Ok(destination)
    }
    fn has_owned_parent(&self) -> bool {
        let audio = self.state_root.join("audio");
        let tmp = audio.join("tmp");
        self.path.parent() == Some(tmp.as_path())
            && [&self.state_root, &audio, &tmp].into_iter().all(|path| {
                std::fs::symlink_metadata(path)
                    .is_ok_and(|meta| meta.is_dir() && !meta.file_type().is_symlink())
            })
    }
}
impl Drop for StagedAudio {
    fn drop(&mut self) {
        if self.cleanup && self.has_owned_parent() {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}
fn private_audio_dir(root: &Path, leaf: &str) -> Result<PathBuf> {
    ensure!(
        root.is_absolute() && root.is_dir(),
        "State root must be an existing absolute directory"
    );
    let root = root.canonicalize()?;
    let mut dir = root;
    for component in ["audio", leaf] {
        dir.push(component);
        match std::fs::symlink_metadata(&dir) {
            Ok(meta) => ensure!(
                meta.is_dir() && !meta.file_type().is_symlink(),
                "Audio staging directories must not be symlinks"
            ),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                std::fs::create_dir(&dir)?;
            }
            Err(error) => return Err(error.into()),
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
        }
    }
    Ok(dir)
}
fn staging_file(root: &Path) -> Result<(StagedAudio, File)> {
    let path = private_audio_dir(root, "tmp")?.join(format!("{}.wav", uuid::Uuid::new_v4()));
    let mut options = OpenOptions::new();
    options.write(true).read(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options.open(&path)?;
    Ok((
        StagedAudio {
            path,
            state_root: root.canonicalize()?,
            cleanup: true,
        },
        file,
    ))
}

/// Call start only after explicit Record action, on a worker: device/permission APIs may block.
/// Stop/Cancel/drop release the cpal stream before joining the encoder. No process is spawned.
pub struct Recording {
    stream: Option<cpal::Stream>,
    writer: Option<std::thread::JoinHandle<Result<()>>>,
    audio: Option<StagedAudio>,
    error: Arc<Mutex<Option<String>>>,
    limits: AudioLimits,
}
impl Recording {
    pub fn start(state_root: &Path, limits: AudioLimits) -> Result<Self> {
        limits.validate()?;
        let device = cpal::default_host()
            .default_input_device()
            .context("No microphone available; select an audio file instead")?;
        let config = device.default_input_config().context("Microphone unavailable or permission denied; check macOS Privacy & Security > Microphone")?;
        let spec = hound::WavSpec {
            channels: config.channels(),
            sample_rate: config.sample_rate(),
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let (audio, file) = staging_file(state_root)?;
        let (sender, receiver) = mpsc::sync_channel::<Vec<i16>>(8);
        let error = Arc::new(Mutex::new(None));
        let device_error = error.clone();
        let on_error = move |e: cpal::Error| {
            if let Ok(mut error) = device_error.lock() {
                *error = Some(format!("Microphone stopped: {e}"));
            }
        };
        let stream_config: cpal::StreamConfig = config.into();
        let stream = match config.sample_format() {
            cpal::SampleFormat::F32 => {
                input_stream::<f32>(&device, stream_config, sender, error.clone(), on_error)?
            }
            cpal::SampleFormat::I16 => {
                input_stream::<i16>(&device, stream_config, sender, error.clone(), on_error)?
            }
            cpal::SampleFormat::U16 => {
                input_stream::<u16>(&device, stream_config, sender, error.clone(), on_error)?
            }
            format => bail!(
                "Unsupported microphone sample format {format}; select a PCM WAV file instead"
            ),
        };
        let writer_error = error.clone();
        let writer = std::thread::spawn(move || {
            encode_recording(file, spec, limits, receiver, writer_error)
        });
        let mut recording = Self {
            stream: Some(stream),
            writer: Some(writer),
            audio: Some(audio),
            error,
            limits,
        };
        if let Err(error) = recording.stream.as_ref().expect("stream exists").play() {
            let _ = recording.release();
            bail!(
                "Cannot start microphone: {error}. Check macOS microphone permission or select a file"
            );
        }
        Ok(recording)
    }
    /// UI polls this for device loss, queue overflow, and configured-limit exhaustion.
    /// On an error the caller must stop/cancel immediately to release the device.
    pub fn error(&self) -> Option<String> {
        self.error.lock().ok().and_then(|e| e.clone())
    }
    fn release(&mut self) -> Result<()> {
        self.stream.take(); // dropping closes device and the callback's sender
        if let Some(writer) = self.writer.take() {
            writer
                .join()
                .map_err(|_| anyhow::anyhow!("Audio encoder panicked"))??;
        }
        Ok(())
    }
    pub fn stop(mut self) -> Result<StagedAudio> {
        self.release()?;
        if let Some(error) = self.error() {
            bail!("{error}");
        }
        let audio = self.audio.take().context("Recording already stopped")?;
        validate_audio(audio.path(), self.limits)?;
        Ok(audio)
    }
    pub fn cancel(mut self) -> Result<()> {
        self.release()?;
        self.audio.take();
        Ok(())
    }
}
impl Drop for Recording {
    fn drop(&mut self) {
        let _ = self.release();
    }
}
fn input_stream<T>(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    sender: mpsc::SyncSender<Vec<i16>>,
    error: Arc<Mutex<Option<String>>>,
    on_error: impl FnMut(cpal::Error) + Send + 'static,
) -> Result<cpal::Stream>
where
    T: cpal::SizedSample,
    i16: cpal::FromSample<T>,
{
    Ok(device.build_input_stream(
        config,
        move |data: &[T], _| {
            let samples = data
                .iter()
                .map(|s| <i16 as cpal::FromSample<T>>::from_sample_(*s))
                .collect();
            if sender.try_send(samples).is_err()
                && let Ok(mut error) = error.try_lock()
            {
                *error = Some("Audio encoder could not keep up; recording incomplete".into());
            }
        },
        on_error,
        None,
    )?)
}
fn encode_recording(
    file: File,
    spec: hound::WavSpec,
    limits: AudioLimits,
    receiver: mpsc::Receiver<Vec<i16>>,
    error: Arc<Mutex<Option<String>>>,
) -> Result<()> {
    let result = encode_wav(file, spec, limits, receiver, error.clone());
    if let Err(cause) = &result
        && let Ok(mut error) = error.lock()
    {
        error.get_or_insert_with(|| format!("Audio encoder failed: {cause:#}"));
    }
    result
}

fn encode_wav(
    file: File,
    spec: hound::WavSpec,
    limits: AudioLimits,
    receiver: mpsc::Receiver<Vec<i16>>,
    error: Arc<Mutex<Option<String>>>,
) -> Result<()> {
    let mut writer = hound::WavWriter::new(std::io::BufWriter::new(file), spec)?;
    let duration_samples = (limits.max_duration_seconds
        * f64::from(spec.sample_rate)
        * f64::from(spec.channels)) as u64;
    let max_samples = ((limits.max_bytes.saturating_sub(44)) / 2).min(duration_samples);
    let mut count = 0;
    for data in receiver {
        if count + data.len() as u64 > max_samples {
            if let Ok(mut error) = error.lock() {
                *error =
                    Some("Recording reached configured size/duration limit; stop recording".into());
            }
            break;
        }
        for sample in data {
            writer.write_sample(sample)?;
            count += 1;
        }
    }
    writer.finalize()?;
    Ok(())
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub enum InsertMode {
    Append,
    Replace,
}
/// Return a composer draft only; sending is a separate caller action.
pub fn insert_transcript(draft: &str, transcript: &str, mode: InsertMode) -> String {
    match mode {
        InsertMode::Replace => transcript.into(),
        InsertMode::Append if draft.is_empty() => transcript.into(),
        InsertMode::Append => format!("{draft}\n{transcript}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn wav(root: &Path) -> PathBuf {
        let path = root.join("source.wav");
        let mut w = hound::WavWriter::create(
            &path,
            hound::WavSpec {
                channels: 1,
                sample_rate: 16000,
                bits_per_sample: 16,
                sample_format: hound::SampleFormat::Int,
            },
        )
        .unwrap();
        for _ in 0..1600 {
            w.write_sample(0i16).unwrap();
        }
        w.finalize().unwrap();
        path
    }
    #[test]
    fn validates_media_limits_and_preserves_original() {
        let dir = tempfile::tempdir().unwrap();
        let path = wav(dir.path());
        let original = std::fs::read(&path).unwrap();
        let info = validate_audio(&path, AudioLimits::default()).unwrap();
        assert_eq!(info.duration_seconds, 0.1);
        assert!(
            validate_audio(
                &path,
                AudioLimits {
                    max_bytes: 100,
                    ..AudioLimits::default()
                }
            )
            .is_err()
        );
        assert!(
            validate_audio(
                &path,
                AudioLimits {
                    max_duration_seconds: 0.01,
                    ..AudioLimits::default()
                }
            )
            .is_err()
        );
        assert_eq!(original, std::fs::read(&path).unwrap());
        std::fs::write(&path, b"not wav").unwrap();
        assert!(validate_audio(&path, AudioLimits::default()).is_err());
        let mut truncated = original;
        truncated.truncate(truncated.len() - 20);
        std::fs::write(&path, truncated).unwrap();
        assert!(validate_audio(&path, AudioLimits::default()).is_err());
    }
    #[test]
    fn simulated_capture_cleanup_stop_and_retention() {
        let dir = tempfile::tempdir().unwrap();
        let (audio, file) = staging_file(dir.path()).unwrap();
        let path = audio.path().to_owned();
        let (sender, receiver) = mpsc::sync_channel(8);
        sender.send(vec![0i16; 100]).unwrap();
        drop(sender);
        encode_recording(
            file,
            hound::WavSpec {
                channels: 1,
                sample_rate: 16000,
                bits_per_sample: 16,
                sample_format: hound::SampleFormat::Int,
            },
            AudioLimits::default(),
            receiver,
            Arc::new(Mutex::new(None)),
        )
        .unwrap();
        assert!(validate_audio(&path, AudioLimits::default()).is_ok());
        drop(audio);
        assert!(!path.exists());
        let (audio, _) = staging_file(dir.path()).unwrap();
        let retained = audio.retain(dir.path()).unwrap();
        assert!(retained.starts_with(dir.path().canonicalize().unwrap().join("audio/retained")));
        assert!(retained.exists());
    }
    fn simulated_recording(root: &Path, samples: Vec<i16>) -> Recording {
        let (audio, file) = staging_file(root).unwrap();
        let error = Arc::new(Mutex::new(None));
        let encoder_error = error.clone();
        let (sender, receiver) = mpsc::sync_channel(8);
        sender.send(samples).unwrap();
        drop(sender);
        let writer = std::thread::spawn(move || {
            encode_recording(
                file,
                hound::WavSpec {
                    channels: 1,
                    sample_rate: 16000,
                    bits_per_sample: 16,
                    sample_format: hound::SampleFormat::Int,
                },
                AudioLimits::default(),
                receiver,
                encoder_error,
            )
        });
        Recording {
            stream: None,
            writer: Some(writer),
            audio: Some(audio),
            error,
            limits: AudioLimits::default(),
        }
    }
    #[test]
    fn native_encoder_failure_is_observable_before_stop() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("read-only.wav");
        std::fs::write(&path, []).unwrap();
        let read_only = File::open(path).unwrap();
        let (sender, receiver) = mpsc::sync_channel(1);
        drop(sender);
        let error = Arc::new(Mutex::new(None));
        let result = encode_recording(
            read_only,
            hound::WavSpec {
                channels: 1,
                sample_rate: 16000,
                bits_per_sample: 16,
                sample_format: hound::SampleFormat::Int,
            },
            AudioLimits::default(),
            receiver,
            error.clone(),
        );
        assert!(result.is_err());
        assert!(
            error
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .starts_with("Audio encoder failed:")
        );
    }
    #[test]
    fn simulated_stop_cancel_drop_and_empty_release_owned_files() {
        let root = tempfile::tempdir().unwrap();
        let recording = simulated_recording(root.path(), vec![0; 50]);
        let path = recording.audio.as_ref().unwrap().path().to_owned();
        let audio = recording.stop().unwrap();
        assert!(path.exists());
        drop(audio);
        assert!(!path.exists());
        let recording = simulated_recording(root.path(), vec![0; 50]);
        let path = recording.audio.as_ref().unwrap().path().to_owned();
        recording.cancel().unwrap();
        assert!(!path.exists());
        let recording = simulated_recording(root.path(), vec![0; 50]);
        let path = recording.audio.as_ref().unwrap().path().to_owned();
        drop(recording);
        assert!(!path.exists());
        let recording = simulated_recording(root.path(), vec![]);
        let path = recording.audio.as_ref().unwrap().path().to_owned();
        assert!(recording.stop().is_err());
        assert!(!path.exists());
    }
    #[tokio::test]
    async fn cancellation_before_dispatch_does_not_read_or_upload() {
        let client = TranscriptionClient::new("http://127.0.0.1:1").unwrap();
        let cancel = CancellationToken::new();
        cancel.cancel();
        let result = client
            .transcribe(
                Path::new("/nonexistent"),
                "local-asr",
                None,
                AudioLimits::default(),
                &cancel,
            )
            .await
            .unwrap();
        assert_eq!(
            result,
            TranscriptionOutcome::Cancelled {
                server_outcome_unknown: false
            }
        );
        assert!(TranscriptionClient::new("https://example.org").is_err());
    }
    async fn read_upload(socket: &mut tokio::net::TcpStream) -> String {
        use tokio::io::AsyncReadExt;
        let mut bytes = Vec::new();
        loop {
            let mut chunk = [0; 4096];
            let n = socket.read(&mut chunk).await.unwrap();
            assert!(n > 0, "upload closed unexpectedly");
            bytes.extend_from_slice(&chunk[..n]);
            if let Some(end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                let headers = String::from_utf8_lossy(&bytes[..end]);
                let length: usize = headers
                    .lines()
                    .find_map(|line| {
                        let (key, value) = line.split_once(':')?;
                        key.eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse().unwrap())
                    })
                    .expect("multipart has known length");
                if bytes.len() >= end + 4 + length {
                    break;
                }
            }
        }
        String::from_utf8_lossy(&bytes).into_owned()
    }
    #[tokio::test]
    async fn multipart_uses_selected_model_and_verified_response() {
        use tokio::io::AsyncWriteExt;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let client =
            TranscriptionClient::new(&format!("http://{}", listener.local_addr().unwrap()))
                .unwrap();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let request = read_upload(&mut socket).await;
            assert!(request.starts_with("POST /v1/audio/transcriptions HTTP/1.1"));
            assert!(!request.to_ascii_lowercase().contains("authorization:"));
            assert!(request.contains("name=\"model\"\r\n\r\nselected-local-asr"));
            assert!(request.contains("name=\"language\"\r\n\r\nen"));
            assert!(request.contains("name=\"response_format\"\r\n\r\nverbose_json"));
            assert!(request.contains("filename=\"audio.wav\""));
            let body = r#"{"text":"hello","language":"en","duration":0.1,"segments":[]}"#;
            socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body).as_bytes()).await.unwrap();
        });
        let dir = tempfile::tempdir().unwrap();
        let path = wav(dir.path());
        let outcome = client
            .transcribe(
                &path,
                "selected-local-asr",
                Some("en"),
                AudioLimits::default(),
                &CancellationToken::new(),
            )
            .await
            .unwrap();
        server.await.unwrap();
        let TranscriptionOutcome::Completed(transcript) = outcome else {
            panic!("expected transcript")
        };
        assert_eq!(transcript.text, "hello");
        assert_eq!(transcript.model, "selected-local-asr");
        assert_eq!(transcript.audio_duration_seconds, Some(0.1));
        assert_eq!(
            serde_json::from_str::<Transcript>(&serde_json::to_string(&transcript).unwrap())
                .unwrap(),
            transcript
        );
    }
    #[tokio::test]
    async fn current_run_credential_is_header_only_and_errors_are_redacted() {
        use tokio::io::AsyncWriteExt;
        const SECRET: &str = "test-current-run-asr-secret";
        for status in ["200 OK", "401 Unauthorized", "500 Internal Server Error"] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let client = TranscriptionClient::with_credential(
                &format!("http://{}", listener.local_addr().unwrap()),
                Some(SECRET.into()),
            )
            .unwrap();
            assert!(!format!("{client:?}").contains(SECRET));
            assert!(!format!("{:?}", client.authorization).contains(SECRET));
            let server = tokio::spawn(async move {
                let (mut socket, _) = listener.accept().await.unwrap();
                let request = read_upload(&mut socket).await;
                let (headers, multipart) = request.split_once("\r\n\r\n").unwrap();
                assert!(
                    headers
                        .to_ascii_lowercase()
                        .contains(&format!("authorization: bearer {SECRET}"))
                );
                assert!(!multipart.contains(SECRET));
                assert!(multipart.contains("name=\"model\"\r\n\r\nlocal-asr"));
                let body = if status.starts_with("200") {
                    r#"{"text":"synthetic transcript"}"#.to_owned()
                } else {
                    format!("echoed bearer secret: {SECRET}")
                };
                socket.write_all(format!("HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
            });
            let dir = tempfile::tempdir().unwrap();
            let path = wav(dir.path());
            let outcome = client
                .transcribe(
                    &path,
                    "local-asr",
                    None,
                    AudioLimits::default(),
                    &CancellationToken::new(),
                )
                .await;
            server.await.unwrap();
            if status.starts_with("200") {
                let outcome = outcome.unwrap();
                assert!(!serde_json::to_string(&outcome).unwrap().contains(SECRET));
                assert!(!format!("{outcome:?}").contains(SECRET));
            } else {
                let diagnostic = format!("{:#}", outcome.unwrap_err());
                assert!(!diagnostic.contains(SECRET));
                assert!(diagnostic.contains(if status.starts_with("401") {
                    "authentication rejected"
                } else {
                    "[REDACTED]"
                }));
            }
        }
        let invalid = "test-invalid-secret\r\nInjected: value";
        let diagnostic = format!(
            "{:#}",
            TranscriptionClient::with_credential("http://127.0.0.1:1", Some(invalid.into()))
                .unwrap_err()
        );
        assert!(!diagnostic.contains("test-invalid-secret"));
    }
    #[test]
    fn stop_finalizes_all_frames_before_returning() {
        let root = tempfile::tempdir().unwrap();
        let recording = simulated_recording(root.path(), vec![10; 160]);
        let audio = recording.stop().unwrap();
        let reader = hound::WavReader::open(audio.path()).unwrap();
        assert_eq!(reader.len(), 160);
        drop(reader);
        let finalized = std::fs::read(audio.path()).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        assert_eq!(std::fs::read(audio.path()).unwrap(), finalized);
    }
    #[test]
    fn retention_cannot_move_recordings_to_another_state_root() {
        let root = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        let (audio, file) = staging_file(root.path()).unwrap();
        drop(file);
        let original = audio.path().to_owned();
        assert!(audio.retain(other.path()).is_err());
        assert!(!original.exists());
        assert!(!other.path().join("audio").exists());
    }
    #[cfg(unix)]
    #[test]
    fn cleanup_skips_replaced_staging_directory_and_never_removes_external_file() {
        use std::os::unix::fs::symlink;
        let root = tempfile::tempdir().unwrap();
        let external = tempfile::tempdir().unwrap();
        let (audio, file) = staging_file(root.path()).unwrap();
        drop(file);
        let name = audio.path().file_name().unwrap().to_owned();
        let external_file = external.path().join(name);
        std::fs::write(&external_file, b"preserve external audio").unwrap();
        let tmp = root.path().join("audio/tmp");
        std::fs::rename(&tmp, root.path().join("audio/abandoned-tmp")).unwrap();
        symlink(external.path(), &tmp).unwrap();
        drop(audio);
        assert_eq!(
            std::fs::read(external_file).unwrap(),
            b"preserve external audio"
        );
        assert!(staging_file(root.path()).is_err());
    }
    #[tokio::test]
    async fn cancelled_upload_has_unknown_server_outcome() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let client =
            TranscriptionClient::new(&format!("http://{}", listener.local_addr().unwrap()))
                .unwrap();
        let (uploaded, received) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            read_upload(&mut socket).await;
            uploaded.send(()).unwrap();
            std::future::pending::<()>().await;
        });
        let dir = tempfile::tempdir().unwrap();
        let path = wav(dir.path());
        let cancel = CancellationToken::new();
        let request_cancel = cancel.clone();
        let request = tokio::spawn(async move {
            client
                .transcribe(
                    &path,
                    "local-asr",
                    None,
                    AudioLimits::default(),
                    &request_cancel,
                )
                .await
        });
        tokio::time::timeout(std::time::Duration::from_secs(3), received)
            .await
            .unwrap()
            .unwrap();
        cancel.cancel();
        let outcome = tokio::time::timeout(std::time::Duration::from_secs(1), request)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(
            outcome,
            TranscriptionOutcome::Cancelled {
                server_outcome_unknown: true
            }
        );
        server.abort();
        let _ = server.await;
    }
    #[test]
    fn draft_insert_is_explicit() {
        assert_eq!(
            insert_transcript("draft", "speech", InsertMode::Append),
            "draft\nspeech"
        );
    }
}
