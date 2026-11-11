# Design

## Context

See [proposal.md](./proposal.md) for motivation and scope. The repository contains documentation and OpenSpec tooling but no Rust workspace or application implementation. No existing main specifications constrain this new design.

Planning inspection on 2026-11-11 found an arm64 development machine with `rapid-mlx 1.0.2` available on PATH. Version/help commands and read-only source inspection verified CLI contracts; no server was launched, model loaded, cache changed, or inference performed. Upstream documentation is evidence for an adapter design, not proof that a particular local model supports every documented field. The requested additions extend this plan to OS tools, an MCP client, transcription, and explicit model downloads/deletion; discovery and ordinary inference remain local-only.

### Integration evidence

| Source | Observed contract | Implementation consequence |
| --- | --- | --- |
| Local `rapid-mlx --version` and `serve --help` | Version 1.0.2; host/port configuration; local-only loading; effort defaults; parent watchdog | Begin compatibility fixtures with this version and probe the configured executable before startup. |
| [Upstream API reference](https://rapidmlx.com/docs/api) | Chat Completions supports SSE, separate reasoning, discovery, health, OpenAPI, status, and cancellation routes | Use Chat Completions as the chat transport and verify optional features against the running server. |
| [Upstream CLI reference](https://rapidmlx.com/docs/cli) | Server launch and controls for network binding, downloads, telemetry, and version checks | Launch an explicitly configured loopback child process; do not use interactive chat/start helpers. |
| [Local-server guide](https://rapidmlx.com/docs/openai-compatible-server) | An explicitly selected port fails if occupied | Pin a configured port and surface a collision rather than adopting an unrelated service. |
| [Bring-your-own-model guide](https://rapidmlx.com/docs/byom) | A local folder is a valid serve target; remote IDs can trigger downloads | Launch the chosen resolved local path and enforce local-only mode. |
| [Hugging Face cache guide](https://huggingface.co/docs/huggingface_hub/guides/manage-cache) | Configurable cache roots, repository revisions, and snapshot files backed by shared blobs | Read snapshots directly, preserve revision identity, and follow ordinary cache symlinks without mutation. |
| [eframe documentation](https://docs.rs/eframe/latest/eframe/) | Rust application framework with native window support | Prototype the desktop shell without introducing a browser frontend language. |
| Local `models`, `pull`, and `rm` help plus installed CLI source | JSON available/cached catalogs; explicit pulls; repository-wide cached-model removal through Hugging Face revision deletion | Use machine-readable catalog output and preview all affected revisions before removal. |
| [Audio model guide](https://rapidmlx.com/docs/models/families/audio) | Local audio serving and multipart transcription requests with a selected model | Verify an ASR-specific adapter and its required local auxiliary assets independently of chat. |
| [Official MCP Rust SDK](https://github.com/modelcontextprotocol/rust-sdk) | Rust client support for child-process stdio and Streamable HTTP transports | Use rmcp with explicit protocol-version/capability negotiation and pinned dependency versions. |

## Goals / Non-Goals

**Goals:** keep state transitions testable without a GPU, keep rendering independent of I/O, preserve user history across failures, and make optional runtime/model capabilities explicit.

**Non-Goals:** embed or rewrite the inference engine, maintain a Python application layer, promise equivalent behavior across arbitrary runtime versions, expose this app as an MCP server, or implement general multi-server inference orchestration. The first release runs on macOS with Apple Silicon. Inference remains loopback-only; explicitly registered remote MCP endpoints and requested model pulls can use the network.

## Decisions

### 1. Use egui/eframe for an all-Rust desktop UI

Choose egui with eframe for the initial implementation. The app's controls, chat state, and live statistics fit a Rust UI, and a single language avoids a separate frontend build. This is a proposed architecture decision recorded before scaffolding, not an assertion that the UI already exists.

| Alternative | Tradeoff and decision |
| --- | --- |
| egui/eframe | Selected for a Rust implementation and direct state-driven rendering. Custom chat styling, Markdown selection, keyboard interaction, and accessibility require early validation. |
| Tauri with a web frontend | Attractive for rich document rendering; introduces frontend languages, a second build pipeline, and an IPC boundary. Defer unless the Rust UI prototype fails its acceptance checks. |
| Iced | Also keeps UI code in Rust; not selected to avoid spending this release comparing multiple UI implementations. Reconsider only if the prototype exposes an unmet requirement. |
| SwiftUI frontend with Rust core | Could offer closer platform integration but introduces Swift and a language bridge; outside the chosen single-language approach. |

Milestone 2 must demonstrate selectable streamed Markdown, copyable code, multiline input, visible keyboard focus, resizing, and macOS accessibility behavior before broader UI investment. If the chosen framework cannot meet the spec, revise this change explicitly before switching frameworks. Exact crate versions and minimum macOS/Rust versions are selected from a successful build and locked during scaffolding.

### 2. Separate the desktop executable from a core library

Plan a Cargo workspace with a desktop executable crate and a core library crate. Core modules own domain models, runtime supervision, catalog/model operations, chat/tool orchestration, MCP clients, transcription, storage, and monitoring. The desktop crate owns rendering and translates user actions into typed commands.

| Component | Responsibility | Boundary |
| --- | --- | --- |
| Desktop | Layout, navigation, composer, model/effort controls, stats presentation | No filesystem scans, process waits, or blocking DB/HTTP calls on the render thread |
| Runtime supervisor | Owned child, external connection, readiness, model activation, logs | Only an owned child handle grants termination authority |
| Catalog | Cache roots, local paths, revisions, compatibility observations | Read-only filesystem operations |
| Chat transport | Request snapshots, incremental SSE decoding, terminal outcomes | Typed events carry session and attempt IDs |
| Storage | Session/branch/attempt transactions, settings, migrations | One serialized writer on a worker |
| Monitor | System and owned-process samples, optional server metrics | Bounded history and optional providers |
| Model operations | Available catalog, explicit pull jobs, cache-aware deletion | Discovery is read-only; mutations require a separate explicit action |
| Tool executor | Built-in OS tools and multi-step chat orchestration | Session tool policy, bounded execution, durable call identity |
| MCP manager | Server registry, negotiated connections, discovery, and routed calls | App is a client; only owned stdio children can be terminated |
| Transcription | Audio capture/import, ASR model activation, jobs, and transcript editing | Explicit recording and local inference; app-owned audio stays in the state root |

Use Tokio for async process/network tasks, reqwest for HTTP streaming/uploads, serde for structured data, rusqlite for SQLite, sysinfo for CPU/memory sampling, and the official rmcp SDK for MCP clients. Select a Rust audio-capture/encoding dependency after its macOS recording prototype passes; no Python application code is introduced. Verify and lock dependency versions during implementation. Keep these modules within the existing two-crate design rather than introducing separate service crates.

Send commands and lossless chat events through bounded queues with backpressure. Coalesce replaceable telemetry/progress updates, never drop chat deltas, and discard late events whose attempt is already terminal. Keep blocking scans and SQLite work on workers. Limit redraw requests rather than repainting continuously when idle.

### 3. Treat the runtime as a supervised local service

For managed mode, use a structured process argument list, never a shell command. Probe the executable's version and help before using its supported flags. The initial supported profile is the inspected 1.0.2 CLI; do not automatically declare all later versions compatible.

The planned launch shape uses verified CLI options: `rapid-mlx serve <resolved-local-model-path> --served-model-name <stable-model-id> --host 127.0.0.1 --port <configured-port> --disable-model-downloads --watchdog-ppid <app-pid>`. Assign a stable served identity to the selected catalog entry and use that same identity for readiness and subsequent requests. Set `RAPID_MLX_TELEMETRY=0`, `RAPID_MLX_DISABLE_VERSION_CHECK=1`, and `HF_HUB_OFFLINE=1` in the child's environment. Verify offline behavior in the managed integration check; never fall back to a remote repository ID. If the selected executable cannot enforce these constraints, managed launch stays unavailable.

Represent lifecycle as stopped, starting, ready, stopping, or failed; represent external attachment separately so a connection cannot acquire process ownership. Maintain at most one owned chat service and one owned transcription service, with distinct role-specific configurations and ports. Readiness requires the child to remain alive, a successful health response, matching model identity, and the role's verified endpoint. Poll with backoff and a default configurable startup deadline of five minutes. Retain at most 2,000 diagnostic log lines with a maximum line size of 8 KiB.

Default the chat port to 8000 and the transcription port to 8001; make both editable and reject collisions. Explicit external attachment validates a loopback endpoint; automatic port scanning or silently attaching to an occupied port is excluded. Credentials for inference or MCP, when required, are entered for the current app run or resolved from explicitly configured environment references and are not persisted as secret values.

Shutdown cancels/finalizes the current attempt, signals only the child started by this app, waits up to ten seconds, then terminates and reaps that same owned process if necessary. Use the verified parent watchdog to cover abrupt app death. Check that no descendants survive in the runtime integration test. No PID-based adoption or machine-wide process killing is allowed.

Model switching in managed mode is a stop/start operation after generation ends; HTTP model management endpoints are not required. External mode offers only identities actually served and never tries to reconfigure the external server. This is simpler and safer than depending on version-specific dynamic model loading.

### 4. Scan local model files without a downloader dependency

Resolve cache location in this order: explicit app override, `HF_HUB_CACHE`, `HF_HOME` plus `hub`, `XDG_CACHE_HOME` plus `huggingface/hub`, then the user's default cache location. Inspect model repositories and their snapshots; also accept directly selected local model folders. Do not treat the served-model HTTP list as a complete inventory of local downloads.

Record repository ID, revision when known, resolved load path, available format/quantization metadata, logical weight size, and compatibility state. Deduplicate the same resolved path across scan roots. Label logical model sizes separately from physical cache usage; do not claim summed model sizes equal unique disk usage when blobs are shared.

Validate indexed shards and required local configuration with bounded metadata reads, follow normal cache links, detect broken links, avoid recursive directory cycles, and isolate per-directory errors. Treat metadata as untrusted data and never execute repository code. Static checks establish a candidate, not a guarantee of successful model loading. Runtime load outcomes refine compatibility for that executable version. An unknown candidate requires explicit load action; clearly unsupported or incomplete entries cannot activate.

The alternative is wrapping a Hugging Face Python scanner or rapid-mlx listing command. Direct read-only Rust discovery avoids another application language and keeps behavior fixture-testable.

### 5. Use a capability-aware streaming chat adapter

Fetch `/openapi.json` and `/v1/models` on explicit connection. Required routes are `/health`, `/v1/models`, and `/v1/chat/completions`; optional routes include `/v1/status`, `/metrics`, and `/v1/requests/{id}/cancel`. The adapter uses schema evidence plus a tested version/model profile, not the presence of a parser name alone, to enable optional controls. Unknown optional fields remain unavailable.

Use `stream: true`, parse SSE across arbitrary HTTP chunk/UTF-8 boundaries, and handle comment frames, multiple data lines, errors, final usage, tool-call deltas, and terminal markers. Persist answer and reasoning separately and render them as inert text/Markdown. Only completed structured calls to enabled tools can enter the tool executor; embedded HTML/scripts and tool-like plain text never execute. Prefer runtime-reported usage; no token estimator is needed for the first release.

Offer the runtime's verified per-model effort values, not an unconditional low/medium/high menu. Local CLI help lists `none`, `minimal`, `low`, `medium`, `high`, and `xhigh` for the runtime default, but this does not prove per-request/model support. Verify request schemas and model profiles before exposing a value. Store the requested value, serialized fields, and known normalization (native effort or thinking-budget mapping); acceptance by a generic schema alone does not prove meaningful model behavior.

Cancellation immediately makes the attempt terminal locally and closes its stream. Use the optional cancel route only when tests establish how the server's cancellation identity maps to the active request. If identity or support is unavailable, show that server termination is unconfirmed and refresh runtime status; never kill an external server to cancel a request.

Allow one active chat turn globally, including its model/tool continuation steps, and serialize heavyweight model inference between chat and transcription jobs in the first release. Preserve drafts and navigation while another session or ASR job is using inference. Edits to settings while streaming affect future requests only. Do not silently trim context or retry a rejected request. Local output-token and sampling validation must agree with the negotiated API profile.

### 6. Persist a conversation tree and immutable request attempts

Use `~/.rapid-mlx-ui` as the sole home-relative application state root, resolved from the current OS user's home independently of the working directory. Store SQLite data in `state.sqlite3`; settings, window state, sessions/drafts, tool records, MCP registrations, model-operation records, and transcription records live in that database. Use `audio/tmp`, `audio/retained`, `cache`, `logs`, and `backups` below the same root for app-owned files. Do not silently fall back to a platform-default app-data folder when this root is unwritable.

Existing shared Hugging Face model caches remain in their configured locations; model weight downloads/partial files are model-cache contents, not a second app-state store. Explicit exports and OS-tool actions use user-selected destinations. Redirect app-specific runtime temporary/cache writes through supported settings or disable the relevant disk cache; verify that no app-owned session or recording data leaks to a default engine directory. Application state files use user-only permissions and are excluded from source control.

Use SQLite with serialized writes, foreign keys, migrations, and transactions. JSON-only files would make branches, partial checkpoints, and usage reconciliation more fragile. The database is local and not encrypted by the app in this release; credentials stay out of persisted state and routine logs.

Logical records:

- Session: ID, title, timestamps, active branch head, saved composer draft, selected model/revision, next-request settings, OS-tool scope, and selected MCP server IDs.
- Message node: ID, session, parent node, role, and text; sent nodes are preserved.
- Generation attempt: ID, prompt node, resulting assistant node, model identity, settings snapshot, requested/serialized effort, status, finish reason, partial answer/reasoning, runtime version, and available request identity.
- Attempt usage: reported counts and source, optional reasoning breakdown, client/server timing with separate names, and terminal metadata.
- Model request: one inference step within a turn, its request snapshot, tool-call envelopes, terminal state, and usage; aggregate these rows once into turn/session totals.
- Tool call: provider/server ID, tool/call ID, arguments, policy decision, status, bounded result, and timestamps; persisted user content is excluded from routine logs.
- MCP registration: stable ID, name, transport, endpoint or executable/arguments, non-secret environment references, enabled state, and selected capabilities.
- Transcription job: source reference, selected ASR identity/revision, supported options, status, transcript/edits, and timing; raw audio retention is a separate preference.
- Model operation: catalog identity, target cache, operation type, transfer/deletion status, and affected repository scope.
- Preferences: configured executable, role-specific loopback endpoints/ports, scan roots, transcription selection, and display/tool choices; no credentials.

Editing a sent user message creates a sibling user node from its parent. Retrying creates a sibling assistant attempt for the same user node using the settings shown in the retry action. Existing descendants stay on their original path. Selecting a branch controls visible messages and eligible request context, never deletes another branch.

Commit the submitted prompt and pending attempt before network dispatch, and tool intent before executing side effects. Coalesce streaming changes onto a 250 ms checkpoint cadence and flush at terminal outcomes. A crash can lose content received since the last checkpoint; restored active attempts/jobs become interrupted and tools with uncertain outcomes are never replayed automatically. Keep unsaved content visible after storage failure and block new dispatch until its pending record can be saved. Continue from completed turns; unresolved turns require user recovery. Completed structured tool/result pairs stay in branch context; displayed reasoning is not implicit replay context. Retrying a turn never rolls back or silently repeats earlier side effects.

### 7. Separate reported usage from measured load

Use sysinfo sampling on a worker for system CPU and memory and separately label app/owned-process measurements. An external runtime's process usage stays unavailable unless ownership/identity can be safely established; HTTP status metrics are runtime-reported, not OS process measurements. Default sampling is one second with 300 samples per metric, at most one outstanding sample per provider, and stale state after three missed intervals. Stop monitors on shutdown.

GPU utilization is optional and starts unavailable unless a reliable unprivileged provider is verified on the supported macOS target. Runtime Metal allocation, when present, is a separate memory metric. Do not invoke privileged collection tools or convert memory usage into utilization.

| Statistic | Definition |
| --- | --- |
| Prompt/completion/total tokens | Runtime-reported counts on one model-request step; cumulative updates replace prior values. A tool-enabled turn sums its distinct steps once. Missing fields remain unknown. |
| Reasoning tokens | Optional subset/breakdown of reported completion usage; never added twice. |
| Client elapsed | Monotonic duration from request dispatch to completed, cancelled, failed, or interrupted terminal handling in the running process. |
| Client time to first output | Dispatch to first non-empty answer or reasoning delta; absent if no output arrives. |
| End-to-end generated tokens/s | Reported completion tokens divided by positive client elapsed seconds; includes waiting and prefill and is not decode speed. |
| Server timing/decode rate | Optional separately labeled runtime measurements; parse only verified fields and retain their original definitions. |
| Session reported usage | Sum each attempt's distinct model-request steps once across every branch; include known cancelled/failed usage and missing-count indicators alongside partial subtotals. |

Do not sum repeated prompt contexts as unique transcript size: usage totals represent work performed for each request, including retries. Preserve raw reported fields so a future adapter can reinterpret a breakdown without altering recorded history. Unknown values are not zero. Cross-restart interruption cannot reconstruct a monotonic end time; retain only previously checkpointed timing and label terminal timing unavailable.

### 8. Manage available and pulled models explicitly

Use the verified `rapid-mlx models --json` catalog and `models --cached --json` as machine-readable integration sources, merging pulled results with direct discovery for custom directories and revisions. Local help confirms that JSON cannot be combined with the CLI's search/modality switches; filter the parsed catalog in Rust and classify ASR entries separately from TTS and chat. Cache catalog observations in the app database and label their origin/freshness.

Run explicit pull jobs with `rapid-mlx pull <selected-reference>` after the user chooses Pull. Show the target cache, size if known, transfer progress when available or an honest indeterminate state, cancellation, and retry. Use a separate operation environment: model pulls permit their requested network transfer, while serving still disables downloads. Never set the support-request submission flag automatically. Validate required model and auxiliary assets before marking a pull ready, including ASR processor/tokenizer assets.

For removal, local source inspection shows that `rapid-mlx rm` removes all cached revisions of a matched model repository through Hugging Face's cache deletion strategy. Preview that full scope; do not present repository-wide removal as deletion of just one revision. Invoke the verified cache-aware command with confirmation bypass only after the app's explicit deletion confirmation, and bind it to the selected cache. Verify shared-blob behavior against the installed cache library before enabling removal. Do not use recursive directory deletion, delete arbitrary source folders, or remove a model while any app-known loaded/job/pull reference holds its use lock. External users of a shared cache cannot be assumed absent; never stop their service to free a lock. Stored sessions/transcripts survive removal and keep the missing model identity.

The alternative is implementing cache garbage collection in Rust. The verified runtime/cache library owns deletion semantics; a new collector would unnecessarily duplicate shared-blob reference handling. Discovery remains read-only regardless of this separate mutation pathway.

### 9. Execute basic OS tools through a shared tool layer

Built-in tools are directory listing, file reading, filename/text search, file creation/writing, and command execution. OS tools default disabled until a session is configured. Store filesystem roots, working directory, enabled tools, and the user's execution policy. File tools resolve paths and symlinks before enforcing roots. Command tools accept an executable, argument list, and working directory; they use the configured command policy, not a promise that a working directory is an OS sandbox. Do not request elevation implicitly.

Reads within granted roots can run under the enabled policy; writes and commands follow the user-configured mutation/execution policy, with explicit review when that policy requires it. Show the exact operation and outcome, apply time/output limits, and clean up only app-owned command children. The initial defaults cap a command at 60 seconds and 1 MiB combined output; users can configure bounded alternatives. Model/tool output cannot widen permissions.

Assemble tool arguments across streaming deltas and validate complete calls only after the model step ends. Namespace providers, preserve call IDs, execute calls serially in the first release, and cap a turn at eight model/tool cycles. Append complete assistant tool-call envelopes and matched tool-result messages before asking the model to continue. Return schema-valid tool errors on denial/failure. Render calls/results with visible progress. No tool executes merely because Markdown or partial text resembles a call.

The tool layer records intent before dispatch and outcomes afterward. Stop propagates cancellation, but an already applied file write, command, or remote operation is not undone. After a crash or ambiguous network failure, expose outcome-unknown instead of retrying automatically. This shared layer also dispatches selected MCP tools, avoiding separate history/accounting implementations.

### 10. Register and connect to MCP servers as a client

Use the official rmcp client SDK with verified pinned protocol/transport support. Store server definitions in the app database under the fixed state root, using stdio executable/arguments/environment references or a Streamable HTTP URL. Registration means configuring a server for this app to connect to; publishing a server or registry entry is outside scope, as clarified by the user.

Support local stdio, loopback HTTP, and user-registered remote HTTPS endpoints. Keep inference endpoint validation separate so the loopback inference restriction does not reject a remote MCP server. Connect through the SDK's negotiated discovery lifecycle, track connection/errors, follow tool/resource/prompt pagination, and refresh advertised changes. Enable selected server tools per session with distinct model-visible identifiers for collisions. Resource and prompt contents enter context only through explicit selection.

Resolve explicitly configured environment/credential references or accept credentials for the current run; persist no secret values in registrations. Surface required/unsupported authentication flows without claiming connectivity. Connecting a stdio registration authorizes launching that configured program, not installing arbitrary dependencies. Restore definitions after restart and require Connect for the new app run. Disconnect stops owned stdio children and closes HTTP clients without terminating external servers.

Remote tools receive only their call arguments and explicitly selected context; full session histories are not uploaded automatically. Local filesystem-root enforcement belongs to built-in file tools; it cannot constrain what a remote provider does internally, so MCP tool selection/policy is provider-specific. Model capability gating, cancellation, durable results, and step limits use the same execution layer as built-in tools.

The alternative is forwarding every MCP registration to rapid-mlx's server-side MCP configuration. Keep the registry and execution policy in the Rust app to preserve UI visibility, independent connection lifecycle, and shared session history without requiring runtime restarts.

### 11. Transcribe locally with an independently selected ASR model

Provide audio-file import and explicit microphone recording with native macOS permission handling and a visible recording indicator. Use Rust capture/encoding on a worker, validate advertised supported formats and limits, and stage app-owned recordings only under the state root. Do not alter imported files. Do not capture system audio or add system-wide dictation shortcuts in this change.

Use a separately configured local rapid-mlx ASR service and its verified `POST /v1/audio/transcriptions` multipart contract. The audio guide documents a file plus model selection; inspect the installed schema/adapter for local model identifiers, language/options, readiness, cancellation, and auxiliary asset requirements before enabling them. Choose speech-to-text entries, not arbitrary audio aliases. Start the selected pulled local folder with verified offline controls, and use a separate loopback port so transcription selection never changes chat selection. At most one inference job runs at a time across both roles in the first release; background process residency remains visible in stats.

Expose explicit pull/load/unload/remove controls for the ASR selection through model management. Missing runtime extras or auxiliary assets produce actionable unavailable states and explicit acquisition steps, not automatic installation or downloading during transcription. The selected ASR identity/revision is saved independently of chat and each job records the model actually used.

Persist editable transcript text and job metadata immediately after receiving a result; preserve received partial text when supplied. Insert into a composer only on user action, with an append/replace choice when a draft exists and a separate Send action. Default raw-audio retention is off: clean staged recordings after terminal jobs and clean abandoned app-owned temporary files during recovery. An optional retention preference keeps recordings below `audio/retained`. Record transcription duration/real-time performance separately from text-token usage.

The alternative is a cloud transcription provider or an embedded Rust ASR engine. Reuse rapid-mlx's local audio interface to keep model management consistent and avoid a second inference backend; protocol tests and an explicit local ASR smoke test verify actual support.

### 12. Deliver in vertical milestones

1. **Compatibility contract:** record the supported CLI/API profile and sanitized fixtures, including effort mapping, usage, and cancellation identity. No bundled engine or runtime installation is required.
2. **Desktop foundation:** scaffold the two-crate Rust workspace and validate the framework with fixture-driven chat, statistics, and keyboard/accessibility states before wider integration.
3. **Models and runtime:** implement read-only discovery, available/pulled views, explicit pull/removal, managed lifecycle, and external attachment. Exit with local activation, mutation scope, and cleanup demonstrated.
4. **Streaming chat:** wire real requests, generation settings, effort gating, formatting, and cancellation. Exit with mocked protocol edge cases and a local smoke test.
5. **Durable sessions:** finish transactional history, branching, crash recovery, preferences, and deletion. Basic persistence infrastructure begins in milestone 2 so no real request depends on a later storage retrofit.
6. **Observability:** wire bounded sampling and usage/timing presentation. Exit with fixture-verified accounting and stale/unavailable states.
7. **OS tools:** enable the scoped filesystem/command tools and durable model/tool continuation loop, including per-request usage accounting.
8. **MCP client:** add persistent server registration, stdio/HTTP connection lifecycle, discovery, and selected tool/context integration.
9. **Transcription:** add file/microphone workflows, independent ASR model management, and durable transcript/composer integration.
10. **Release validation:** run the combined acceptance flows, publish reproducible build/run guidance, and produce a local macOS app bundle. Public distribution/signing is a later release operation.

## Risks / Trade-offs

- **UI richness and platform integration** → prove text selection, Markdown, focus, scaling, and accessibility early; revise the framework decision explicitly if the prototype fails.
- **Runtime/version drift** → maintain a verified compatibility profile, capture sanitized protocol fixtures, and disable unverified optional features rather than guessing.
- **Effort appears accepted but has no model effect** → require tested per-model mapping evidence and expose known normalization; generic schema acceptance is insufficient.
- **Large or partly downloaded caches** → bound reads, keep scans off the UI thread, isolate errors, and revalidate at activation.
- **Abrupt shutdown leaves server work active** → use owned-child cleanup/watchdog and distinguish local cancellation from confirmed server cancellation.
- **Telemetry or downloading initiated by the engine** → verify inherited offline/disable controls with the supported executable; never silently weaken local-only startup.
- **SQLite contention or storage failure** → serialize writes, use short transactions, retain unsaved in-memory content, and test full/unwritable storage.
- **Unavailable GPU data** → ship an explicit unavailable state while still providing CPU, memory, and verified runtime memory statistics.
- **Cache removal affects more than one revision** → preview repository-wide scope, hold active-use locks, and test shared-blob preservation before enabling deletion.
- **Tool side effects outlive a stopped turn** → record intent/outcomes, show uncertainty, and never automatically replay commands or MCP calls.
- **Remote MCP credentials/context escape into saved state** → use non-secret references, memory-only credentials, explicit server selection, and focused redaction tests.
- **ASR assets or audio extras are missing** → verify the selected model and its auxiliary assets separately, keep acquisition explicit, and test missing-capability states.

## Migration Plan

There is no existing application database or public API to migrate. Introduce an initial schema version under `~/.rapid-mlx-ui` and seed only synthetic development fixtures. No platform app-data location or model cache is relocated implicitly. Create backups under the same state root before future schema upgrades; if a newer database is unsupported, report incompatibility without resetting history. Backups require the same permissions as the database. Implementation must reconcile the README, agent guidance, and OpenSpec context with explicit model mutation and the fixed state root; earlier blanket cache-preservation wording applies to discovery and ordinary inference, not user-requested pulls/removal.

Each milestone must pass its focused checks before proceeding. Keep implementation tasks unchecked in this planning change until code and validation exist. Before archiving, run strict OpenSpec validation, Rust formatting/lint/tests, and the desktop acceptance walkthrough. Rollback of the first implementation means reverting application changes while retaining user data; never remove a cache or database as an automatic repair action.

## Open Questions

- The exact minimum macOS and Rust versions will be recorded from the successful milestone-2 build; this does not expand the initial Apple Silicon target.
- A concrete model for the opt-in live smoke test depends on which already downloaded compatible checkpoint the developer selects. Automated validation uses synthetic fixtures and never downloads weights.
- An unprivileged GPU utilization provider can be added if verified on the initial target; the unavailable behavior is already specified and does not block release.
