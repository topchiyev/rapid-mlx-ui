# Chat transport and built-in tools

The core [chat module](../crates/core/src/chat.rs) and [tool module](../crates/core/src/tools.rs) are independent of the database and desktop domain types. All HTTP probes and test traffic use numeric loopback addresses. No model, package, or remote service is downloaded by these modules.

## Integration API

`chat::stream_chat(endpoint, credential, request, sender, cancel).await -> anyhow::Result<ChatOutcome>` accepts a `serde_json::Value` Chat Completions request and a bounded `tokio::sync::mpsc::Sender<ChatEvent>`. The transport sets `stream: true` and `stream_options.include_usage: true` and returns the resulting exact serialized request in the outcome. Use `prepare_request(request)` to create this immutable wire snapshot and persist it before dispatch; the transport applies the same idempotent preparation. Credentials belong only in the separate memory-only argument.

Events are `Identity { id, model }`, `Content(String)`, `Reasoning(String)`, `Usage(Value)`, and `ToolCalls(Vec<ToolCall>)`. Each `ToolCall` has `id`, `name`, and complete object-valued `arguments`. The returned outcome, rather than a channel terminal event, is authoritative. The caller must consume events concurrently with awaiting the outcome, so a full bounded channel can apply backpressure. Dropping the receiver fails the attempt. Cancelling the `CancellationToken` interrupts network waits and channel backpressure.

`ChatOutcome` retains answer/reasoning separately, model, server request ID, wire request, final cumulative raw usage, finish reason, monotonic elapsed/first-output milliseconds, completed/cancelled/failed status, error, and `server_cancel_confirmed`. Streaming, HTTP, malformed-frame, disconnect, output-limit, and deadline failures return an outcome with the received partial text. Invalid endpoint/request configuration returns `Err`. There are no automatic retries or model substitutions. Credentials are never inserted into the outcome or routine logs; these modules do not emit logs. Outcomes and error bodies can contain sensitive session text and must not be logged routinely.

Only successful completed steps expose executable tool calls in the returned outcome. A `ToolCalls` event is emitted only after the finish reason, `[DONE]`, complete JSON arguments, and unique nonempty call IDs have been verified. The parent must still require a completed outcome and reject late events after its own terminal transition. A cancelled or failed attempt's partial text is inspectable history, not implicit assistant context.

`negotiate(endpoint, credential)` probes health, OpenAPI, and served model identities. `negotiate_for_runtime(endpoint, credential, Some(probed_version))` additionally takes the **separately verified executable package version**. `Capabilities.schema_version` stores OpenAPI `info.version`; `runtime_version` stores that separate evidence. Explicit external attachments whose executable cannot be identified should use `None`. A process version is relevant only when the parent has established that the endpoint belongs to that process.

`validate_request(&request, &caps, &effort_profile, tools_verified)` checks model membership, nonempty history, advertised optional fields, supported sampling bounds, positive output limits, effort evidence, and tool gating. `EffortProfile::Unknown` disables effort. `ThinkingBudget` and `NativeLevels(Vec<String>)` are evidence supplied by the caller for the **actual selected served template/model**, not guesses from a model ID, parser name, or schema acceptance. Native reasoning override fields are deliberately excluded from this validated builder to prevent conflicting normalization. The low-level JSON transport remains usable by a separately validated integration.

Inference endpoint validation accepts HTTP(S) server origins or `/v1`, with numeric loopback IP addresses only. `localhost` is rejected to avoid DNS rebinding; use `127.0.0.1` or `[::1]`. Embedded credentials, queries, fragments, arbitrary paths, proxies, and redirects are excluded. This restriction applies only to inference, not separately registered MCP servers.

## Verified rapid-mlx 1.0.2 contract

Read-only source inspection used `/opt/homebrew/Cellar/rapid-mlx/1.0.2/libexec/lib/python3.15/site-packages/rapid_mlx/` on 2026-11-11. Those source checks establish adapter mappings, not successful inference for arbitrary weights. The later Qwen3.5 4B text/cancellation smoke run is recorded separately in [implementation validation](./validation.md); live optional effort behavior remains unverified.

