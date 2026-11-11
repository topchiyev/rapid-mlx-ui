# Proposal

## Why

Managing a local rapid-mlx runtime, finding downloaded Hugging Face models, and keeping conversations currently requires separate tools. A Rust desktop app should bring these actions together with persistent sessions and trustworthy system and token statistics.

## What Changes

- Deliver a desktop application with session navigation, streaming chat, model and effort selectors, runtime controls, statistics, tools, MCP server settings, and transcription.
- Propose macOS on Apple Silicon as the first release target and egui/eframe as the Rust UI framework; record alternatives and validation gates in the design before scaffolding.
- Discover existing Hugging Face cache snapshots and user-selected local model directories without downloads or cache modifications.
- List the runtime's available models separately from pulled models, support explicit pulls, and provide cache-aware deletion of pulled models.
- Manage app-owned chat and transcription services or connect to existing local services, with explicit process ownership, readiness, logs, and failure handling.
- Support text chat with Markdown, code copying, cancellation, prompt editing, retries, generation settings, and capability-aware reasoning effort.
- Store app-owned state under `~/.rapid-mlx-ui`, including durable sessions, branches, settings, tool-call history, MCP registrations, transcription records, and per-attempt usage. Existing shared model caches retain their configured locations.
- Provide basic OS tools for directory listing, file reading/search/writing, and bounded command execution from chat, with visible calls and results.
- Act as an MCP client: register, edit, enable, connect, inspect, and remove local stdio and Streamable HTTP MCP servers and expose selected tools to capable models.
- Transcribe imported audio and microphone recordings locally, select and manage the transcription model, preserve editable transcripts, and insert text into the chat composer.
- Show system load and request/session statistics with sources, timing definitions, and unavailable states.
- Deliver in ten ordered milestones: compatibility contract; desktop foundation; model/runtime integration and model management; streaming chat; durable history; observability; OS tools; MCP client; transcription; release validation.
- Keep automatic model downloads, model conversion, cloud inference/history services, exposing this app as an MCP server, image/video chat, general multi-server orchestration, and platforms beyond the initial target outside this release. User-registered remote MCP servers and explicit model pulls are supported network operations.

## Capabilities

### New Capabilities

- `desktop-shell`: Desktop navigation, shared state presentation, responsive background work, preferences, and keyboard interaction.
- `runtime-management`: Runtime discovery, configuration, owned process lifecycle, external attachment, readiness, and logs.
- `local-model-catalog`: Read-only model discovery, revision identity, metadata, compatibility, and activation selection.
- `model-chat`: Streaming text conversations, supported model and effort selection, generation controls, cancellation, and recoverable failures.
- `chat-sessions`: Durable sessions, immutable generation attempts, branching edits/retries, and recovery after interruption.
- `observability`: System/process monitoring and response/session usage with explicit sources and aggregation semantics.
- `model-management`: Available-versus-pulled model views, explicit downloads, and cache-aware removal, including transcription models.
- `os-tools`: Basic filesystem and command tools, session scopes, model tool-call execution, and visible results.
- `mcp-client`: Persistent MCP server registration, connection lifecycle, discovery, and selected MCP tool execution in chat.
- `audio-transcription`: Imported/recorded audio transcription, transcription-model selection and lifecycle, and durable transcript history.

### Modified Capabilities

None. There are no existing capability specifications or application code.

## Impact

- Introduces a planned Rust workspace with separate desktop and core components. Proposed dependencies cover UI rendering, async HTTP/process work, SQLite, serialization, system sampling, MCP client support, and audio capture; exact versions are selected and locked during implementation.
- Integrates with the installed rapid-mlx executable and its loopback HTTP service. The local CLI reports version 1.0.2; route and model behavior still require fixture-backed and live verification before enabling features.
- Writes app-owned state under `~/.rapid-mlx-ui`. Discovery stays read-only; explicit pulls/deletions are separate actions that may change the selected model cache. Session deletion never deletes weights, and externally owned services are never terminated.
- Establishes acceptance scenarios and executable implementation tasks under this change. This proposal does not scaffold the app or claim any planned feature is already available.
