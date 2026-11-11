//! Explicitly granted built-in tools. Command grants are execution authority, not an OS sandbox.
use crate::chat::ToolCall;
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Component, Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::{io::AsyncReadExt, process::Command};
use tokio_util::sync::CancellationToken;

#[derive(Debug)]
struct Denied(&'static str);
impl std::fmt::Display for Denied {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}
impl std::error::Error for Denied {}

pub const DEFAULT_OUTPUT_LIMIT: usize = 1024 * 1024;
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandGrant {
    /// Absolute path; canonical identity is pinned when constructing the executor.
    pub executable: PathBuf,
    /// Exact accepted argument prefix; remaining arguments require allow_extra_args.
    pub args_prefix: Vec<String>,
    pub allow_extra_args: bool,
    /// Set false only for a command/argument combination the user trusts as read-only.
    pub mutates: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolPolicy {
    pub enabled: BTreeSet<String>,
    pub roots: Vec<PathBuf>,
    pub working_dir: PathBuf,
    pub allow_mutations: bool,
    pub commands: Vec<CommandGrant>,
    pub timeout_ms: u64,
    pub output_limit: usize,
}
impl Default for ToolPolicy {
    fn default() -> Self {
        Self {
            enabled: BTreeSet::new(),
            roots: vec![],
            working_dir: PathBuf::new(),
            allow_mutations: false,
            commands: vec![],
            timeout_ms: 60_000,
            output_limit: DEFAULT_OUTPUT_LIMIT,
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ToolStatus {
    Completed,
    Denied,
    Failed,
    Cancelled,
    TimedOut,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResult {
    pub call_id: String,
    pub name: String,
    pub status: ToolStatus,
    pub output: Value,
    pub error: Option<String>,
    pub elapsed_ms: u64,
    /// Cancellation/failure does not roll back writes or command side effects.
    pub side_effects_possible: bool,
}
impl ToolResult {
    pub fn message(&self) -> Value {
        json!({"role": "tool", "tool_call_id": self.call_id, "content": serde_json::to_string(self).unwrap_or_default()})
    }
}
#[derive(Clone)]
pub struct ToolExecutor {
    policy: ToolPolicy,
    roots: Vec<(PathBuf, Arc<std::fs::File>)>,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PathArgs {
    path: String,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct WriteArgs {
    path: String,
    content: String,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SearchArgs {
    path: String,
    query: String,
    #[serde(default)]
    filenames_only: bool,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CommandArgs {
    executable: PathBuf,
    #[serde(default)]
    args: Vec<String>,
    cwd: PathBuf,
}

impl ToolExecutor {
    pub fn new(mut policy: ToolPolicy) -> Result<Self> {
        ensure!(
            policy.output_limit > 0 && policy.output_limit <= 16 * DEFAULT_OUTPUT_LIMIT,
            "Output limit must be 1 byte through 16 MiB"
        );
        ensure!(
            policy.timeout_ms > 0 && policy.timeout_ms <= 600_000,
            "Timeout must be 1 ms through 10 minutes"
        );
        for root in &mut policy.roots {
            *root = root.canonicalize().context("Cannot resolve tool root")?;
            ensure!(root.is_dir(), "Tool root must be a directory");
        }
        policy.working_dir = policy
            .working_dir
            .canonicalize()
            .context("Cannot resolve working directory")?;
        ensure!(
            policy.working_dir.is_dir()
                && policy
                    .roots
                    .iter()
                    .any(|r| policy.working_dir.starts_with(r)),
            "Working directory outside roots"
        );
        for grant in &mut policy.commands {
            ensure!(
                grant.executable.is_absolute(),
                "Command executable must be absolute"
            );
            grant.executable = grant.executable.canonicalize()?;
            ensure!(
                !is_elevation(&grant.executable),
                "Elevation helpers cannot be granted"
            );
        }
        let roots = policy
            .roots
            .iter()
            .map(|path| Ok((path.clone(), Arc::new(std::fs::File::open(path)?))))
            .collect::<Result<_>>()?;
        Ok(Self { policy, roots })
    }
    pub fn policy(&self) -> &ToolPolicy {
        &self.policy
    }
    pub fn definitions(&self) -> Vec<Value> {
        definitions()
            .into_iter()
            .filter(|d| {
                d.pointer("/function/name")
                    .and_then(Value::as_str)
                    .is_some_and(|n| self.policy.enabled.contains(n))
            })
            .collect()
    }
    fn path(&self, input: &Path, create: bool) -> Result<PathBuf> {
        ensure!(!input.as_os_str().is_empty(), "Path cannot be empty");
        let joined = if input.is_absolute() {
            input.to_path_buf()
        } else {
            self.policy.working_dir.join(input)
        };
        // Parent traversal is unnecessary for basic tools and complicates missing-path resolution.
        ensure!(
            !joined
                .components()
                .any(|c| matches!(c, Component::ParentDir)),
            Denied("Parent traversal is denied")
        );
        let resolved = if create && !joined.exists() {
            // A dangling final symlink must never be treated as a new file.
            ensure!(
                std::fs::symlink_metadata(&joined).is_err(),
                Denied("Dangling symlink target denied")
            );
            let parent = joined.parent().context("Missing parent")?.canonicalize()?;
            parent.join(joined.file_name().context("Missing file name")?)
        } else {
            joined.canonicalize().context("Cannot resolve tool path")?
        };
        ensure!(
            self.policy.roots.iter().any(|r| resolved.starts_with(r)),
            Denied("Path resolves outside granted roots")
        );
        Ok(resolved)
    }
    fn open(&self, path: &Path, mode: FileMode) -> Result<std::fs::File> {
        let (root, handle) = self
            .roots
            .iter()
            .find(|(root, _)| path.starts_with(root))
            .context("Path outside roots")?;
        #[cfg(unix)]
        {
            anchored::open(handle, path.strip_prefix(root)?, mode)
        }
        #[cfg(not(unix))]
        {
            let _ = (root, handle, mode);
            bail!("Scoped tools require Unix descriptor-relative IO")
        }
    }
    fn entries(&self, path: &Path) -> Result<Vec<PathBuf>> {
        let directory = self.open(path, FileMode::Directory)?;
        #[cfg(unix)]
        {
            Ok(anchored::entries(directory)?
                .into_iter()
                .map(|name| path.join(name))
                .collect())
        }
        #[cfg(not(unix))]
        {
            let _ = directory;
            bail!("Scoped directory tools unavailable")
        }
    }
    /// Caller must durably record intent before invoking. Calls execute once; never auto-retried.
    pub async fn execute(&self, call: ToolCall, cancel: CancellationToken) -> ToolResult {
        let start = Instant::now();
        let mut result = ToolResult {
            call_id: call.id.clone(),
            name: call.name.clone(),
            status: ToolStatus::Failed,
            output: Value::Null,
            error: None,
            elapsed_ms: 0,
            side_effects_possible: false,
        };
        if call.id.is_empty() || !self.policy.enabled.contains(&call.name) {
            result.status = ToolStatus::Denied;
            result.error = Some("Tool disabled or missing call identity".into());
            return result;
        }
        if cancel.is_cancelled() {
            result.status = ToolStatus::Cancelled;
            return result;
        }
        if call.name == "os_command" {
            match self.command(&call.arguments, cancel.clone()).await {
                Ok((status, output, effects)) => {
                    result.status = status;
                    result.output = output;
                    result.side_effects_possible = effects;
                }
                Err(error) => {
                    result.status = if cancel.is_cancelled() {
                        ToolStatus::Cancelled
                    } else if error.downcast_ref::<Denied>().is_some() {
                        ToolStatus::Denied
                    } else {
                        ToolStatus::Failed
                    };
                    result.error = Some(error.to_string());
                }
            }
        } else {
            let executor = self.clone();
            let name = call.name.clone();
            // File work is bounded and off the UI thread. Await completion even on Stop:
            // dropping a blocking worker could otherwise report cancellation before a write occurs.
            let token = cancel.clone();
            match tokio::task::spawn_blocking(move || {
                executor.filesystem(&name, call.arguments, &token)
            })
            .await
            {
                Ok(Ok(output)) => {
                    result.status = ToolStatus::Completed;
                    result.output = output;
                    result.side_effects_possible =
                        matches!(call.name.as_str(), "os_write" | "os_create");
                }
                Ok(Err(error)) => {
                    result.status = if cancel.is_cancelled() {
                        ToolStatus::Cancelled
                    } else if error.downcast_ref::<Denied>().is_some() {
                        ToolStatus::Denied
                    } else {
                        ToolStatus::Failed
                    };
                    result.error = Some(error.to_string());
                    result.side_effects_possible =
                        matches!(call.name.as_str(), "os_write" | "os_create")
                            && self.policy.allow_mutations
                            && result.status != ToolStatus::Denied;
                }
                Err(_) => {
                    result.error = Some("Filesystem worker failed".into());
                    result.side_effects_possible =
                        matches!(call.name.as_str(), "os_write" | "os_create");
                }
            }
        }
        result.elapsed_ms = start.elapsed().as_millis() as u64;
        result
    }
    fn filesystem(&self, name: &str, args: Value, cancel: &CancellationToken) -> Result<Value> {
        ensure!(!cancel.is_cancelled(), "Cancelled");
        match name {
            "os_read" => {
                use std::io::Read;
                let args: PathArgs = serde_json::from_value(args)?;
                let path = self.path(Path::new(&args.path), false)?;
                let mut data = vec![];
                self.open(&path, FileMode::Read)?
                    .take(self.policy.output_limit as u64 + 1)
                    .read_to_end(&mut data)?;
                let truncated = data.len() > self.policy.output_limit;
                data.truncate(self.policy.output_limit);
                Ok(
                    json!({"path": path.to_string_lossy(), "content": String::from_utf8_lossy(&data), "truncated": truncated}),
                )
            }
            "os_list" => {
                let args: PathArgs = serde_json::from_value(args)?;
                let path = self.path(Path::new(&args.path), false)?;
                let mut entries = vec![];
                let mut size = 0;
                let children = self.entries(&path)?;
                let mut truncated = children.len() > 10_000;
                for entry in children.into_iter().take(10_000) {
                    ensure!(!cancel.is_cancelled(), "Cancelled");
                    let item = json!({"path": entry.to_string_lossy(), "path_lossy": entry.to_str().is_none()});
                    size += serde_json::to_vec(&item)?.len();
                    if size > self.policy.output_limit {
                        truncated = true;
                        break;
                    }
                    entries.push(item);
                }
                Ok(
                    json!({"path": path.to_string_lossy(), "entries": entries, "truncated": truncated}),
                )
            }
            "os_write" | "os_create" => {
                use std::io::Write;
                ensure!(
                    self.policy.allow_mutations,
                    Denied("File mutations are denied by policy")
                );
                let args: WriteArgs = serde_json::from_value(args)?;
                ensure!(
                    args.content.len() <= self.policy.output_limit,
                    "Write content exceeds limit"
                );
                let path = self.path(Path::new(&args.path), true)?;
                ensure!(!cancel.is_cancelled(), "Cancelled");
                let mut file = self.open(
                    &path,
                    if name == "os_create" {
                        FileMode::Create
                    } else {
                        FileMode::Write
                    },
                )?;
                file.write_all(args.content.as_bytes())?;
                file.sync_all()?;
                Ok(
                    json!({"path": path.to_string_lossy(), "bytes_written": args.content.len(), "truncated": false}),
                )
            }
            "os_search" => {
                let args: SearchArgs = serde_json::from_value(args)?;
                ensure!(
                    !args.query.is_empty() && args.query.len() <= 4096,
                    "Search query must be 1..4096 bytes"
                );
                let path = self.path(Path::new(&args.path), false)?;
                let mut stack = vec![path.clone()];
                let mut matches = vec![];
                let mut size = 0;
                let mut visited = 0;
                let mut truncated = false;
                let deadline = Instant::now() + Duration::from_millis(self.policy.timeout_ms);
                while let Some(current) = stack.pop() {
                    ensure!(!cancel.is_cancelled(), "Cancelled");
                    if visited >= 10_000 || Instant::now() >= deadline {
                        truncated = true;
                        break;
                    }
                    visited += 1;
                    // Do not follow directory or file symlinks during recursive search.
                    let meta = std::fs::symlink_metadata(&current)?;
                    if meta.file_type().is_symlink() {
                        continue;
                    }
                    if meta.is_dir() {
                        for entry in self.entries(&current)? {
                            if stack.len() >= 10_000 {
                                truncated = true;
                                break;
                            }
                            stack.push(entry);
                        }
                    } else if meta.is_file() {
                        let matched = if args.filenames_only {
                            current
                                .file_name()
                                .is_some_and(|n| n.to_string_lossy().contains(&args.query))
                        } else {
                            use std::io::Read;
                            let checked = self.path(&current, false)?;
                            let mut data = vec![];
                            self.open(&checked, FileMode::Read)?
                                .take(self.policy.output_limit as u64 + 1)
                                .read_to_end(&mut data)?;
                            if data.len() > self.policy.output_limit {
                                truncated = true;
                                data.truncate(self.policy.output_limit);
                            }
                            String::from_utf8_lossy(&data).contains(&args.query)
                        };
                        if matched {
                            let item = json!({"path": current.to_string_lossy(), "path_lossy": current.to_str().is_none()});
                            size += serde_json::to_vec(&item)?.len();
                            if size > self.policy.output_limit {
                                truncated = true;
                                break;
                            }
                            matches.push(item);
                        }
                    }
                }
                Ok(
                    json!({"path": path.to_string_lossy(), "matches": matches, "truncated": truncated, "visited": visited}),
                )
            }
            _ => bail!("Unknown OS tool"),
        }
    }
    async fn command(
        &self,
        args: &Value,
        cancel: CancellationToken,
    ) -> Result<(ToolStatus, Value, bool)> {
        let args: CommandArgs = serde_json::from_value(args.clone())?;
        ensure!(args.executable.is_absolute(), "Executable must be absolute");
        let executable = args
            .executable
            .canonicalize()
            .context("Executable not found")?;
        ensure!(
            !is_elevation(&executable),
            Denied("Elevation is not supported")
        );
        let cwd = self.path(&args.cwd, false)?;
        ensure!(cwd.is_dir(), "Command cwd must be a directory");
        let grant = self
            .policy
            .commands
            .iter()
            .find(|g| {
                g.executable == executable
                    && args.args.starts_with(&g.args_prefix)
                    && (g.allow_extra_args || args.args.len() == g.args_prefix.len())
            })
            .ok_or_else(|| anyhow::anyhow!(Denied("Command/arguments are not allowed")))?;
        ensure!(
            !grant.mutates || self.policy.allow_mutations,
            Denied("Command mutations are denied")
        );
        ensure!(!cancel.is_cancelled(), "Cancelled");
        let mut command = Command::new(&executable);
        command
            .args(&args.args)
            .current_dir(&cwd)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
        // Private process group permits cleanup of descendants without touching external processes.
        #[cfg(unix)]
        {
            command.process_group(0);
        }
        let mut child = command.spawn().context("Cannot start allowed command")?;
        let pid = child.id();
        let remaining = Arc::new(AtomicUsize::new(self.policy.output_limit));
        let stdout = child.stdout.take().context("Missing stdout")?;
        let stderr = child.stderr.take().context("Missing stderr")?;
        let stdout_task = tokio::spawn(capture(stdout, remaining.clone()));
        let stderr_task = tokio::spawn(capture(stderr, remaining));
        let (status, exit) = tokio::select! {
            biased;
            _ = cancel.cancelled() => (ToolStatus::Cancelled, None),
            _ = tokio::time::sleep(Duration::from_millis(self.policy.timeout_ms)) => (ToolStatus::TimedOut, None),
            result = child.wait() => match result { Ok(exit) => (if exit.success() {ToolStatus::Completed} else {ToolStatus::Failed}, exit.code()), Err(_) => (ToolStatus::Failed, None) },
        };
        // Also clean descendants left behind by an exited command, which may hold pipes open.
        #[cfg(unix)]
        if let Some(pid) = pid {
            let _ = Command::new("/bin/kill")
                .arg("-KILL")
                .arg(format!("-{pid}"))
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .await;
        }
        if child.try_wait()?.is_none() {
            let _ = child.kill().await;
        }
        let _ = child.wait().await;
        let (stdout, out_cut) = join_capture(stdout_task).await;
        let (stderr, err_cut) = join_capture(stderr_task).await;
        Ok((
            status,
            json!({"executable": executable.to_string_lossy(), "args": args.args, "cwd": cwd.to_string_lossy(), "exit_code": exit, "stdout": stdout, "stderr": stderr, "truncated": out_cut || err_cut}),
            grant.mutates,
        ))
    }
}
fn is_elevation(path: &Path) -> bool {
    path.file_name()
        .is_some_and(|n| matches!(n.to_str(), Some("sudo" | "doas" | "su" | "pkexec")))
}
async fn capture(
    mut reader: impl tokio::io::AsyncRead + Unpin,
    remaining: Arc<AtomicUsize>,
) -> (String, bool) {
    let mut output = vec![];
    let mut truncated = false;
    let mut buffer = [0u8; 8192];
    loop {
        match reader.read(&mut buffer).await {
            Ok(0) => break,
            Ok(size) => {
                let old = remaining
                    .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| {
                        Some(n.saturating_sub(size))
                    })
                    .unwrap_or(0);
                let keep = old.min(size);
                output.extend_from_slice(&buffer[..keep]);
                truncated |= keep < size;
            }
            Err(_) => {
                truncated = true;
                break;
            }
        }
    }
    let mut text = String::from_utf8_lossy(&output).into_owned();
    if text.len() > output.len() {
        let mut end = output.len();
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
        truncated = true;
    }
    (text, truncated)
}
async fn join_capture(mut task: tokio::task::JoinHandle<(String, bool)>) -> (String, bool) {
    match tokio::time::timeout(Duration::from_secs(2), &mut task).await {
        Ok(Ok(output)) => output,
        _ => {
            task.abort();
            (String::new(), true)
        }
    }
}
pub fn definitions() -> Vec<Value> {
    let mut definitions = BTreeMap::new();
    for (name, description) in [
        ("os_list", "List a granted directory"),
        ("os_read", "Read bounded file content"),
    ] {
        definitions.insert(
            name,
            (
                description,
                json!({"path": {"type":"string"}}),
                vec!["path"],
            ),
        );
    }
    for (name, description) in [
        ("os_create", "Create a new file; never overwrite"),
        ("os_write", "Write a file under the mutation policy"),
    ] {
        definitions.insert(
            name,
            (
                description,
                json!({"path":{"type":"string"},"content":{"type":"string"}}),
                vec!["path", "content"],
            ),
        );
    }
    definitions.insert("os_search", ("Search filenames or bounded file text without following symlinks", json!({"path":{"type":"string"},"query":{"type":"string"},"filenames_only":{"type":"boolean"}}), vec!["path", "query"]));
    definitions.insert("os_command", ("Execute an explicitly allowed command with bounded time and output", json!({"executable":{"type":"string"},"args":{"type":"array","items":{"type":"string"}},"cwd":{"type":"string"}}), vec!["executable", "cwd"]));
    definitions.into_iter().map(|(name,(description,properties,required))| json!({"type":"function","function":{"name":name,"description":description,"parameters":{"type":"object","properties":properties,"required":required,"additionalProperties":false}}})).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    fn executor(root: &Path, mutation: bool) -> ToolExecutor {
        ToolExecutor::new(ToolPolicy {
            roots: vec![root.to_path_buf()],
            working_dir: root.to_path_buf(),
            allow_mutations: mutation,
            enabled: [
                "os_read",
                "os_list",
                "os_search",
                "os_write",
                "os_create",
                "os_command",
            ]
            .into_iter()
            .map(str::to_owned)
            .collect(),
            ..ToolPolicy::default()
        })
        .unwrap()
    }
    fn call(name: &str, arguments: Value) -> ToolCall {
        ToolCall {
            id: "call-test".into(),
            name: name.into(),
            arguments,
        }
    }
    #[tokio::test]
    async fn read_write_create_search_and_bounds() {
        let root = tempfile::tempdir().unwrap();
        let mut executor = executor(root.path(), true);
        let created = executor
            .execute(
                call(
                    "os_create",
                    json!({"path":"test.txt","content":"hello world"}),
                ),
                CancellationToken::new(),
            )
            .await;
        assert_eq!(created.status, ToolStatus::Completed);
        assert_eq!(created.call_id, "call-test");
        assert_eq!(
            executor
                .execute(
                    call(
                        "os_create",
                        json!({"path":"test.txt","content":"overwrite"})
                    ),
                    CancellationToken::new()
                )
                .await
                .status,
            ToolStatus::Failed
        );
        let found = executor
            .execute(
                call("os_search", json!({"path":".","query":"hello"})),
                CancellationToken::new(),
            )
            .await;
        assert_eq!(found.output["matches"].as_array().unwrap().len(), 1);
        executor.policy.output_limit = 5;
        let read = executor
            .execute(
                call("os_read", json!({"path":"test.txt"})),
                CancellationToken::new(),
            )
            .await;
        assert_eq!(read.output["content"], "hello");
        assert_eq!(read.output["truncated"], true);
        let denied = executor
            .execute(
                call("os_read", json!({"path":"../outside"})),
                CancellationToken::new(),
            )
            .await;
        assert_ne!(denied.status, ToolStatus::Completed);
    }
    #[tokio::test]
    async fn strict_schema_and_mutation_policy() {
        let root = tempfile::tempdir().unwrap();
        let executor = executor(root.path(), false);
        for args in [
            json!({"path":"file","content":"text"}),
            json!({"path":"file","content":"text","override_policy":true}),
        ] {
            assert_ne!(
                executor
                    .execute(call("os_write", args), CancellationToken::new())
                    .await
                    .status,
                ToolStatus::Completed
            );
        }
        assert!(!root.path().join("file").exists());
        let mut disabled = executor.clone();
        disabled.policy.enabled.clear();
        assert_eq!(
            disabled
                .execute(
                    call("os_read", json!({"path":"file"})),
                    CancellationToken::new()
                )
                .await
                .status,
            ToolStatus::Denied
        );
    }
    #[cfg(unix)]
    #[tokio::test]
    async fn symlink_escape_and_dangling_write_are_denied() {
        use std::os::unix::fs::symlink;
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("secret"), "private").unwrap();
        symlink(outside.path(), root.path().join("escape")).unwrap();
        symlink(outside.path().join("missing"), root.path().join("dangling")).unwrap();
        let executor = executor(root.path(), true);
        for (name, args) in [
            ("os_read", json!({"path":"escape/secret"})),
            ("os_write", json!({"path":"escape/new","content":"bad"})),
            ("os_write", json!({"path":"dangling","content":"bad"})),
        ] {
            assert_ne!(
                executor
                    .execute(call(name, args), CancellationToken::new())
                    .await
                    .status,
                ToolStatus::Completed
            );
        }
        assert!(!outside.path().join("new").exists());
        assert!(!outside.path().join("missing").exists());
    }
    #[cfg(unix)]
    fn command_executor(root: &Path, executable: &str, args: Vec<String>) -> ToolExecutor {
        let mut executor = executor(root, false);
        executor.policy.commands = vec![CommandGrant {
            executable: PathBuf::from(executable).canonicalize().unwrap(),
            args_prefix: args,
            allow_extra_args: false,
            mutates: false,
        }];
        executor
    }
    #[cfg(unix)]
    #[tokio::test]
    async fn commands_are_exact_bounded_and_return_exit_status() {
        let root = tempfile::tempdir().unwrap();
        let args = vec!["abcdef".into()];
        let mut executor = command_executor(root.path(), "/bin/echo", args.clone());
        executor.policy.output_limit = 4;
        let result = executor
            .execute(
                call(
                    "os_command",
                    json!({"executable":"/bin/echo","args":args,"cwd":"."}),
                ),
                CancellationToken::new(),
            )
            .await;
        assert_eq!(result.status, ToolStatus::Completed);
        assert_eq!(result.output["stdout"], "abcd");
        assert_eq!(result.output["exit_code"], 0);
        assert_eq!(result.output["truncated"], true);
        assert_eq!(
            executor
                .execute(
                    call(
                        "os_command",
                        json!({"executable":"/bin/echo","args":["other"],"cwd":"."})
                    ),
                    CancellationToken::new()
                )
                .await
                .status,
            ToolStatus::Denied
        );
    }
    #[cfg(unix)]
    #[tokio::test]
    async fn failing_command_retains_exit_and_combines_output_budget() {
        let root = tempfile::tempdir().unwrap();
        let args = vec![
            "-c".to_owned(),
            "printf abcdef; printf ghijkl >&2; exit 7".to_owned(),
        ];
        let mut executor = command_executor(root.path(), "/bin/sh", args.clone());
        executor.policy.output_limit = 5;
        let result = executor
            .execute(
                call(
                    "os_command",
                    json!({"executable":"/bin/sh","args":args,"cwd":"."}),
                ),
                CancellationToken::new(),
            )
            .await;
        assert_eq!(result.status, ToolStatus::Failed);
        assert_eq!(result.output["exit_code"], 7);
        assert_eq!(result.output["truncated"], true);
        assert!(
            result.output["stdout"].as_str().unwrap().len()
                + result.output["stderr"].as_str().unwrap().len()
                <= 5
        );
    }
    #[cfg(unix)]
    #[tokio::test]
    async fn command_timeout_and_cancel_reap_owned_process() {
        let root = tempfile::tempdir().unwrap();
        let mut executor = command_executor(root.path(), "/bin/sleep", vec!["10".into()]);
        executor.policy.timeout_ms = 30;
        let args = json!({"executable":"/bin/sleep","args":["10"],"cwd":"."});
        let timed = executor
            .execute(call("os_command", args.clone()), CancellationToken::new())
            .await;
        assert_eq!(timed.status, ToolStatus::TimedOut);
        executor.policy.timeout_ms = 10000;
        let cancel = CancellationToken::new();
        let token = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(30)).await;
            token.cancel();
        });
        let cancelled = executor.execute(call("os_command", args), cancel).await;
        assert_eq!(cancelled.status, ToolStatus::Cancelled);
        assert!(cancelled.elapsed_ms < 2000);
    }
    #[cfg(unix)]
    #[tokio::test]
    async fn command_timeout_cleans_descendants_and_retains_output() {
        let root = tempfile::tempdir().unwrap();
        // A shell runs only because this exact executable and argument vector is granted.
        let args = vec!["-c".to_owned(), "/bin/sleep 10 & echo $!; wait".to_owned()];
        let mut executor = command_executor(root.path(), "/bin/sh", args.clone());
        executor.policy.timeout_ms = 100;
        let result = executor
            .execute(
                call(
                    "os_command",
                    json!({"executable":"/bin/sh","args":args,"cwd":"."}),
                ),
                CancellationToken::new(),
            )
            .await;
        assert_eq!(result.status, ToolStatus::TimedOut);
        let pid = result.output["stdout"]
            .as_str()
            .unwrap()
            .trim()
            .parse::<u32>()
            .unwrap();
        let mut gone = false;
        for _ in 0..20 {
            let status = Command::new("/bin/kill")
                .arg("-0")
                .arg(pid.to_string())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .await
                .unwrap();
            if !status.success() {
                gone = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(gone, "Owned descendant survived timeout");
    }
    #[cfg(unix)]
    #[tokio::test]
    async fn directory_handles_are_fresh_and_ancestor_swaps_cannot_escape() {
        use std::os::unix::fs::symlink;
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("inside"), "safe").unwrap();
        std::fs::write(outside.path().join("secret"), "private").unwrap();
        symlink(outside.path(), root.path().join("swapped")).unwrap();
        let executor = executor(root.path(), true);
        for _ in 0..2 {
            let listed = executor
                .execute(
                    call("os_list", json!({"path":"."})),
                    CancellationToken::new(),
                )
                .await;
            assert_eq!(listed.status, ToolStatus::Completed);
            assert_eq!(listed.output["entries"].as_array().unwrap().len(), 2);
        }
        // Simulates a canonical path whose ancestor became a symlink after policy validation.
        assert!(
            executor
                .open(&root.path().join("swapped/secret"), FileMode::Read)
                .is_err()
        );
        assert!(
            executor
                .open(&root.path().join("swapped/new"), FileMode::Write)
                .is_err()
        );
        assert!(!outside.path().join("new").exists());
    }
    #[test]
    fn definitions_are_namespaced_and_disabled_by_default() {
        assert!(ToolPolicy::default().enabled.is_empty());
        assert_eq!(definitions().len(), 6);
        for definition in definitions() {
            assert!(
                definition["function"]["name"]
                    .as_str()
                    .unwrap()
                    .starts_with("os_")
            );
            assert_eq!(
                definition["function"]["parameters"]["additionalProperties"],
                false
            );
        }
    }
}

#[derive(Clone, Copy)]
enum FileMode {
    Read,
    Directory,
    Write,
    Create,
}

/// Unix descriptor-relative traversal avoids check/open symlink races without a shell.
#[cfg(unix)]
mod anchored {
    use super::*;
    use std::{
        ffi::{CStr, CString, OsString},
        os::{
            fd::{AsRawFd, FromRawFd, IntoRawFd},
            unix::ffi::{OsStrExt, OsStringExt},
        },
    };
    #[cfg(target_os = "macos")]
    const DIRECTORY: i32 = 0x100000;
    #[cfg(target_os = "macos")]
    const NOFOLLOW: i32 = 0x100;
    #[cfg(target_os = "macos")]
    const CLOEXEC: i32 = 0x1000000;
    #[cfg(target_os = "macos")]
    const CREATE: i32 = 0x200;
    #[cfg(target_os = "macos")]
    const EXCL: i32 = 0x800;
    #[cfg(not(target_os = "macos"))]
    const DIRECTORY: i32 = 0x10000;
    #[cfg(not(target_os = "macos"))]
    const NOFOLLOW: i32 = 0x20000;
    #[cfg(not(target_os = "macos"))]
    const CLOEXEC: i32 = 0x80000;
    #[cfg(not(target_os = "macos"))]
    const CREATE: i32 = 0x40;
    #[cfg(not(target_os = "macos"))]
    const EXCL: i32 = 0x80;
    #[repr(C)]
    struct Dirent {
        ino: u64,
        offset: u64,
        reclen: u16,
        #[cfg(target_os = "macos")]
        namlen: u16,
        kind: u8,
        #[cfg(target_os = "macos")]
        name: [std::ffi::c_char; 1024],
        #[cfg(not(target_os = "macos"))]
        name: [std::ffi::c_char; 256],
    }
    unsafe extern "C" {
        fn openat(fd: i32, path: *const std::ffi::c_char, flags: i32, ...) -> i32;
        fn fdopendir(fd: i32) -> *mut std::ffi::c_void;
        fn readdir(dir: *mut std::ffi::c_void) -> *mut Dirent;
        fn closedir(dir: *mut std::ffi::c_void) -> i32;
    }
    pub(super) fn open(
        root: &std::fs::File,
        relative: &Path,
        mode: FileMode,
    ) -> Result<std::fs::File> {
        let mut parts = relative
            .components()
            .filter_map(|part| match part {
                Component::Normal(name) => Some(Ok(name)),
                Component::CurDir => None,
                _ => Some(Err(anyhow::anyhow!("Invalid relative path"))),
            })
            .collect::<Result<Vec<_>>>()?;
        let mut parent = root.try_clone()?;
        if parts.is_empty() {
            parts.push(std::ffi::OsStr::new("."));
        }
        for (index, part) in parts.iter().enumerate() {
            let last = index + 1 == parts.len();
            #[cfg(target_os = "macos")]
            const NONBLOCK: i32 = 4;
            #[cfg(not(target_os = "macos"))]
            const NONBLOCK: i32 = 0x800;
            let flags = if !last || matches!(mode, FileMode::Directory) {
                DIRECTORY
            } else {
                match mode {
                    FileMode::Write => 1 | CREATE,
                    FileMode::Create => 1 | CREATE | EXCL,
                    _ => 0,
                }
            } | CLOEXEC
                | NOFOLLOW
                | NONBLOCK;
            let name = CString::new(part.as_bytes())?;
            // SAFETY: parent owns a valid descriptor, name is NUL terminated, flags
            // select openat's documented mode argument; returned descriptor transfers once.
            let fd = unsafe { openat(parent.as_raw_fd(), name.as_ptr(), flags, 0o600u32) };
            ensure!(
                fd >= 0,
                "Cannot open scoped path: {}",
                std::io::Error::last_os_error()
            );
            // SAFETY: successful openat returned a new uniquely owned descriptor.
            parent = unsafe { std::fs::File::from_raw_fd(fd) };
        }
        if matches!(mode, FileMode::Write) {
            ensure!(
                parent.metadata()?.is_file(),
                "Write requires a regular file"
            );
            parent.set_len(0)?;
        }
        if matches!(mode, FileMode::Read) {
            ensure!(parent.metadata()?.is_file(), "Read requires a regular file");
        }
        Ok(parent)
    }
    pub(super) fn entries(directory: std::fs::File) -> Result<Vec<OsString>> {
        let fd = directory.into_raw_fd();
        // SAFETY: fd is transferred to fdopendir on success, recovered on failure.
        let dir = unsafe { fdopendir(fd) };
        if dir.is_null() {
            // SAFETY: fdopendir failed and did not take ownership.
            drop(unsafe { std::fs::File::from_raw_fd(fd) });
            bail!(
                "Cannot enumerate scoped directory: {}",
                std::io::Error::last_os_error()
            );
        }
        struct Guard(*mut std::ffi::c_void);
        impl Drop for Guard {
            fn drop(&mut self) {
                unsafe {
                    closedir(self.0);
                }
            }
        }
        let guard = Guard(dir);
        let mut names = vec![];
        loop {
            // SAFETY: the private directory stream is live and only used on this thread.
            let entry = unsafe { readdir(guard.0) };
            if entry.is_null() {
                break;
            }
            // SAFETY: readdir returns a NUL-terminated d_name valid until the next call.
            let name = unsafe { CStr::from_ptr((*entry).name.as_ptr()) }.to_bytes();
            if name != b"." && name != b".." {
                names.push(OsString::from_vec(name.to_vec()));
            }
            // Bound materialized directory names as well as serialized output.
            if names.len() >= 10_001 {
                break;
            }
        }
        Ok(names)
    }
}
