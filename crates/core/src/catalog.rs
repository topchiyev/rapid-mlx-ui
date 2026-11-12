//! Read-only local inventory. A candidate is not a successful runtime load.
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Read,
    path::{Path, PathBuf},
};

const MAX_METADATA: u64 = 4 * 1024 * 1024;
const MAX_FILES: usize = 100_000;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CacheRoots {
    pub app_override: Option<PathBuf>,
    pub hf_hub_cache: Option<PathBuf>,
    pub hf_home: Option<PathBuf>,
    pub xdg_cache_home: Option<PathBuf>,
    pub home: PathBuf,
}
impl CacheRoots {
    pub fn from_env(app_override: Option<PathBuf>) -> Result<Self> {
        Ok(Self {
            app_override,
            hf_hub_cache: std::env::var_os("HF_HUB_CACHE").map(PathBuf::from),
            hf_home: std::env::var_os("HF_HOME").map(PathBuf::from),
            xdg_cache_home: std::env::var_os("XDG_CACHE_HOME").map(PathBuf::from),
            home: std::env::var_os("HOME")
                .map(PathBuf::from)
                .context("HOME is unavailable; configure a cache directory")?,
        })
    }
    pub fn resolve(&self) -> PathBuf {
        self.app_override
            .clone()
            .or_else(|| self.hf_hub_cache.clone())
            .or_else(|| self.hf_home.as_ref().map(|p| p.join("hub")))
            .or_else(|| {
                self.xdg_cache_home
                    .as_ref()
                    .map(|p| p.join("huggingface/hub"))
            })
            .unwrap_or_else(|| self.home.join(".cache/huggingface/hub"))
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", content = "reason", rename_all = "snake_case")]
pub enum Compatibility {
    Candidate,
    Incomplete(String),
    Unsupported(String),
    Unknown(String),
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LocalModel {
    pub repository: Option<String>,
    pub revision: Option<String>,
    pub load_path: PathBuf,
    pub cache_root: Option<PathBuf>,
    pub logical_weight_bytes: u64,
    pub format: Option<String>,
    pub quantization: Option<Value>,
    pub model_type: Option<String>,
    pub compatibility: Compatibility,
}
impl LocalModel {
    pub fn can_attempt_load(&self) -> bool {
        matches!(
            self.compatibility,
            Compatibility::Candidate | Compatibility::Unknown(_)
        )
    }
    pub fn revalidate(&self) -> Result<Self> {
        let mut current = inspect_model(&self.load_path)?;
        current.repository.clone_from(&self.repository);
        current.revision.clone_from(&self.revision);
        current.cache_root.clone_from(&self.cache_root);
        if !current.can_attempt_load() {
            bail!("model cannot be activated: {:?}", current.compatibility);
        }
        Ok(current)
    }
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CatalogReport {
    pub models: Vec<LocalModel>,
    pub errors: Vec<String>,
}

fn read_json(path: &Path) -> Result<Value> {
    let file = fs::File::open(path).with_context(|| format!("read {}", path.display()))?;
    if file.metadata()?.len() > MAX_METADATA {
        bail!("metadata exceeds 4 MiB: {}", path.display());
    }
    let mut bytes = Vec::new();
    file.take(MAX_METADATA + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_METADATA {
        bail!("metadata grew beyond limit");
    }
    Ok(serde_json::from_slice(&bytes)?)
}
fn walk_files(
    root: &Path,
    depth: usize,
    files: &mut Vec<PathBuf>,
    remaining: &mut usize,
) -> Result<()> {
    if depth > 8 {
        bail!("model directory exceeds nesting limit");
    }
    for entry in fs::read_dir(root)? {
        if *remaining == 0 {
            bail!("model exceeds file/directory inventory limit");
        }
        *remaining -= 1;
        let entry = entry?;
        let path = entry.path();
        let kind = entry.file_type()?;
        if kind.is_dir() {
            // A nested checkpoint is independent. Its missing shards must not
            // invalidate a complete sibling/parent checkpoint.
            if path.join("config.json").symlink_metadata().is_ok() {
                continue;
            }
            walk_files(&path, depth + 1, files, remaining)?;
        } else {
            // Follow ordinary HF file links, never directory links (cycles/escapes).
            if kind.is_symlink()
                && fs::metadata(&path)
                    .context("broken model symlink")?
                    .is_dir()
            {
                bail!("directory symlinks are not scanned");
            }
            files.push(path);
        }
    }
    Ok(())
}
pub const QWEN_ASR_REPOSITORY: &str = "mlx-community/Qwen3-ASR-0.6B-5bit";
pub const NEMOTRON_ASR_REPOSITORY: &str = "mlx-community/nemotron-3.5-asr-streaming-0.6b";
pub const UNSUPPORTED_ASR: &str = "STT currently supports only mlx-community/Qwen3-ASR-0.6B-5bit and mlx-community/nemotron-3.5-asr-streaming-0.6b; select a tested model";

/// Exact repository identity, optionally followed by an explicit revision.
pub fn supported_asr_repository(identity: &str) -> Option<&'static str> {
    let repository = match identity.split_once('@') {
        Some((repository, revision)) if !revision.is_empty() => repository,
        Some(_) => return None,
        None => identity,
    };
    [QWEN_ASR_REPOSITORY, NEMOTRON_ASR_REPOSITORY]
        .into_iter()
        .find(|supported| supported.eq_ignore_ascii_case(repository))
}

/// Custom folders without a verified cache repository identity remain inventory only.
pub fn local_asr_repository(path: &Path) -> Result<Option<&'static str>> {
    let path = path.canonicalize()?;
    let repository = path.ancestors().find_map(|ancestor| {
        if ancestor.file_name()? != "snapshots" {
            return None;
        }
        let name = ancestor.parent()?.file_name()?.to_str()?;
        supported_asr_repository(&name.strip_prefix("models--")?.replace("--", "/"))
    });
    let family = local_asr_family(&path)?;
    Ok(repository.filter(|repository| {
        matches!(
            (*repository, family.as_deref()),
            (QWEN_ASR_REPOSITORY, Some("qwen3_asr"))
                | (NEMOTRON_ASR_REPOSITORY, Some("nemotron_asr"))
        )
    }))
}

/// Recognized STT families for inventory classification, not support approval.
/// Aligner/TTS entries are deliberately excluded. This is routing evidence,
/// not a claim that optional processors or dependencies are locally complete.
pub fn local_asr_family(path: &Path) -> Result<Option<String>> {
    let config = read_json(&path.join("config.json"))?;
    let normalize = |name: &str| name.to_lowercase().replace('-', "_");
    let families = [
        "fireredasr2",
        "granite_speech5_ctc",
        "nemotron_asr",
        "parakeet",
        "qwen3_asr",
        "sensevoice",
        "whisper",
        "voxtral_realtime",
    ];
    if let Some(kind) = config.get("model_type").and_then(Value::as_str) {
        let kind = normalize(kind);
        if families.contains(&kind.as_str()) {
            return Ok(Some(kind));
        }
    }
    if let Some(architectures) = config.get("architectures").and_then(Value::as_array) {
        for arch in architectures.iter().filter_map(Value::as_str) {
            let arch = normalize(arch).replace(['_', '.'], "");
            if let Some(family) = families
                .iter()
                .find(|family| arch.starts_with(&family.replace('_', "")))
            {
                return Ok(Some((*family).into()));
            }
        }
    }
    Ok(None)
}
pub fn inspect_model(path: &Path) -> Result<LocalModel> {
    let path = path
        .canonicalize()
        .with_context(|| format!("model directory missing: {}", path.display()))?;
    if !path.is_dir() {
        bail!("model path is not a directory");
    }
    let mut model = LocalModel {
        repository: None,
        revision: None,
        load_path: path.clone(),
        cache_root: None,
        logical_weight_bytes: 0,
        format: None,
        quantization: None,
        model_type: None,
        compatibility: Compatibility::Candidate,
    };
    let inspect = || -> Result<(Value, Vec<PathBuf>)> {
        let config = read_json(&path.join("config.json"))?;
        if !config.is_object() {
            bail!("model config must be an object");
        }
        let mut files = Vec::new();
        let mut remaining = MAX_FILES;
        walk_files(&path, 0, &mut files, &mut remaining)?;
        Ok((config, files))
    };
    let (config, files) = match inspect() {
        Ok(v) => v,
        Err(e) => {
            model.compatibility = Compatibility::Incomplete(e.to_string());
            return Ok(model);
        }
    };
    model.quantization = config
        .get("quantization")
        .or_else(|| config.get("quantization_config"))
        .cloned();
    model.model_type = config
        .get("model_type")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let asr_family = local_asr_family(&path)?;
    let mut format = "safetensors";
    let mut weights: Vec<_> = files
        .iter()
        .filter(|p| p.extension().is_some_and(|e| e == "safetensors"))
        .collect();
    // The verified mlx-audio loader prefers safetensors, then top-level NPZ.
    // NPZ is an audio contract, not evidence that an arbitrary chat model loads.
    if weights.is_empty() && asr_family.is_some() {
        format = "npz";
        weights = files
            .iter()
            .filter(|p| {
                p.parent() == Some(path.as_path()) && p.extension().is_some_and(|e| e == "npz")
            })
            .collect();
    }
    for file in &weights {
        let bytes = fs::metadata(file)?.len();
        if format == "npz" && bytes == 0 {
            model.compatibility = Compatibility::Incomplete("Empty NPZ weight file".into());
            return Ok(model);
        }
        model.logical_weight_bytes = model.logical_weight_bytes.saturating_add(bytes);
    }
    if weights.is_empty() {
        model.compatibility = if files
            .iter()
            .any(|p| p.extension().is_some_and(|e| e == "npz"))
        {
            Compatibility::Unsupported(
                "NPZ weights require a recognized ASR family and top-level weight files".into(),
            )
        } else if files
            .iter()
            .any(|p| p.extension().is_some_and(|e| e == "gguf" || e == "bin"))
        {
            Compatibility::Unsupported(
                "GGUF/.bin-only models cannot load; use supported safetensors or ASR NPZ weights"
                    .into(),
            )
        } else {
            Compatibility::Incomplete(if asr_family.is_some() {
                "No safetensors or top-level NPZ weights found".into()
            } else {
                "No safetensors weights found".into()
            })
        };
        return Ok(model);
    }
    model.format = Some(format.into());
    for index in files.iter().filter(|p| {
        p.file_name()
            .is_some_and(|n| n.to_string_lossy().ends_with(".safetensors.index.json"))
    }) {
        let validate = || -> Result<()> {
            let json = read_json(index)?;
            let map = json
                .get("weight_map")
                .and_then(Value::as_object)
                .context("index has no weight_map")?;
            if map.is_empty() {
                bail!("empty shard index");
            }
            for shard in map.values() {
                let relative = Path::new(shard.as_str().context("invalid indexed shard")?);
                if relative.is_absolute()
                    || relative
                        .components()
                        .any(|c| !matches!(c, std::path::Component::Normal(_)))
                {
                    bail!("unsafe indexed shard path");
                }
                let shard_path = index
                    .parent()
                    .context("index has no parent")?
                    .join(relative);
                if !shard_path.is_file() {
                    bail!("missing indexed shard: {}", relative.display());
                }
            }
            Ok(())
        };
        if let Err(e) = validate() {
            model.compatibility = Compatibility::Incomplete(e.to_string());
            return Ok(model);
        }
    }
    // Tokenizers/ASR processors vary by architecture. Missing metadata is unknown,
    // never inferred compatibility from a model/repository name.
    model.compatibility = if asr_family.is_some() && local_asr_repository(&path)?.is_none() {
        Compatibility::Unsupported(UNSUPPORTED_ASR.into())
    } else if asr_family.is_some() {
        Compatibility::Unknown("Recognized ASR family; local auxiliary assets and runtime dependencies require explicit offline load validation".into())
    } else if model.model_type.is_none() {
        Compatibility::Unknown("config does not identify a model_type".into())
    } else if !files.iter().any(|p| {
        p.file_name().is_some_and(|n| {
            n == "tokenizer.json" || n == "tokenizer.model" || n == "preprocessor_config.json"
        })
    }) {
        Compatibility::Incomplete("Missing local tokenizer or audio processor metadata".into())
    } else {
        Compatibility::Unknown("Static files are present; runtime architecture and auxiliary assets require explicit load validation".into())
    };
    Ok(model)
}
/// Find checkpoint roots without following directory symlinks. A snapshot can
/// contain several independently loadable quantization folders. A single budget
/// bounds all directory entries visited for this snapshot, including containers.
fn checkpoint_paths(root: &Path) -> (Vec<PathBuf>, Vec<String>) {
    fn visit(
        path: &Path,
        depth: usize,
        remaining: &mut usize,
        candidates: &mut Vec<PathBuf>,
        errors: &mut Vec<String>,
    ) {
        if depth > 8 {
            errors.push(format!(
                "{}: checkpoint nesting exceeds eight levels",
                path.display()
            ));
            return;
        }
        let entries = match fs::read_dir(path) {
            Ok(entries) => entries,
            Err(e) => {
                errors.push(format!("{}: {e}", path.display()));
                return;
            }
        };
        let mut children = Vec::new();
        let mut checkpoint = false;
        for entry in entries {
            if *remaining == 0 {
                errors.push(format!(
                    "{}: checkpoint inventory exceeds {MAX_FILES} entries",
                    path.display()
                ));
                return;
            }
            *remaining -= 1;
            let entry = match entry {
                Ok(entry) => entry,
                Err(e) => {
                    errors.push(format!("{}: {e}", path.display()));
                    continue;
                }
            };
            let kind = match entry.file_type() {
                Ok(kind) => kind,
                Err(e) => {
                    errors.push(format!("{}: {e}", entry.path().display()));
                    continue;
                }
            };
            if kind.is_dir() {
                children.push(entry.path());
            } else if kind.is_file() || kind.is_symlink() {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                checkpoint |= name == "config.json"
                    || name.ends_with(".safetensors")
                    || name.ends_with(".safetensors.index.json")
                    || name.ends_with(".npz")
                    || name.ends_with(".gguf")
                    || name.ends_with(".bin");
            }
        }
        if checkpoint {
            candidates.push(path.to_path_buf());
        }
        children.sort();
        for child in children {
            if *remaining == 0 {
                errors.push(format!(
                    "{}: checkpoint inventory limit reached before scanning child directories",
                    path.display()
                ));
                break;
            }
            visit(&child, depth + 1, remaining, candidates, errors);
        }
    }
    let mut candidates = Vec::new();
    let mut errors = Vec::new();
    let mut remaining = MAX_FILES;
    visit(root, 0, &mut remaining, &mut candidates, &mut errors);
    if candidates.is_empty() {
        candidates.push(root.to_path_buf());
    }
    (candidates, errors)
}
/// Synchronous filesystem work: call on a blocking worker, never the UI thread.
pub fn discover(cache_root: &Path, extra_dirs: &[PathBuf]) -> CatalogReport {
    let mut report = CatalogReport::default();
    let mut seen = BTreeSet::new();
    fn insert(
        report: &mut CatalogReport,
        seen: &mut BTreeSet<PathBuf>,
        path: PathBuf,
        repo: Option<String>,
        revision: Option<String>,
        cache: Option<PathBuf>,
    ) {
        match inspect_model(&path) {
            Ok(mut model) => {
                if seen.insert(model.load_path.clone()) {
                    model.repository = repo;
                    model.revision = revision;
                    model.cache_root = cache;
                    report.models.push(model);
                }
            }
            Err(e) => report.errors.push(format!("{}: {e}", path.display())),
        }
    }
    match fs::read_dir(cache_root) {
        Ok(repos) => {
            for repo in repos {
                let repo = match repo {
                    Ok(r) => r,
                    Err(e) => {
                        report.errors.push(e.to_string());
                        continue;
                    }
                };
                let name = repo.file_name().to_string_lossy().into_owned();
                let Some(id) = name.strip_prefix("models--") else {
                    continue;
                };
                if !repo.file_type().is_ok_and(|t| t.is_dir()) {
                    continue;
                }
                let snapshots = repo.path().join("snapshots");
                match fs::read_dir(&snapshots) {
                    Ok(revisions) => {
                        for rev in revisions {
                            let rev = match rev {
                                Ok(rev) => rev,
                                Err(e) => {
                                    report.errors.push(format!("{}: {e}", snapshots.display()));
                                    continue;
                                }
                            };
                            let kind = match rev.file_type() {
                                Ok(kind) => kind,
                                Err(e) => {
                                    report.errors.push(format!("{}: {e}", rev.path().display()));
                                    continue;
                                }
                            };
                            if kind.is_dir() {
                                let (paths, errors) = checkpoint_paths(&rev.path());
                                report.errors.extend(errors);
                                for path in paths {
                                    insert(
                                        &mut report,
                                        &mut seen,
                                        path,
                                        Some(id.replace("--", "/")),
                                        Some(rev.file_name().to_string_lossy().into_owned()),
                                        Some(cache_root.to_path_buf()),
                                    );
                                }
                            }
                        }
                    }
                    Err(e) => report.errors.push(format!("{}: {e}", snapshots.display())),
                }
            }
        }
        Err(e) => report.errors.push(format!("{}: {e}", cache_root.display())),
    }
    for path in extra_dirs {
        insert(&mut report, &mut seen, path.clone(), None, None, None);
    }
    report.models.sort_by(|a, b| a.load_path.cmp(&b.load_path));
    report
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AvailableModel {
    pub alias: String,
    #[serde(alias = "hf_id")]
    pub hf_path: String,
    #[serde(default)]
    pub revision: Option<String>,
    #[serde(default)]
    pub modality: String,
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub size_bytes: Option<u64>,
    #[serde(default)]
    pub reasoning_parser: Option<String>,
    #[serde(default)]
    pub tool_call_parser: Option<String>,
    #[serde(flatten)]
    pub metadata: BTreeMap<String, Value>,
}
impl AvailableModel {
    pub fn is_asr(&self) -> bool {
        self.modality == "audio"
            && self.kind.as_deref() == Some("stt")
            && supported_asr_repository(&self.hf_path).is_some()
            && self.metadata.get("family").and_then(Value::as_str) != Some("qwen3_aligner")
    }
    pub fn is_chat(&self) -> bool {
        self.modality == "text"
            && self.metadata.get("standalone").and_then(Value::as_bool) != Some(false)
    }
}
pub fn parse_available_catalog(bytes: &[u8]) -> Result<Vec<AvailableModel>> {
    let payload: Value = serde_json::from_slice(bytes)?;
    let mut result = Vec::new();
    for bucket in ["text", "audio", "video", "image"] {
        let rows = payload
            .get(bucket)
            .and_then(Value::as_array)
            .with_context(|| format!("missing available catalog bucket {bucket}"))?;
        for row in rows {
            let model: AvailableModel = serde_json::from_value(row.clone())?;
            if model.modality == "audio" && model.kind.as_deref() == Some("stt") && !model.is_asr()
            {
                continue;
            }
            result.push(model);
        }
    }
    Ok(result)
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CachedModel {
    pub repo: String,
    pub state: String,
    pub size_bytes: u64,
    pub external: bool,
    #[serde(default)]
    pub alias: Option<String>,
    #[serde(default)]
    pub subfolder: Option<String>,
    #[serde(default)]
    pub launch_path: Option<PathBuf>,
    #[serde(default)]
    pub chat_ready: Option<bool>,
}
pub fn parse_cached_catalog(bytes: &[u8]) -> Result<Vec<CachedModel>> {
    #[derive(Deserialize)]
    struct Payload {
        cached: Vec<CachedModel>,
    }
    Ok(serde_json::from_slice::<Payload>(bytes)?.cached)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn temp() -> PathBuf {
        let p = std::env::temp_dir().join(format!("rapid-catalog-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&p).unwrap();
        p.canonicalize().unwrap()
    }
    fn model(p: &Path) {
        fs::create_dir_all(p).unwrap();
        fs::write(
            p.join("config.json"),
            r#"{"model_type":"qwen3","quantization":{"bits":4}}"#,
        )
        .unwrap();
        fs::write(p.join("model.safetensors"), b"fixture").unwrap();
        fs::write(p.join("tokenizer.json"), b"{}").unwrap();
    }

    #[test]
    fn unsupported_whisper_npz_snapshots_remain_read_only_inventory_with_exact_sizes() {
        let root = temp();
        for (name, state) in [("small", 768), ("tiny", 384)] {
            let path = root.join(format!(
                "models--mlx-community--whisper-{name}-mlx/snapshots/rev-{name}"
            ));
            fs::create_dir_all(&path).unwrap();
            let config = serde_json::json!({"model_type":"whisper", "n_mels":80, "n_audio_state":state, "n_vocab":51865});
            fs::write(path.join("config.json"), config.to_string()).unwrap();
            let weights = path.join("weights.npz");
            fs::write(&weights, b"synthetic NPZ weights").unwrap();
            let before = fs::metadata(&weights).unwrap().modified().unwrap();
            let report = discover(&root, &[]);
            let entry = report.models.iter().find(|m| m.load_path == path).unwrap();
            assert!(!entry.can_attempt_load());
            assert!(matches!(entry.compatibility, Compatibility::Unsupported(_)));
            assert_eq!(entry.format.as_deref(), Some("npz"));
            assert_eq!(entry.logical_weight_bytes, 21);
            assert_eq!(
                entry.repository.as_deref(),
                Some(format!("mlx-community/whisper-{name}-mlx").as_str())
            );
            assert!(entry.revalidate().is_err());
            assert_eq!(fs::read(&weights).unwrap(), b"synthetic NPZ weights");
            assert_eq!(fs::metadata(&weights).unwrap().modified().unwrap(), before);
            fs::write(path.join("model.safetensors"), b"preferred").unwrap();
            let preferred = inspect_model(&path).unwrap();
            assert_eq!(preferred.format.as_deref(), Some("safetensors"));
            assert_eq!(preferred.logical_weight_bytes, 9);
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn npz_requires_recognized_asr_metadata_and_nonempty_top_level_weights() {
        let root = temp();
        let path = root.join("checkpoint");
        fs::create_dir_all(&path).unwrap();
        fs::write(path.join("weights.npz"), b"synthetic").unwrap();
        for model_type in ["qwen3", "kokoro", "qwen3_forced_aligner", "unknown"] {
            fs::write(
                path.join("config.json"),
                serde_json::json!({"model_type":model_type}).to_string(),
            )
            .unwrap();
            assert!(matches!(
                inspect_model(&path).unwrap().compatibility,
                Compatibility::Unsupported(_)
            ));
        }
        fs::write(path.join("config.json"), r#"{"model_type":"whisper"}"#).unwrap();
        fs::write(path.join("weights.npz"), b"").unwrap();
        assert!(
            matches!(inspect_model(&path).unwrap().compatibility, Compatibility::Incomplete(reason) if reason.contains("Empty NPZ"))
        );
        fs::remove_file(path.join("weights.npz")).unwrap();
        assert!(
            matches!(inspect_model(&path).unwrap().compatibility, Compatibility::Incomplete(reason) if reason.contains("No safetensors or top-level NPZ"))
        );
        fs::create_dir(path.join("assets")).unwrap();
        fs::write(path.join("assets/weights.npz"), b"nested unsupported").unwrap();
        assert!(!inspect_model(&path).unwrap().can_attempt_load());
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(path.join("missing.npz"), path.join("weights.npz")).unwrap();
            assert!(
                matches!(inspect_model(&path).unwrap().compatibility, Compatibility::Incomplete(reason) if reason.contains("broken model symlink"))
            );
        }
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn precedence() {
        let mut r = CacheRoots {
            home: "/home/test".into(),
            ..Default::default()
        };
        assert_eq!(
            r.resolve(),
            PathBuf::from("/home/test/.cache/huggingface/hub")
        );
        r.xdg_cache_home = Some("/xdg".into());
        assert_eq!(r.resolve(), PathBuf::from("/xdg/huggingface/hub"));
        r.hf_home = Some("/hf".into());
        assert_eq!(r.resolve(), PathBuf::from("/hf/hub"));
        r.hf_hub_cache = Some("/hub".into());
        assert_eq!(r.resolve(), PathBuf::from("/hub"));
        r.app_override = Some("/app".into());
        assert_eq!(r.resolve(), PathBuf::from("/app"));
    }
    #[test]
    fn revisions_dedup_and_no_mutation() {
        let root = temp();
        let a = root.join("models--org--model/snapshots/a");
        let b = root.join("models--org--model/snapshots/b");
        model(&a);
        model(&b);
        let before = fs::metadata(a.join("config.json"))
            .unwrap()
            .modified()
            .unwrap();
        let report = discover(&root, std::slice::from_ref(&a));
        assert_eq!(report.models.len(), 2);
        assert_eq!(report.models[0].repository.as_deref(), Some("org/model"));
        assert_eq!(report.models[0].logical_weight_bytes, 7);
        assert_eq!(
            before,
            fs::metadata(a.join("config.json"))
                .unwrap()
                .modified()
                .unwrap()
        );
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn missing_shards_and_unsupported() {
        let root = temp();
        model(&root);
        fs::write(
            root.join("model.safetensors.index.json"),
            r#"{"weight_map":{"x":"absent.safetensors"}}"#,
        )
        .unwrap();
        assert!(matches!(
            inspect_model(&root).unwrap().compatibility,
            Compatibility::Incomplete(_)
        ));
        fs::remove_file(root.join("model.safetensors")).unwrap();
        fs::write(root.join("model.gguf"), b"fake").unwrap();
        assert!(matches!(
            inspect_model(&root).unwrap().compatibility,
            Compatibility::Unsupported(_)
        ));
        fs::remove_dir_all(root).unwrap();
    }
    #[cfg(unix)]
    #[test]
    fn shared_and_broken_links() {
        let root = temp();
        let p = root.join("snapshot");
        model(&p);
        let blob = root.join("blob");
        fs::write(&blob, b"shared").unwrap();
        fs::remove_file(p.join("model.safetensors")).unwrap();
        std::os::unix::fs::symlink(&blob, p.join("model.safetensors")).unwrap();
        assert_eq!(inspect_model(&p).unwrap().logical_weight_bytes, 6);
        fs::remove_file(blob).unwrap();
        assert!(matches!(
            inspect_model(&p).unwrap().compatibility,
            Compatibility::Incomplete(_)
        ));
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn stt_allowlist_matches_exact_repositories_and_rejects_other_variants() {
        for repository in [QWEN_ASR_REPOSITORY, NEMOTRON_ASR_REPOSITORY] {
            for identity in [
                repository.to_string(),
                repository.to_uppercase(),
                format!("{repository}@revision"),
            ] {
                assert_eq!(supported_asr_repository(&identity), Some(repository));
            }
        }
        for rejected in [
            "mlx-community/whisper-tiny-mlx",
            "mlx-community/parakeet-tdt-0.6b-v3",
            "mlx-community/Voxtral-Mini-4B-Realtime-2602-4bit",
            "mlx-community/Qwen3-ASR-0.6B-4bit",
            "mlx-community/Qwen3-ASR-1.7B-5bit",
            "mlx-community/nemotron-3.5-asr-streaming",
            "other/Qwen3-ASR-0.6B-5bit",
            "mlx-community/Qwen3-ASR-0.6B-5bit-extra",
            "mlx-community/Qwen3-ASR-0.6B-5bit@",
            "alias:qwen3-asr",
            "qwen3_asr",
            "nemotron_asr",
        ] {
            assert!(supported_asr_repository(rejected).is_none(), "{rejected}");
        }
        let root = temp();
        for (repository, family, supported) in [
            (QWEN_ASR_REPOSITORY, "qwen3_asr", true),
            (NEMOTRON_ASR_REPOSITORY, "nemotron_asr", true),
            ("mlx-community/Qwen3-ASR-1.7B-5bit", "qwen3_asr", false),
            (
                "mlx-community/Voxtral-Mini-4B-Realtime-2602-4bit",
                "voxtral_realtime",
                false,
            ),
        ] {
            let path = root.join(format!(
                "models--{}/snapshots/revision",
                repository.replace('/', "--")
            ));
            fs::create_dir_all(&path).unwrap();
            fs::write(
                path.join("config.json"),
                serde_json::json!({"model_type":family}).to_string(),
            )
            .unwrap();
            fs::write(path.join("model.safetensors"), b"unchanged").unwrap();
            assert_eq!(local_asr_repository(&path).unwrap().is_some(), supported);
            assert_eq!(inspect_model(&path).unwrap().can_attempt_load(), supported);
            assert_eq!(
                fs::read(path.join("model.safetensors")).unwrap(),
                b"unchanged"
            );
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn catalogs_keep_asr_separate_from_tts() {
        let models =
            parse_available_catalog(include_bytes!("../../../fixtures/runtime/available.json"))
                .unwrap();
        assert_eq!(models.iter().filter(|m| m.is_asr()).count(), 2);
        assert_eq!(models.iter().filter(|m| m.is_chat()).count(), 1);
        assert!(
            models
                .iter()
                .any(|model| model.kind.as_deref() == Some("tts"))
        );
        assert!(
            !models
                .iter()
                .any(|model| model.hf_path == "fixture/asr" || model.hf_path == "fixture/aligner")
        );
        let cached =
            parse_cached_catalog(include_bytes!("../../../fixtures/runtime/cached.json")).unwrap();
        assert!(cached.iter().any(|m| m.state == "incomplete"));
    }
    #[test]
    fn bounded_metadata_and_unknown_local_model_survive_missing_cache() {
        let root = temp();
        let local = root.join("custom");
        model(&local);
        fs::write(local.join("config.json"), "{}").unwrap();
        let report = discover(&root.join("missing-cache"), std::slice::from_ref(&local));
        assert_eq!(report.models.len(), 1);
        assert_eq!(report.errors.len(), 1);
        assert!(matches!(
            report.models[0].compatibility,
            Compatibility::Unknown(_)
        ));
        fs::write(
            local.join("config.json"),
            vec![b' '; MAX_METADATA as usize + 1],
        )
        .unwrap();
        assert!(matches!(
            inspect_model(&local).unwrap().compatibility,
            Compatibility::Incomplete(_)
        ));
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn nested_variants_keep_snapshot_identity_and_isolate_missing_sibling_shards() {
        let root = temp();
        let snapshot = root.join("models--org--variants/snapshots/revision-a");
        let four = snapshot.join("checkpoints/4bit");
        let eight = snapshot.join("checkpoints/8bit");
        model(&four);
        model(&eight);
        fs::write(
            eight.join("config.json"),
            r#"{"model_type":"qwen3","quantization":{"bits":8}}"#,
        )
        .unwrap();
        let partial = snapshot.join("checkpoints/partial");
        model(&partial);
        fs::write(
            partial.join("model.safetensors.index.json"),
            r#"{"weight_map":{"x":"missing.safetensors"}}"#,
        )
        .unwrap();
        let report = discover(&root, std::slice::from_ref(&four));
        assert!(report.errors.is_empty());
        assert_eq!(report.models.len(), 3);
        for entry in &report.models {
            assert_eq!(entry.repository.as_deref(), Some("org/variants"));
            assert_eq!(entry.revision.as_deref(), Some("revision-a"));
            assert_eq!(entry.cache_root.as_deref(), Some(root.as_path()));
        }
        for (path, bits) in [(&four, 4), (&eight, 8)] {
            let entry = report.models.iter().find(|m| &m.load_path == path).unwrap();
            assert!(entry.can_attempt_load());
            assert_eq!(entry.logical_weight_bytes, 7);
            assert_eq!(entry.quantization.as_ref().unwrap()["bits"], bits);
        }
        assert!(matches!(
            report
                .models
                .iter()
                .find(|m| m.load_path == partial)
                .unwrap()
                .compatibility,
            Compatibility::Incomplete(_)
        ));
        assert_eq!(
            fs::read(four.join("model.safetensors")).unwrap(),
            b"fixture"
        );
        fs::remove_dir_all(root).unwrap();
    }
    #[cfg(unix)]
    #[test]
    fn nested_scan_does_not_follow_directory_cycles_and_bounds_depth() {
        let root = temp();
        let snapshot = root.join("models--org--nested/snapshots/r");
        let good = snapshot.join("4bit");
        model(&good);
        std::os::unix::fs::symlink(&snapshot, snapshot.join("cycle")).unwrap();
        let too_deep = snapshot.join("a/b/c/d/e/f/g/h/i");
        model(&too_deep);
        let report = discover(&root, &[]);
        assert_eq!(report.models.len(), 1);
        assert_eq!(report.models[0].load_path, good);
        assert!(
            report
                .errors
                .iter()
                .any(|error| error.contains("eight levels"))
        );
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn nested_checkpoint_failure_does_not_poison_parent_weights() {
        let root = temp();
        let snapshot = root.join("models--org--nested/snapshots/r");
        model(&snapshot);
        let child = snapshot.join("other");
        model(&child);
        fs::write(
            child.join("model.safetensors.index.json"),
            r#"{"weight_map":{"x":"absent.safetensors"}}"#,
        )
        .unwrap();
        let report = discover(&root, &[]);
        assert_eq!(report.models.len(), 2);
        assert!(
            report
                .models
                .iter()
                .find(|m| m.load_path == snapshot)
                .unwrap()
                .can_attempt_load()
        );
        assert!(matches!(
            report
                .models
                .iter()
                .find(|m| m.load_path == child)
                .unwrap()
                .compatibility,
            Compatibility::Incomplete(_)
        ));
        fs::remove_dir_all(root).unwrap();
    }
    #[cfg(unix)]
    #[test]
    fn unreadable_snapshot_preserves_readable_sibling_and_metadata() {
        use std::os::unix::fs::PermissionsExt;
        struct RestorePermissions {
            path: PathBuf,
            permissions: fs::Permissions,
        }
        impl Drop for RestorePermissions {
            fn drop(&mut self) {
                let _ = fs::set_permissions(&self.path, self.permissions.clone());
            }
        }
        let root = temp();
        let readable = root.join("models--org--readable/snapshots/good");
        let blocked = root.join("models--org--blocked/snapshots/denied");
        model(&readable);
        model(&blocked);
        let config = readable.join("config.json");
        let config_bytes = fs::read(&config).unwrap();
        let config_metadata = fs::metadata(&config).unwrap();
        let weight = readable.join("model.safetensors");
        let weight_metadata = fs::metadata(&weight).unwrap();
        let restore = RestorePermissions {
            path: blocked.clone(),
            permissions: fs::metadata(&blocked).unwrap().permissions(),
        };
        fs::set_permissions(&blocked, fs::Permissions::from_mode(0o000)).unwrap();
        let blocked_metadata = fs::metadata(&blocked).unwrap();
        let denial = fs::read_dir(&blocked).err().map(|error| error.kind());
        let report = discover(&root, &[]);
        let blocked_after = fs::metadata(&blocked).unwrap();
        // Restore even if later assertions fail, before temporary-tree teardown.
        drop(restore);
        let sibling = report
            .models
            .iter()
            .find(|model| model.load_path == readable)
            .unwrap();
        assert!(sibling.can_attempt_load());
        assert_eq!(sibling.repository.as_deref(), Some("org/readable"));
        assert_eq!(sibling.revision.as_deref(), Some("good"));
        if let Some(kind) = denial {
            assert_eq!(kind, std::io::ErrorKind::PermissionDenied);
            assert!(
                report
                    .errors
                    .iter()
                    .any(|error| error.contains(&blocked.display().to_string()))
            );
            assert!(
                !report
                    .models
                    .iter()
                    .any(|model| model.load_path == blocked && model.can_attempt_load())
            );
        } else {
            eprintln!(
                "chmod000 is bypassed by this test user; permission-denial assertions unavailable, sibling/read-only assertions still exercised"
            );
        }
        assert_eq!(blocked_after.permissions().mode() & 0o777, 0);
        assert_eq!(
            blocked_metadata.modified().unwrap(),
            blocked_after.modified().unwrap()
        );
        assert_eq!(fs::read(&config).unwrap(), config_bytes);
        let config_after = fs::metadata(&config).unwrap();
        assert_eq!(
            config_metadata.modified().unwrap(),
            config_after.modified().unwrap()
        );
        assert_eq!(
            config_metadata.permissions().mode(),
            config_after.permissions().mode()
        );
        assert_eq!(fs::read(&weight).unwrap(), b"fixture");
        assert_eq!(
            weight_metadata.modified().unwrap(),
            fs::metadata(&weight).unwrap().modified().unwrap()
        );
        assert!(fs::read_dir(&blocked).is_ok());
        fs::remove_dir_all(root).unwrap();
    }
}
