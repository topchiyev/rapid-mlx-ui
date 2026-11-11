# Rapid MLX UI

A Rust desktop app for managing rapid-mlx, local Hugging Face models, and conversations on Apple Silicon Macs. Requirements and implementation progress are tracked with OpenSpec.

## Development status

The repository now contains a two-crate Rust workspace, an egui/eframe desktop UI, SQLite persistence, runtime/model management, streaming chat, scoped OS tools, an MCP client, transcription, and system/usage monitoring. Automated checks cover protocol fixtures, temporary files, and combined MCP/chat persistence. Live text generation and cancellation were also checked with an already downloaded Qwen3.5 4B model. Real ASR inference, microphone permissions, and the complete native desktop walkthrough remain verification gates; see the [validation record](./docs/validation.md) and unchecked [project tasks](./openspec/changes/plan-rapid-mlx-desktop/tasks.md).

## Build and run

The current development target is **macOS 27 on Apple Silicon**, tested with **Rust 1.96.1**. An existing rapid-mlx **1.0.2** installation is required for managed inference; the app does not install it. See [runtime compatibility](./docs/compatibility.md) for source-backed support and limitations.

```sh
cargo run -p rapid-mlx-ui
```

The window opens without automatically starting a server, downloading models, recording audio, or connecting registered MCP servers. Configure the runtime executable and model locations in Settings, choose a local model revision, then Load or explicitly Connect to an external loopback server. External served models must be selected by their advertised identity.

To inspect synthetic UI fixtures without writing application data or running real operations:

```sh
cargo run -p rapid-mlx-ui -- --demo
```

Create a local unsigned app bundle:

```sh
scripts/bundle.sh --debug
# Or build the optimized bundle:
scripts/bundle.sh
```

The bundle is created at `dist/Rapid MLX UI.app`. Public signing and distribution are outside this change. Bundle metadata includes a microphone usage explanation; audio recording starts only when the user presses Record.

## Local application state

All app-owned state is stored under **`~/.rapid-mlx-ui`**, independently of the working directory:

- `state.sqlite3`: sessions, drafts, message branches, generation settings, usage, tool records, preferences, MCP registrations, and transcription jobs.
- `audio/tmp` and `audio/retained`: temporary and explicitly retained app recordings.
- `cache`, `logs`, `backups`, and role-specific runtime staging: bounded app-managed supporting files.

An unwritable state root produces an error instead of switching storage locations. Credentials are kept in memory for the current app run or resolved from configured environment references. They are not stored in the state database. Existing shared Hugging Face model caches keep their configured locations.

## Models and runtime

The Models page separates **Available** catalog entries from **Pulled** local revisions, with search, task filters, and current/cached catalog timestamps. Discovery reads the configured cache plus additional local folders and does not download or modify models. Pull is explicit. Deleting a pulled model requires a fresh preview and confirmation of the complete repository/revision scope; loaded or active models cannot be removed. Session deletion never deletes weights.

The app supervises separate chat and transcription roles, using `127.0.0.1` ports 8000 and 8001 by default, and serializes heavyweight inference. External connections also support numeric IPv6 loopback. Runtime ownership is explicit: shutdown stops app-owned children and disconnects from external servers. Model deletion requires external inference connections to be disconnected first because the app cannot enumerate their cache usage; disconnecting leaves those servers running. See [model management](./docs/models.md).

## Conversations, tools, and MCP

Chat supports streamed answer/reasoning presentation, Markdown and code copying, model selection, supported reasoning effort, system/sampling settings, stopping, retries, editing prompts as preserved branches, and durable session history. Unsupported or unverified optional settings stay unavailable. Interrupted requests and uncertain tool outcomes are not automatically replayed.

Basic OS tools support directory listing, bounded file reads/search, file creation/writing, and explicitly granted commands. Configure a session working directory, filesystem access, mutation permissions, and allowed executable paths in OS tools. A command grant is execution authority, not an OS filesystem sandbox. Calls and results are retained with the conversation. See [chat and OS tools](./docs/chat-tools.md) and [session storage](./docs/storage.md).

The app acts as an **MCP client**. Register local stdio commands or Streamable HTTP endpoints, connect explicitly, inspect discovery, and select tools for the current session. Registration state is local; credentials are references or current-run values. The app does not publish an MCP server. See [MCP setup and a synthetic test server](./docs/mcp.md).

## Transcription and statistics

Transcription uses an independently selected local speech-to-text model, with explicit model pull/load/unload/removal, or an explicitly connected loopback ASR service. Select the exact advertised model after connecting. Import validated WAV audio or record from the microphone, edit and save the transcript, then explicitly append or replace a chat draft. Inserting text does not send it. Raw recording retention is off by default. See [audio support and verification limits](./docs/audio.md).

The statistics view distinguishes system CPU/memory, owned process usage, runtime-reported tokens, client timing, and unavailable measurements. GPU utilization is unavailable without a verified provider. Usage includes distinct model requests from retries and tool continuations; missing usage stays unknown. See [metric definitions](./docs/metrics.md).

## Validation

```sh
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
npx --yes @fission-ai/openspec@1.17.0 validate --all --strict --no-interactive
```

Tests do not require model weights or microphone access. The successful real stdio fixture test has an explicit example-build prerequisite documented in [the MCP guide](./docs/mcp.md).

## OpenSpec workflow

The active [proposal](./openspec/changes/plan-rapid-mlx-desktop/proposal.md), [design](./openspec/changes/plan-rapid-mlx-desktop/design.md), [acceptance specs](./openspec/changes/plan-rapid-mlx-desktop/specs/), and [tasks](./openspec/changes/plan-rapid-mlx-desktop/tasks.md) define the implementation. Main specs are populated when the completed change is archived.

OpenSpec 1.17.0 uses the `spec-driven` schema with Codex skills. Invoke `$openspec-propose`, `$openspec-update-change`, or `$openspec-apply-change` for the relevant workflow. The CLI can run through `npx --yes @fission-ai/openspec@1.17.0` without a global installation.

See [AGENTS.md](./AGENTS.md) for agent instructions and [openspec/config.yaml](./openspec/config.yaml) for project constraints.
