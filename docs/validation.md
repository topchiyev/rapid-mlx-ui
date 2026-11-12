# Implementation validation

Validation date: 2026-11-11. Development host: Apple Silicon, macOS 27, Rust 1.96.1, installed Homebrew rapid-mlx 1.0.2. This record distinguishes exercised behavior from remaining acceptance gates; the OpenSpec change remains active.

## Automated checks

The workspace formatting, strict all-target Clippy, and default tests passed from a clean temporary Git checkout of the implementation source: 99 core tests and 43 desktop tests, with no failures. The tests use temporary homes/caches, synthetic loopback servers, fake runtime children, and headless egui frames. They cover discovery and nested quantization checkpoints, offline startup arguments, ownership and cleanup, stream decoding, policy-scoped filesystem/command tools, durable intent/result barriers, branch/retry history, unknown outcomes, usage accounting, microphone encoder simulations, transcription multipart validation, and bounded telemetry. The isolated-home parent test invokes its otherwise ignored child test from a different working directory.

Strict OpenSpec validation passed (`npx --yes @fission-ai/openspec@1.17.0 validate --all --strict --no-interactive`). The optimized bundle was produced with `scripts/bundle.sh`; its Info.plist passed `plutil -lint`.

The default suite deliberately excludes tests requiring a prebuilt executable. Run these explicit stdio checks after building the fixture:

```sh
cargo build -p rapid-mlx-core --example mcp_fixture
cargo test -p rapid-mlx-core mcp::tests::real_stdio_fixture_discovers_calls_cancels_and_exits_gracefully --lib -- --ignored --exact
cargo test -p rapid-mlx-ui backend::integration_tests::stdio_mcp_turn_commits_results_usage_and_cancelled_outcomes_without_replay --bin rapid-mlx-ui -- --ignored --exact
cargo test -p rapid-mlx-ui backend::integration_tests::shutdown_drains_active_stdio_tool_outcome_before_disconnect_and_restart --bin rapid-mlx-ui -- --ignored --exact
```

All three passed. The first verifies real SDK child-process discovery, Unicode calls, resources/prompts, cancellation, connection reuse, graceful exit, and child PID cleanup. The second drives actual Worker commands, SQLite persistence acknowledgments, and the turn runner through a real stdio MCP echo/wait server plus three synthetic chat requests. Two successful model steps report 9 prompt + 7 completion = 16 tokens. The subsequent cancelled step preserves an outcome-unknown tool result; reopening restores all three steps (21 reported total tokens), selections, provider/call identities, and history without connecting or replaying anything. The third invokes actor shutdown during an active stdio call and verifies a populated terminal tool result is committed before graceful disconnect and PID cleanup. These checks do not exercise a remote TLS network or native UI interaction.

Transcription picker filtering follow-up (2026-11-11): `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace` (99 core and 45 desktop tests passed; four existing tests ignored), and strict OpenSpec validation passed in the working checkout. Two added headless tests cover case-insensitive name/repository/identity search, combined availability filtering, exact local versus cached catalog entries, exclusion of chat/TTS entries, keyboard search, actual availability-dropdown and Clear filters clicks, match/empty feedback, selected/missing identity preservation, independence from the Models page, and absence of dispatched filtering commands at narrow/wide sizes during idle, active, and error states. After adding an accessibility label, formatting, Clippy, and all five headless frontend tests passed again. No native desktop walkthrough or live ASR inference was performed for this follow-up.

## Live local text generation

The opt-in smoke harness was run against existing `mlx-community/Qwen3.5-4B-MLX-4bit` snapshot `32f3e8ecf65426fc3306969496342d504bfa13f3`. It starts a test-owned runtime on an ephemeral IPv4 loopback port with offline/download-disable controls, isolated temporary app state, the exact local weight path, and explicit served identity `rapid-mlx-ui-smoke`. No weights were downloaded and no existing server was adopted or stopped.

```sh
cargo run -p rapid-mlx-ui --bin runtime-smoke -- <existing-local-model-folder>
```

Observed result: answer `rapid-mlx-ui-smoke-ok`; runtime-reported prompt 23, completion 7, total 30 tokens; client elapsed 3,141 ms and first nonempty output 2,782 ms. These are one-run client measurements, not decode-rate benchmarks. A second request was cancelled after its first content delta and retained one byte of partial answer. Server cancellation was **unconfirmed**, correctly reported as the local-stream-close fallback. The owned runtime then stopped and was reaped. Live optional effort behavior was not certified by this test; source-backed mappings and serialization fixtures remain separate evidence.

## Live Qwen ASR and the common response contract

On 2026-11-12, a test-owned rapid-mlx 1.0.2 server using the already-pulled `mlx-community/Qwen3-ASR-0.6B-5bit` revision `782e765bd462bbe565586d6e55588a3bf0e3750a` transcribed non-silent speech synthesized locally with macOS `say`: “This is a local speech recognition test.” The raw response was HTTP 200 with the correct text, `language: ["English"]`, duration 2.5525625 seconds, and timestamped segments. This reproduced our parser bug: it required scalar language metadata and rejected a valid transcription. The captured response is the [Qwen fixture](../fixtures/runtime/asr-qwen3-response.json); provenance identifies synthetic test speech, not user audio.