| Source | Evidence and behavior |
| --- | --- |
| `server.py:1452` | FastAPI advertises API version `0.6.0`, even though the installed package is `1.0.2`. Do not infer package compatibility from OpenAPI version. |
| `api/models.py:1591` | Temperature is 0 through 2 inclusive; top-p is greater than 0 through 1 inclusive. |
| `api/models.py:556` and `service/helpers.py:1975` | `none` maps to `chat_template_kwargs.enable_thinking=false`. Graded fallback budgets are minimal=256, low=512, medium=2048, high=8192, xhigh=24000 reasoning tokens. This is a cap, not a native model effort tier. |
| `service/helpers.py:2064` and `utils/chat_template.py:2106` | The served template may declare a native effort vocabulary. Graded requests then map to the closest ranked native level, ties upward, with no added budget. `max` ranks as `xhigh`. Qwen3.8's documented native set is low/medium/xhigh: minimal normalizes to low; high to xhigh. Actual template evidence is still required. |
| `service/helpers.py:2014` | Explicit native fields override the corresponding shorthand dimensions. The validator rejects such conflicting fields rather than claiming the shorthand was applied. |
| `routes/chat.py:5898` and `routes/health.py:458` | The public first SSE `chatcmpl-…` ID is the cancellation identity. POST `/v1/requests/{request_id}/cancel` returns `{object:"request.cancel",id:…,cancelled:true}` on accepted abort, or 404 for missing/finished/unsupported abort. |
| `api/models.py:3373` | SSE separates `reasoning_content`, answer `content`, and structured `tool_calls`. |

`effort_mapping("1.0.2", &profile, requested)` returns the requested wire field and separately recorded expected runtime normalization. Unknown versions/profiles/values return `None`. The application must retain both fields and normalization with the immutable attempt snapshot.

`stream_chat_with_capabilities(..., Some(&caps))` attempts explicit server cancellation only when the negotiated route and verified 1.0.2 profile permit it and a public ID was received. Local cancellation closes the response first. Confirmation requires a successful matched `{id,cancelled:true}` envelope; 404, authentication/HTTP/network errors, missing ID, and unknown versions leave `server_cancel_confirmed=false`. The bounded cancellation request can take up to two seconds. A false value means server termination is unconfirmed; parent status refresh remains necessary. No process is killed to cancel chat.

## Protocol and usage limits

SSE parsing handles arbitrary HTTP/UTF-8 fragmentation, CR/LF/CRLF, comments, multiline data, errors, reasoning, answer text, interleaved tool argument fragments, usage tails, and `[DONE]`. EOF is not success. A finish reason followed by `[DONE]` is required; `length`, `cancelled`, and other incomplete outcomes are failures with partial content retained. Tool arguments are parsed only after the model step has terminated. Partial/malformed calls never reach execution.

Frames are capped at 1 MiB; assembled text/tool fragments at 16 MiB; received wire data at 32 MiB. Each stream has a one-hour deadline. Metadata responses are capped at 4 MiB with ten-second request timeouts. Repeated cumulative usage replaces prior usage. Missing counts remain unknown; no tokenizer estimates are invented. Raw reported reasoning usage remains a completion-token breakdown, not an additional count to sum. Elapsed time starts immediately before HTTP dispatch and ends at terminal handling; first output is the first nonempty answer/reasoning delta. Parent accounting sums each distinct model-request step once, including retries and known failed/cancelled usage, and labels tool execution duration separately.

## OS tool policy and dispatch

`ToolPolicy::default()` disables every tool. Configure roots and a working directory before constructing `ToolExecutor::new(policy)`. `definitions()` on the executor advertises only the selected `os_list`, `os_read`, `os_search`, `os_create`, `os_write`, and `os_command` definitions. The module-level `definitions()` lists all six schemas. Unknown argument fields are rejected.

`execute(ToolCall, CancellationToken).await -> ToolResult` executes once and returns the original `call_id`, name, terminal status, structured output, optional error, elapsed milliseconds, and conservative `side_effects_possible`. Results have completed/denied/failed/cancelled/timed_out statuses. `result.message()` produces a Chat Completions tool-result message with the matched `tool_call_id`. The parent should show pending/running before invoking and persist the terminal result afterward.

Relative paths resolve against the fixed configured working directory. Absolute paths must resolve under a granted canonical root. Parent traversal and dangling symlinks are denied; normal existing symlinks may resolve only within the grants. Files and directories are subsequently opened relative to pinned root handles, refusing symlinks at each traversed component. This prevents a concurrent ancestor/final symlink swap from bypassing the earlier canonical check. Recursive search skips symlinks, checks cancellation/deadlines, limits traversal to 10,000 entries, and bounds file reads/results; skipped tails have explicit truncation. File creation never overwrites. Writing requires `allow_mutations`; errors may leave partial writes and are never represented as rollback. Filesystem work runs on blocking workers. An already started filesystem write is awaited to establish its actual outcome rather than abandoning a worker that could mutate after reporting cancellation. Slow filesystem syscalls are not forcibly interrupted.

