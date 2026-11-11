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

## Live local text generation

The opt-in smoke harness was run against existing `mlx-community/Qwen3.5-4B-MLX-4bit` snapshot `32f3e8ecf65426fc3306969496342d504bfa13f3`. It starts a test-owned runtime on an ephemeral IPv4 loopback port with offline/download-disable controls, isolated temporary app state, the exact local weight path, and explicit served identity `rapid-mlx-ui-smoke`. No weights were downloaded and no existing server was adopted or stopped.

```sh
cargo run -p rapid-mlx-ui --bin runtime-smoke -- <existing-local-model-folder>
```

Observed result: answer `rapid-mlx-ui-smoke-ok`; runtime-reported prompt 23, completion 7, total 30 tokens; client elapsed 3,141 ms and first nonempty output 2,782 ms. These are one-run client measurements, not decode-rate benchmarks. A second request was cancelled after its first content delta and retained one byte of partial answer. Server cancellation was **unconfirmed**, correctly reported as the local-stream-close fallback. The owned runtime then stopped and was reaped. Live optional effort behavior was not certified by this test; source-backed mappings and serialization fixtures remain separate evidence.

## Desktop interaction and packaging

Both debug and optimized macOS app bundles were built; the optimized bundle launched outside the development terminal, discovered six existing local revisions and 304 runtime catalog entries, displayed system readings, navigated to Models, and created a session through native accessibility actions. Screenshots verified readable dark-theme contrast and layout; the accessibility tree exposed navigation, input controls, session text, and code-copy actions. Native automation was inconsistent for subsequent popup/keyboard operations (earlier coordinate input reported `noWindowsAvailable`), so this is not a completed native keyboard/clipboard/accessibility walkthrough.

Headless egui tests exercise Enter-to-send, Shift+Enter multiline editing, disconnected-send prevention, empty/error states, partial Markdown/code fences, and all pages at narrow and wide sizes. They complement, rather than replace, remaining native focus, selection, copying, and display-scaling checks. Bundles contain the microphone usage description and are unsigned local development builds.

## Remaining acceptance gates

- Complete the native desktop walkthrough, including selectable streamed text, clipboard/code copying, focus order, scaling, real tool conversations, and restart actions.
- Select a compatible already-pulled MLX ASR model and verify real transcription and auxiliary assets. No compatible local ASR weights were available for this check; the incomplete CTranslate2 faster-whisper cache is not treated as an MLX ASR model.
- Exercise actual microphone grant/denial/device loss, recording, and transcript insertion. Automated tests never requested microphone access.
- Finish live supported-effort verification, remote TLS MCP end-to-end coverage, abrupt parent-watchdog validation, and the complete release acceptance checklist.

Model mutation tests use temporary caches only. The user's existing model weights, imported audio, and unrelated sessions were not deleted. See [the task list](../openspec/changes/plan-rapid-mlx-desktop/tasks.md) for the full outstanding contract.