After fixing the common parser, the production Rust `TranscriptionClient` passed the same speech check through the new opt-in harness, including exact expected-text comparison. Observed output: 40 text characters, detected language `English`, duration 2.5525625 seconds, client elapsed 496 ms. This is one local test, not a performance benchmark. Both diagnostics and the harness avoid printing user transcript content. The owned runtime used an ephemeral loopback port and offline/download-disable settings, was stopped and reaped, and removed temporary state. Weights, existing app sessions, and the user's running service were untouched. No model downloads or microphone access occurred.

To repeat with an existing local model and non-silent PCM WAV:

```sh
cargo run -p rapid-mlx-ui --bin asr-smoke -- <existing-local-ASR-folder> <speech.wav>
# Optionally add the expected transcript as one quoted argument.
```

All STT families share this HTTP/parser path. The parser accepts scalar/array/null/absent language metadata and preserves text, duration, and segments. This positive Qwen speech check does not certify every STT model or resolve the separate Whisper processor dependency failure below.

Final checks for the response fix passed: `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace --quiet` (103 core and 54 desktop passed; four existing tests ignored), `cargo build -p rapid-mlx-ui`, and strict OpenSpec validation. The HTTP regression runs both scalar-language and captured Qwen array-language responses through the same production client; additional cases cover empty/multiple/null/absent languages and safe errors for invalid JSON, text, language, duration, and segments.

## Live Whisper loading

The NPZ fix passed `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace` (101 core and 46 desktop tests passed; four existing tests ignored), and strict OpenSpec validation. Regression checks cover Small/Tiny layouts, correct NPZ sizes, read-only revalidation, safetensors preference, missing/empty/broken/nested weights, non-ASR exclusion, offline runtime arguments, desktop selection, and specific blocked-load diagnostics.

NPZ compatibility follow-up (2026-11-11): the app's actual `RuntimeSupervisor` and transcription client were checked with already-pulled `mlx-community/whisper-tiny-mlx` revision `6caf9c55601caafbe6508a8b0d216bdf4783c4e8` (74,418,540 NPZ bytes) and `mlx-community/whisper-small-mlx` revision `45f3915923c7a79a5a5b5a7d909d39aeb0e5630e` (481,307,592 NPZ bytes). Both were recognized as ASR load candidates, started on separate ephemeral loopback ports with offline/download-disable settings, passed health/served-identity/audio-route checks, and completed a synthetic one-second silent PCM WAV upload with an empty transcript and HTTP 200. This exercises lazy weight loading and transport, not speech recognition accuracy. Each test-owned runtime was stopped and reaped; temporary state/audio stayed under `~/.rapid-mlx-ui/cache` and was removed. Existing models and external services were untouched, and no weights or processor assets were downloaded.

The runtime emitted warnings that the snapshot lacked a Whisper processor and its dimension-based `openai/whisper-base` fallback was not cached; it also reported unavailable optional Silero VAD assets. Silent audio does not prove non-silent transcription works without these assets. Actual speech transcription and native UI ASR interaction remain acceptance gates; processor selection/loading is a separate rapid-mlx integration limitation.

## Desktop interaction and packaging

Chat dictation follow-up (2026-11-12): formatting, strict all-target Clippy, workspace tests (101 core and 54 desktop passed; four existing tests ignored), debug build, and strict OpenSpec validation passed. Eight new worker/headless UI tests cover chat record/stop/setup/cancel controls, keyboard edits during recording, current/background draft merging, fixed originating session, no automatic send, duplicate snapshots/commands, queue backpressure, empty transcripts, unready ASR, simulated microphone denial, cancelled late transcription results, failed transcript/session saves, and restart without dispatch or reinsertion. The demo executable launched and was stopped, but native automation could not bind its unbundled window. No real microphone permission or speech test was performed, and the Whisper processor warning above remains unresolved.

Both debug and optimized macOS app bundles were built; the optimized bundle launched outside the development terminal, discovered six existing local revisions and 304 runtime catalog entries, displayed system readings, navigated to Models, and created a session through native accessibility actions. Screenshots verified readable dark-theme contrast and layout; the accessibility tree exposed navigation, input controls, session text, and code-copy actions. Native automation was inconsistent for subsequent popup/keyboard operations (earlier coordinate input reported `noWindowsAvailable`), so this is not a completed native keyboard/clipboard/accessibility walkthrough.

Headless egui tests exercise Enter-to-send, Shift+Enter multiline editing, disconnected-send prevention, empty/error states, partial Markdown/code fences, and all pages at narrow and wide sizes. They complement, rather than replace, remaining native focus, selection, copying, and display-scaling checks. Bundles contain the microphone usage description and are unsigned local development builds.

## Remaining acceptance gates

- Complete the native desktop walkthrough, including selectable streamed text, clipboard/code copying, focus order, scaling, real tool conversations, and restart actions.
- Verify speech accuracy beyond the positive synthesized-English Qwen3-ASR check, including other languages, models, real microphone input, and auxiliary assets. Whisper Small/Tiny NPZ startup and silent-WAV checks pass, but their processor failure remains unresolved. The incomplete CTranslate2 faster-whisper cache is not treated as an MLX ASR model.
- Exercise actual microphone grant/denial/device loss, recording, and transcript insertion. Automated tests never requested microphone access.
- Finish live supported-effort verification, remote TLS MCP end-to-end coverage, abrupt parent-watchdog validation, and the complete release acceptance checklist.

Model mutation tests use temporary caches only. The user's existing model weights, imported audio, and unrelated sessions were not deleted. See [the task list](../openspec/changes/plan-rapid-mlx-desktop/tasks.md) for the full outstanding contract.