Descriptor-relative filesystem support targets Unix (verified on this Apple Silicon macOS host); other targets fail closed. This is pathname grant enforcement, not a protection against hard links, changes by already authorized commands, or the user's own simultaneous edits.

Commands require an absolute executable, explicit argument array and cwd. A `CommandGrant` pins a canonical executable path, exact argument prefix, whether extra arguments are allowed, and whether the operation can mutate. Granting a shell or interpreter explicitly authorizes that program's semantics; no shell is inserted implicitly. Cwd must resolve under filesystem roots, but command execution is **not an OS filesystem sandbox**. Arguments can authorize broader access according to the trusted command grant. Mutation-classified commands also require `allow_mutations`. Elevation helper executables are rejected; no elevation prompt or helper is inserted.

Defaults cap commands at 60 seconds and 1 MiB combined stdout/stderr; configurations are bounded at ten minutes and 16 MiB. Output is drained after truncation so pipes do not block execution. Results retain exit code, stdout/stderr and truncation. Non-UTF-8 directory names are displayed lossily with an explicit `path_lossy` flag rather than panicking a worker. On cancellation, timeout, or process exit, Unix cleanup kills only the command's private owned process group, waits for the direct child, and bounds pipe-task cleanup. Commands that deliberately detach into a new session/process group are outside this cleanup guarantee; grants should exclude daemons/detaching programs. Completed side effects are never rolled back. Background descendants remaining in the original group are cleaned even if their direct parent already exited.

## Parent orchestration responsibilities

1. Acquire the single global chat/ASR inference guard; commit the prompt, immutable wire settings, and pending attempt before HTTP dispatch.
2. Use only eligible selected-branch completed history. Persist answer/reasoning checkpoints separately. Match every sender/outcome to the originating session and attempt.
3. Advertise enabled tools only with verified per-model support. On a completed tool step, commit all complete call intents before execution. Execute serially, validating through the executor or the selected MCP provider.
4. Append the complete assistant `tool_calls` envelope with original IDs and JSON-serialized argument strings, then each matched result message. Continue with the same model/settings snapshot. Stop after `chat::MAX_TOOL_CYCLES` (8) model/tool cycles.
5. Propagate Stop to model and tools. Do not continue after cancellation, storage failure, uncertain side effects, a failed model step, or the cycle limit. Do not replay side effects on retry/restart.
6. Commit terminal outcomes and usage per distinct model request. Recovery marks unfinished calls interrupted/outcome-unknown without dispatching them again.

The modules intentionally do not enforce the database transactions, global guard, UI progress, MCP dispatch, or continuation loop. These remain integration work; backend tests do not establish desktop acceptance or live-model compatibility.

## Fixtures and validation

Every file under [fixtures/chat](../fixtures/chat) is **synthetic**, derived from the inspected source contracts rather than a user conversation or live inference capture:

- `answer-tools.sse`: separate Unicode reasoning/answer, fragmented structured arguments, tool finish, usage tail, DONE.
- `multiline.sse`: comments and multiline JSON data; tests transform line endings.
- `partial.sse`: deliberately incomplete arguments, forbidden from execution.
- `openapi.json`: minimal supported schema subset, including the actual API/package version distinction.
- `cancel-supported.json` and `cancel-unavailable.json`: matched confirmation and unsupported/finished fallback.

Module tests exercise every chunk width of the Unicode/tool fixture, invalid UTF-8/JSON, frame limits, malformed tails, early disconnect, cancellation during backpressure, metadata negotiation, confirmed/unconfirmed cancellation, source effort normalization, temporary scoped filesystem operations, symlink escape/swaps, repeated directory reads, exact command grants, bounded output, timeout/cancellation, and owned descendant cleanup. They use temporary files, tiny commands, and loopback fake servers, with no weights or network downloads.

Run `cargo test -p rapid-mlx-core chat::` and `cargo test -p rapid-mlx-core tools::`. Full-workspace formatting, lint, tests, OpenSpec tracking, desktop integration, and an explicit selected-local-model smoke test remain the parent change's responsibility.
