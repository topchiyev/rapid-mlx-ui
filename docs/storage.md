# Local state and storage API

The core owns state below the current OS user's `~/.rapid-mlx-ui`. `AppPaths::discover()` resolves that home through `directories::BaseDirs`; `AppPaths::from_home(&Path)` injects an existing absolute home for tests. No working-directory, platform app-data, or temporary persistent-store fallback exists. Failure is returned to the caller.

```text
~/.rapid-mlx-ui/
  state.sqlite3
  audio/tmp/
  audio/retained/
  cache/
  logs/
  backups/
```

Directories use mode 0700; database and backup files use 0600 on Unix. Existing application directories/files have permissions tightened. Symlink state directories/files and hard-linked database files are rejected. This is owner-only local storage, not encryption. The root does not relocate Hugging Face caches or imported audio. Other modules must use these paths and create their own files privately; the path API cannot control writes performed elsewhere. Defending against a malicious process running as the same OS user is outside this filesystem boundary.

## Worker integration

`Store::open(AppPaths) -> anyhow::Result<Store>` opens one synchronous rusqlite connection. The desktop's dedicated background worker owns it and serializes every call; it must never block the render thread. A Store does not create a second worker or spawn a runtime. Keep only one live Store per app state root: opening a new Store performs crash recovery and must not race an existing app instance.

- `load_sessions() -> Result<Vec<Session>>`, `load_session(&str) -> Result<Option<Session>>`
- `save_session(&Session) -> Result<()>` checkpoints the entire tree atomically.
- `begin_turn(&mut Session, impl Into<String>) -> Result<String>` commits the submitted prompt and pending assistant/model-request together and returns the assistant ID. It only changes the caller's Session after commit succeeds. Dispatch only after success.
- `retry(&mut Session, assistant_id: &str)` and `edit_prompt(&mut Session, prompt_id: &str, content)` create and commit preserved branches, returning the new assistant ID.
- `delete_session(&str) -> Result<()>` rejects active generations/tools/jobs and cascades session-linked generic records. The UI owns explicit confirmation. It never removes files, weights, global MCP registrations, or another session.
- `set_setting<T: Serialize>(&str, &T)`, `get_setting<T: DeserializeOwned>(&str) -> Result<Option<T>>`, `delete_setting(&str)` store non-secret preferences.
- `save_record(&Record)`, `load_records(namespace: &str) -> Result<Vec<Record>>`, `delete_record(namespace: &str, id: &str)` store provider-independent JSON records. `Record::new(namespace, id, value)` defaults to no session ownership/status. Set `session_id` to cascade with that session; leave it absent for global registrations. Set lifecycle `status` for executable jobs. Foreign keys reject nonexistent session links.

Generic values are intentionally schema-independent, not a secret sanitizer. Callers must validate their own configuration and persist only non-secret environment/credential references, never resolved credentials. The storage layer does not log content. Store `Record` status at the envelope level, not only inside JSON. A generic record save is its own transaction; use `Session.messages[].tools` for intent that must commit atomically with a chat checkpoint.

## Conversation state

`Session::new(title, model)` creates an independent session with public title/draft/model/settings, `active_head`, and `messages`, plus timestamps, model revision, tool policy JSON, selected MCP IDs, transcript IDs, and metadata JSON. `GenerationSettings::default()` sets an empty system prompt, temperature 0.7, top-p 1, max tokens 2048, and no effort. These are desired settings; runtime capability validation remains the adapter's responsibility.

`Message::new(role, content, parent_id, model, settings)` creates a node; assistant nodes start pending and other roles completed. `Session::begin_turn` additionally creates the first stable `ModelRequest` ID. Stream events must identify both session/assistant and model-request IDs. `message_mut(id)` provides the active node; callers must reject stale/terminal events before applying deltas. `MessageStatus::is_active()` and `is_terminal()` support that check. Each model-request step has its actual model/settings, request JSON, status, reported usage, and separately labeled client/server timing.

`request_history(assistant_id)` builds eligible prior context plus the committed user prompt for a pending attempt; it excludes that attempt’s partial answer and every sibling attempt. Call this after the pending save succeeds to assemble initial dispatch context. `selected_branch()` returns root-to-head node references. `select_branch(Option<String>)` changes visibility without deleting history. `eligible_history()` fails for unresolved branches, excludes reasoning, and includes completed matched tool-call/result pairs before the final answer, grouping calls from the same model-request identity into one assistant envelope. A cancelled/failed/interrupted turn requires retry, prompt editing, or selection of an earlier completed head before appending a new turn. Retrying uses current session settings and adds a sibling assistant under the same prompt. Editing adds a sibling prompt under the original prompt's parent, excluding old descendants from the new context. Neither action copies or executes old tools. Historical branches remain available.

Sent prompts and terminal attempts cannot be overwritten by `save_session`; active output can grow and its status can become terminal. Existing nodes, model requests, and tool calls cannot be removed by saving. Completed model-request steps and terminal tool outcomes are immutable even while the outer turn continues. Use explicit branch operations rather than mutating sent text. The in-memory caller must keep unsaved state visible if saving fails. Coalesce stream checkpoints at the design's 250 ms cadence in the worker and flush on terminal outcome; the Store does not own a timer. Update the corresponding model-request status alongside assistant status. Optional request snapshots/raw usage/settings metadata preserve provider details without coupling core storage to adapter modules.

## Durability and recovery

SQLite uses foreign keys, FULL synchronous commits, DELETE journaling, in-memory SQL temporary storage, a five-second busy timeout, and short atomic transactions. Session snapshots are JSON in SQLite rows; branch/request/tool identity and validation live in the domain model. Session rows and session-owned generic records share cascading deletion boundaries. Settings and independent global records occupy separate tables.

On open, pending/streaming attempts and model requests become interrupted in one recovery transaction. Pending/running tools become outcome-unknown. Generic pending/running records in `tool`, `tools`, `tool_call`, `tool_calls`, or `mcp_call` namespaces become outcome-unknown; other jobs become interrupted. Recovery restores only committed checkpoints. It does not reconstruct a monotonic terminal duration, resume requests, launch MCP registrations, run tools, or retranscribe audio. New user action and a new execution identity are required to retry side effects. Audio file cleanup belongs to the audio module, not database deletion/recovery.

Schema version 1 is initial. Existing version-0 databases are backed up with SQLite's online backup API into private `backups/state-v0-<uuid>.sqlite3` before schema initialization. Empty new databases need no backup. The migration and user_version update commit together; backup failure aborts upgrade. A version newer than supported fails before migrations/recovery/journal-mode changes and is never reset or deleted. No speculative future-version migration is provided.

`Usage` uses optional counts with an explicit runtime-reported/estimated source and raw fields. `upsert_model_request` replaces cumulative updates by stable ID. `usage_totals()` visits all branches and distinct model-request IDs, includes known counts from partial/retried attempts, counts missing requests, and never adds reasoning tokens to completion again. Known subtotals start at zero but `is_partial()` and per-field unknown-request counts indicate incompleteness; do not present these subtotals as exact totals. Estimated counts are excluded from reported totals.

Tests use temporary injected homes and synthetic messages. They cover branch edits/retries, unresolved history, partial/unknown usage, exact paths/modes, blocked/symlink roots, crash checkpoints and uncertain tools, generic transcripts/settings, active/scoped deletion, immutable history, write failure with unchanged memory, backups, and newer-schema refusal. No real user data or model downloads are required.
