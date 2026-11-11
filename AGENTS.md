# Agent instructions

## Project intent

Build a desktop app that manages rapid-mlx and provides a complete UI for local Hugging Face models, chat with model and effort selection, persistent sessions, system load, and token statistics. See [README.md](./README.md) for product scope and current status.

## Required direction

- Use Rust for application logic, runtime integration, persistence, and telemetry.
- Deliver a desktop UI as part of the product.
- Use OpenSpec for specifications and implementation planning.
- Keep the application focused on local models and local session data by default.
- Store all app-owned state under `~/.rapid-mlx-ui`, including sessions/drafts, preferences, MCP registrations, transcripts, managed audio, logs/caches, and backups. Existing shared model caches retain their configured locations.
- Do not select a UI framework or introduce an additional implementation language implicitly. Record the decision and its tradeoffs before scaffolding it.

## OpenSpec workflow

- Before implementing a feature or materially changing behavior, inspect relevant specifications and active changes, then create or update the corresponding OpenSpec change.
- Capture requirements, acceptance scenarios, design decisions where needed, and concrete tasks before implementation.
- Initialize OpenSpec if it is still absent when implementation begins. Inspect the installed tooling and its current guidance before choosing commands or generating its directory structure.
- Keep specifications aligned with actual behavior and mark tasks complete only after validation.
- Validate and archive completed changes using the repository's OpenSpec tooling.
- Small fixes that restore specified behavior and documentation-only changes may be handled directly; update specifications if the intended behavior changes.
- Do not invent build commands, runtime capabilities, or tooling configuration. Document what actually exists.

## Architecture and integration

- Separate UI state and rendering from runtime control, model discovery, persistence, and telemetry collection.
- Verify rapid-mlx's real interfaces and supported options before implementing adapters. Do not assume an API, command-line flag, streaming format, or reasoning-effort parameter exists.
- Track runtime ownership. Do not stop or kill an externally managed process as part of ordinary app shutdown.
- Handle process exits, failed startup, disconnected streams, cancellation, and model-loading failures explicitly.
- Keep long-running work off the UI thread and release processes, streams, and monitoring tasks cleanly.
- Discover local models without modifying existing Hugging Face cache contents. Model pulling and cache-aware deletion are separate explicit user actions; never trigger downloads or deletion implicitly or delete weights when deleting a session.
- Provide basic scoped OS tools, MCP client connectivity and server registration, and local transcription with an independently selected transcription model. See the active OpenSpec change for their contracts.
- Avoid adding unrelated remote services or background network calls.

## Chat and sessions

- Preserve streamed content and associate responses with the model and settings actually used.
- Expose reasoning effort only when supported, and explain unavailable options clearly.
- Never silently substitute a different model or pretend an unsupported setting was applied.
- Persist sessions locally, including messages, model selection, and generation settings.
- Specify history behavior for editing, retrying, cancellation, and partial responses before implementing those flows.
- Keep credentials and sensitive prompts out of routine logs. Never commit secrets or personal session data.
- Treat session deletion and model-file deletion as separate operations; deleting a conversation must not remove model files.

## Statistics

- Identify the source, scope, and units of displayed metrics.
- Distinguish system-wide load from app or runtime process usage.
- Distinguish runtime-reported token counts from estimates; show unavailable metrics honestly.
- Define timing boundaries for throughput and latency, and define whether session totals include retries and partial generations.
- Use bounded sampling and retention so monitoring does not degrade generation or UI responsiveness.

## Implementation and validation

- Inspect repository state and existing conventions before editing. Preserve unrelated changes.
- Prefer focused changes and clear Rust error handling over speculative abstractions.
- Once a Rust workspace exists, use its configured formatting, linting, and test commands. Where no overrides exist, check formatting with `cargo fmt --check`, lint with `cargo clippy --all-targets -- -D warnings`, and run `cargo test`.
- Add focused tests for meaningful behavior such as model discovery, stream handling, persistence, and usage accounting. Avoid tests that only repeat implementation details.
- Prefer fixtures and mocked runtime responses for automated checks; do not require large model downloads for routine tests.
- For UI work, verify loading, empty, error, streaming, and cancellation states, as well as keyboard access and readable layout.
- Report what changed, what was verified, and any checks that could not run. Never claim tests passed without running them.
- Update [README.md](./README.md) when setup, commands, architecture, or user-visible capabilities change.
