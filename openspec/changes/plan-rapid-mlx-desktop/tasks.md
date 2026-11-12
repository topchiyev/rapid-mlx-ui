# Tasks

All tasks are implementation work and remain unchecked until the stated verification succeeds. Follow the ten milestones in order; later groups can extend earlier infrastructure without postponing that infrastructure's own tests. Acceptance contracts are in [specs/](./specs/) and architecture decisions are in [design.md](./design.md).

## 1. Runtime compatibility contract

- [x] 1.1 Record the installed rapid-mlx version, executable discovery behavior, local-path launch arguments, offline controls, watchdog behavior, and supported CLI profile in a compatibility document; verify each claimed CLI option against that executable's help output.
- [x] 1.2 Inspect the supported runtime's OpenAPI/source contracts and create sanitized fixtures for health, served models, chat requests, answer/reasoning SSE, final usage, errors, and optional timing; label synthetic fixtures and verify every fixture's provenance is recorded.
- [x] 1.3 Establish per-request reasoning-effort mappings for supported model families, including normalization and unsupported/unknown cases; verify the compatibility document distinguishes schema acceptance from actual model support and cites the relevant runtime implementation or live evidence.
- [x] 1.4 Establish whether a streaming request ID maps to the cancellation route, and document the local-stream-close fallback; verify supported and unavailable cancellation cases each have protocol fixtures.
- [x] 1.5 Reconcile the README, agent guidance, and OpenSpec context with the fixed `~/.rapid-mlx-ui` state root, OS tools, MCP client, transcription, and explicit model pulls/deletion; verify ordinary inference remains local-only and older blanket exclusions no longer contradict this change.
- [x] 1.6 Capture available/cached catalog JSON, pull behavior, and repository-wide removal contracts from the installed runtime; verify provenance, cache overrides, full revision scope, and shared-blob preservation are documented and have synthetic fixtures before implementing deletion.
- [x] 1.7 Inspect transcription serving, multipart schema, local-model identity, optional audio dependencies/assets, readiness, cancellation, and supported input limits; verify compatible and unavailable ASR cases are represented in sanitized fixtures without downloading weights.
- [x] 1.8 Record verified structured tool-call/result contracts and a pinned supported MCP SDK/protocol profile for stdio and Streamable HTTP; verify version/capability negotiation, authentication limits, and namespacing behavior are documented before implementing clients.

## 2. Rust workspace and desktop foundation

- [x] 2.1 Scaffold the desktop/core Cargo workspace, lock dependencies, select a Rust toolchain, and ignore build output and app data; verify both crates build and no user data or model files are included in source control.
- [x] 2.2 Add typed runtime-role, catalog/model-operation, session, model-request, tool-call, MCP, transcription-job, and metric state plus bounded worker communication; verify state-transition tests reject stale events and slow workers do not block the UI event loop.
- [x] 2.3 Build a fixture-driven shell with sessions, composer, model/effort controls, available/pulled views, runtime controls, statistics, OS-tool settings, MCP registrations, and transcription controls; verify loading, empty, ready, unavailable, and error states at supported window sizes.
- [ ] 2.4 Prove selectable streamed Markdown, code copying, Enter/Shift+Enter behavior, keyboard focus, display scaling, and macOS accessibility behavior with the selected UI stack; record the walkthrough results and revise the design before proceeding if required interaction cannot be delivered.
- [x] 2.5 Add the initial SQLite schema at `~/.rapid-mlx-ui/state.sqlite3`, serialized storage worker, owner-only permissions, saved sessions/drafts/settings, and pending-attempt transactions; verify isolated-home round trips and write-failure tests without writing real user data or falling back to another location.
- [x] 2.6 Add memory-only credentials/non-secret environment references and validated preferences; verify inference endpoints reject non-loopback addresses, registered remote HTTPS MCP endpoints are permitted separately, and persisted settings/log output contain no credentials.
- [x] 2.7 Document actual prerequisites, minimum tested macOS/Rust versions, and development commands; verify `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, and `cargo test` run successfully from a clean workspace checkout.
- [x] 2.8 Route owned caches, logs, backups, recording/temp files, and runtime staging to subdirectories of `~/.rapid-mlx-ui` through verified runtime settings or cache-disable controls; verify isolated-home tests and a different-working-directory launch create no alternate app-state store while shared model caches retain their configured locations.

## 3. Local model catalog and runtime management

- [x] 3.1 Implement cache-root precedence and custom local folders; verify fixture tests cover app overrides, Hugging Face environment overrides, default paths, and offline discovery.
- [x] 3.2 Parse snapshot identities, local metadata, indexed weights, quantization, and logical sizes with bounded reads; verify fixtures cover multiple revisions, shared symlinks, overlapping roots, broken links, missing shards, unsupported formats, unreadable directories, and unknown compatibility.
- [x] 3.3 Wire catalog refresh, location settings, selection, and activation validation into the UI; verify a missing selected model is never silently replaced and discovery leaves fixture files and metadata unchanged.
- [x] 3.4 Implement executable probing and role-specific owned-child startup with distinct chat/ASR ports, local-only, telemetry/update-disable, and watchdog controls; verify fake children capture the exact arguments/environment, reject port collisions, and block startup when offline controls are unsupported.
- [x] 3.5 Implement readiness, served-model identity checks, startup deadline, bounded logs, exit detection, and actionable failures; verify fake-process/HTTP tests cover early exit, wrong model, occupied port, timeout, log overflow, and failed startup cleanup.
- [x] 3.6 Implement explicit external loopback attachment and owned-model stop/start switching; verify external processes cannot be stopped by app controls and switching waits for active generation to terminate.
- [ ] 3.7 Implement normal shutdown, forced owned-child cleanup, and parent-watchdog coverage; verify integration tests leave no owned child/descendant running and an external test server remains alive.
- [x] 3.8 Document model discovery, runtime ownership, offline activation, and troubleshooting; verify a selected already downloaded model can start locally without model downloads and its served identity matches the UI selection.
- [x] 3.9 Add Available and Pulled model views using the runtime's JSON catalog plus direct local discovery, with search, task/modality filters, origin/freshness, and download states; verify fixtures distinguish served, available, pulled, incomplete, chat, ASR, and TTS models without relying on incompatible CLI filter combinations.
- [x] 3.10 Implement explicit pull jobs with selected cache destination, size information, determinate/indeterminate progress, cancellation, and retry; verify fake-download tests cover transfer failure, insufficient space, interrupted files, and successful local validation without triggering downloads during inference or browsing.
- [x] 3.11 Implement pulled-model deletion preview and confirmed cache-aware removal of the exact repository scope; verify synthetic-cache tests preserve retained shared blobs, datasets/spaces, arbitrary source directories, sessions, and transcripts and report actual reclaimed space.
- [x] 3.12 Add model-use locks across loaded chat/ASR services, inference jobs, and transfers; verify deletion is blocked during use, unloading requires the relevant job to stop, external services are never killed, and deleted model references become unavailable without silent substitution.
- [ ] 3.13 Document catalog sources, explicit pulls, cache locations, deletion scope, and download cancellation behavior; verify the model-management walkthrough against fixtures and temporary caches, not the user's existing weights.

## 4. Streaming chat and generation controls

- [x] 4.1 Implement version/schema/model capability negotiation and validation for system prompt, temperature, top-p, output limits, effort, and structured tool calling; verify fixtures cover supported values, normalization, unknown support, and explicit runtime rejection.
- [x] 4.2 Implement incremental Chat Completions SSE decoding; verify protocol tests cover split UTF-8/JSON boundaries, comments, multiline data, separate reasoning, tool-call argument deltas, final usage after content, terminal markers, malformed frames, and early disconnects without executing partial calls.
- [x] 4.3 Wire send to a durable pending-attempt transaction, immutable settings snapshot, and a single global active-generation guard; verify failure to save prevents dispatch and navigating sessions never redirects streamed output.
- [x] 4.4 Render answer/reasoning streams, Markdown, copyable code, model/settings metadata, and generation errors; verify incomplete code fences and long responses remain readable and output never executes embedded content.
- [x] 4.5 Implement cancellation with verified server cancellation where available and a visibly unconfirmed local-close fallback; verify retained partial output, ignored late events, terminal-state persistence, and external-server preservation.
- [x] 4.6 Implement contextual errors for authentication, capacity/context limits, model mismatch, and runtime/network failure; verify each error preserves prompt/settings/partial output and never triggers implicit truncation, substitution, or replay.
- [ ] 4.7 Document chat controls and known capability limits, then run a text-chat smoke test against an already downloaded compatible model; verify supported effort serialization and visible streaming match the compatibility profile and record any unavailable optional features.

## 5. Durable sessions and conversation history

- [x] 5.1 Complete session create/list/open/rename/delete UI and durable drafts/model/settings restoration under `~/.rapid-mlx-ui`; verify restart tests preserve session independence and missing models remain explicitly unavailable.
- [x] 5.2 Implement the conversation tree, branch selection, prior-prompt editing, and sibling response retries; verify earlier descendants remain available on their original branch and outgoing context contains only the selected eligible history.
- [x] 5.3 Implement unresolved-turn actions for retry, edit, and branching from an earlier completed turn; verify partial answers and stored reasoning never enter later request context silently.
- [ ] 5.4 Add streaming checkpoints and terminal flushes through the storage worker; verify fake-clock and crash/reopen tests restore the last durable partial content as interrupted without automatic dispatch.
- [x] 5.5 Implement storage-failure presentation, schema-version handling, and backup-before-upgrade behavior; verify full/unwritable storage preserves recoverable memory state and newer incompatible databases are never reset.
- [x] 5.6 Implement confirmed inactive-session deletion with dependent history/usage cleanup; verify active-session deletion is blocked and model files plus unrelated sessions remain unchanged.
- [ ] 5.7 Document branching, retry settings, cancellation recovery, local data location, privacy, and deletion behavior; verify the documented restart/edit/retry/delete walkthrough against the desktop app.
- [x] 5.8 Add durable tool-call/model-request records, MCP registration selections, and transcript references to sessions; verify round-trip/recovery tests restore these records and classify unfinished side effects as interrupted or outcome-unknown without automatically executing them again.

## 6. System and token statistics

- [x] 6.1 Implement worker-based system CPU/memory and separate app/owned-chat/owned-ASR process sampling; verify synthetic providers and local readings have correct scopes, units, and timestamps and external process usage is not guessed.
- [x] 6.2 Add optional verified runtime memory/timing adapters and the explicit unavailable GPU state; verify Metal memory is never labeled GPU utilization and unsupported providers do not require elevated privileges.
- [x] 6.3 Implement one-second sampling, bounded five-minute histories, no overlapping provider calls, and stale states; verify fake-clock tests cover retention, slow providers, three missed intervals, and sampler shutdown.
- [x] 6.4 Persist per-model-request usage and client/server timing with source labels; verify cumulative updates replace prior counts, reasoning breakdowns are not double-counted, missing usage stays unknown, and throughput uses the documented denominator.
- [x] 6.5 Aggregate turn/session usage across every stored branch/attempt and its distinct model-request steps; verify retries, tool-result continuations, cancelled/failed attempts, branch selection, repeated updates, and restart preserve known subtotals and missing-count indicators.
- [x] 6.6 Wire charts/statistics into the desktop UI and document sampling/measurement definitions; verify stats remain usable during streaming and every visible value has a correct unit, scope, source, or unavailable/stale label.

## 7. Basic OS tools and tool-enabled turns

- [x] 7.1 Implement per-session tool enablement, working directory, filesystem grants, and mutation/command policy; verify disabled tools, out-of-scope paths, symlink escapes, and model-generated permission changes cannot authorize execution.
- [x] 7.2 Implement bounded directory listing, file reading, filename/text search, file creation, and file writing; verify temporary-filesystem tests cover errors, truncation, scoped writes, and preservation of unrelated files.
- [x] 7.3 Implement explicit executable/arguments/cwd command execution with timeout, stop, bounded output, and exit status; verify fixture commands cover success, failure, output overflow, owned-child cleanup, and cancellation without implicit elevation.
- [x] 7.4 Implement validated tool dispatch and model-result continuation using complete call IDs, provider namespacing, serial calls, and the eight-cycle turn limit; verify malformed/partial calls never execute, results match their calls, unsupported models receive no tools, and step limits terminate visibly.
- [x] 7.5 Persist tool intent before side effects and terminal results afterward, including intermediate model-request usage; verify crash/reopen and branch/retry tests never automatically replay uncertain commands or count a tool-enabled model step twice.
- [x] 7.6 Add visible tool progress, policy review when required, errors, cancellation outcomes, and session history rendering; verify stopping a turn preserves completed side effects/results without claiming rollback or invoking another tool automatically.
- [ ] 7.7 Document the built-in tool set, session policies, command limits, and recovery semantics; verify a fixture-backed file-read/write/command conversation completes through the desktop UI within its configured scope.

## 8. MCP client connectivity and server registration

- [x] 8.1 Implement MCP registration create/edit/enable/disable/remove UI and non-secret persistence in the state database; verify restarting from another working directory restores definitions/session selections and removing a registration preserves historical call identity.
- [ ] 8.2 Implement rmcp stdio and Streamable HTTP clients with compatible negotiated discovery, pagination, and connection states; verify synthetic servers cover local processes, remote-style HTTPS, version mismatch, malformed responses, timeout, and disconnect.
- [x] 8.3 Add configured credential/environment references and current-run credential input with actionable authentication errors; verify missing/rejected/unsupported auth is reported and credentials never enter saved registrations or routine logs.
- [x] 8.4 Discover tools/resources/prompts, handle supported discovery updates, and expose per-session tool selection and explicit resource/prompt insertion; verify duplicate names remain namespaced, all pages are discovered, and disabled/disconnected tools are absent from model requests.
- [x] 8.5 Route MCP tools through the shared durable execution layer and session policy; verify argument validation, matching results, provider errors, cancelled/uncertain outcomes, usage accounting, and no automatic side-effect replay on connection failure.
- [x] 8.6 Implement explicit connect/disconnect and bounded owned-stdio cleanup, including restart behavior; verify only app-launched children stop while external HTTP servers remain alive and registration restoration alone launches no process by default.
- [ ] 8.7 Document server registration, supported transports/authentication, context sharing, and connection troubleshooting; verify an example synthetic MCP server can be registered, connected, used in chat, reopened, and removed from the UI.

## 9. Transcription and transcription-model management

- [ ] 9.1 Add an independent persistent ASR model selector with available/pulled speech-to-text filters and pull/load/unload/delete actions; verify restart, missing-model, auxiliary-asset, and in-use deletion cases without changing the chat-model selection.
- [x] 9.2 Implement the verified local multipart transcription adapter and role-specific readiness/activation, preserving missing-audio-dependency errors; verify fixtures enforce actual model identity, supported language/options, separate ports, local-only asset loading, and serialization with chat inference.
- [x] 9.3 Add supported audio-file selection and validation of format, size, and duration limits; verify fixture audio uploads never modify the imported source and invalid/empty inputs produce actionable errors.
- [ ] 9.4 Add Rust-based microphone capture/encoding, macOS permission and packaging declarations, visible recording state, stop/cancel, and state-root staging; verify recording succeeds when granted and denied/device-lost cases do not leave the microphone or recording process active.
- [x] 9.5 Persist transcription jobs, actual ASR identity/settings, text edits, and session references; verify restart restores transcripts/drafts without retranscribing and storage failures preserve recoverable in-memory text.
- [x] 9.6 Implement transcript-to-composer insertion with explicit append/replace and separate send, plus cancellation, audio-retention settings, and temporary-file cleanup; verify existing drafts survive, imported originals stay intact, and terminal/recovery cleanup affects only app-owned staging files.
- [ ] 9.7 Display transcription duration/status separately from token statistics and document the complete ASR workflow; verify a local smoke test with an explicitly selected pulled ASR model plus synthetic unavailable/cancelled cases and record the actual tested model/runtime profile.

- [x] 9.8 Add independent case-insensitive search and All/Pulled/Available/Served filtering to the transcription model picker; preserve selected and missing identities, distinguish cached catalog entries from exact local revisions, provide no-match recovery, and verify keyboard input, combined filters, narrow/wide rendering, and absence of filtering side effects with headless fixtures. Update the README and run Rust formatting, lint, tests, and strict OpenSpec validation.

- [x] 9.9 Restore loading of recognized ASR NPZ checkpoints (including Whisper Small/Tiny) according to the installed audio loader, retaining exact local identity, read-only discovery, safetensors preference, and rejection of unsupported/non-ASR or missing weights. Include the specific catalog reason in blocked-load errors. Add discovery/runtime/desktop regressions, check both already-pulled Whisper models offline without downloading or modifying weights, update compatibility/validation notes, and run Rust formatting, lint, tests, and strict OpenSpec validation.

- [x] 9.10 Add voice dictation to the chat composer with explicit record/stop/cancel, ASR readiness/setup guidance, originating-session identity, draft-preserving append without automatic send, and durable transcript links. Verify current/background draft edits, repeated updates, cancellation and late results, microphone/readiness failures, persistence failures, and restart using headless UI and worker fixtures; document native microphone/speech verification limits and run Rust formatting, lint, tests, build, and strict OpenSpec validation.

- [x] 9.11 Repair common ASR response parsing for scalar/array/absent/null language metadata without discarding valid text, duration, or segments; add safe field-specific schema errors, a synthetic-speech Qwen response fixture with provenance, HTTP/parser regressions across response variants, and an opt-in real-ASR smoke harness. Verify non-silent synthesized speech through the production Rust adapter using existing local Qwen weights offline; run Rust formatting, lint, tests, build, and strict OpenSpec validation, and update the documented evidence and limits.

- [x] 9.12 Restrict supported STT models to the exact tested Qwen3-ASR 0.6B 5-bit and Nemotron 3.5 ASR streaming 0.6B repositories; remove the Voxtral adapter and catalog supplement. Filter untested available/pulled/served STT choices, block unsupported managed startup/external attachment/jobs, and preserve saved identities, model files, and transcripts. Update docs/specs and verify allowlist, variant rejection, selection preservation, runtime guards, Rust formatting/lint/tests/build, and strict OpenSpec validation. Defer model-management UI redesign.

## 10. Integrated desktop release validation

- [ ] 10.1 Run the complete acceptance walkthrough for empty launch, fixed state root, offline discovery, available/pulled views, explicit pull/removal, chat/effort, cancellation, branching, restart recovery, statistics, and shutdown; record results and resolve failures before completion.
- [x] 10.2 Run integrated OS-tool and MCP conversations, including multi-step usage, disconnect/stop, and restart with uncertain calls; verify policies apply and no side-effecting operation is replayed automatically.
- [ ] 10.3 Run imported-audio/microphone transcription with independent ASR selection, model removal protection, transcript editing, draft insertion, and restart; verify all app-owned state/staging stays under `~/.rapid-mlx-ui` and missing assets never download during inference.
- [ ] 10.4 Exercise external inference/MCP services plus runtime loss and storage failure during work; verify saved history remains usable, partial text/outcomes are retained, and external processes/model caches change only through their explicit authorized operations.
- [ ] 10.5 Produce a reproducible local Apple Silicon macOS app bundle and verify launch outside the development terminal, executable discovery, recording permissions, fixed state paths, and clean child-process shutdown; signing and public publication are excluded.
- [x] 10.6 Run final Rust formatting, lint, tests, and strict OpenSpec validation and record results; verify no remaining failures or unchecked implementation tasks are represented as complete.
- [x] 10.7 Reconcile the README and compatibility notes with shipped features and known limits; verify documented build/run commands reproduce the bundle and no planned-only behavior is described as delivered.

## Workflow follow-up

- After implementation and verification, archive this change with OpenSpec and inspect the resulting main specifications.
- Keep public signing/distribution, cloud inference/history services, exposing the app as an MCP server, model conversion, image/video chat, and additional platform support in separate changes.
