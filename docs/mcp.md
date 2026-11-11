# MCP client backend

The core uses the official `rmcp` 3.9 SDK. Startup uses `ClientServiceExt::serve_with_lifecycle` with `ClientLifecycleMode::Auto`: prefer stateless `server/discover` using known compatible revisions newest first (starting at 2026-07-28), and fall back to legacy initialization with protocol 2025-11-25 when required. Synthetic tests verify both current stateless discovery and a legacy server returning method-not-found for discovery. The SDK bounds its discovery probe at ten seconds; the overall connection/discovery deadline remains thirty seconds. This startup fallback does not replay tool calls. Creating a manager or deserializing registrations does not launch a process or connect to a service. The application must explicitly connect enabled registrations after each launch.

## Registration and credentials

`McpRegistration` is independent of the storage/session types and supports serde JSON persistence. Its `id` is a UUID generated once by `McpRegistration::new(name, transport)`; preserve that ID on edits. Names are editable labels and need not be unique.

- `McpTransport::Stdio { command, args, env }`: launches the executable directly, without a shell. `env` maps child variable names to **source environment variable names**, never their values. The ordinary parent environment is inherited. Stderr is discarded to prevent server output from entering routine application logs.
- `McpTransport::StreamableHttp { url, bearer_token_env }`: connects to a registered endpoint. The optional credential field is an environment variable name containing the bearer token.

`McpCredentials::default().with_bearer_token(value)` supplies a token for this connection only, overriding the environment reference. `with_env(child_variable, value)` overrides an explicitly configured child variable for this run. Credentials implement neither serde serialization nor `Debug`. Never put secrets in executable arguments, names, command paths, endpoint paths, or environment-reference names: these registration fields are persisted verbatim. Registration deserialization rejects unknown fields.

MCP endpoint validation is independent of inference validation: remote **HTTPS** and loopback **HTTP** are accepted. HTTP accepts literal loopback IP addresses or `localhost`. URL userinfo, query parameters and fragments are rejected; use a credential reference instead. HTTP redirects are disabled. TLS uses the configured reqwest certificate validation; there is no insecure-certificate override.

Supported HTTP authentication is a preconfigured bearer token. Missing references, empty/invalid header credentials, rejected credentials, and incompatible startup have actionable sanitized errors. OAuth discovery, browser login, dynamic registration, token refresh, custom authentication schemes, and interactive server input are unsupported. Reconnect explicitly after replacing credentials. Environment values are resolved at connect time and are never copied into registration JSON. This module emits no routine logs. Keep SDK/HTTP tracing at warning or off in application disk logs: verbose third-party transport tracing is outside this module's redaction boundary.

## Public API

All methods run on the application's async worker, independently of its rendering and database layers. `McpManager` is cloneable; clones refer to the same connections.

```rust,ignore
let manager = McpManager::new();
let info = manager.connect(registration, McpCredentials::default()).await?;
let snapshots = manager.connections().await;
let updated = manager.refresh(&info.id).await?;
let selected = std::collections::BTreeSet::from([updated.tools[0].id.clone()]);
let tools = manager.selected_tools(&selected).await;
let definitions: Vec<_> = tools.iter().map(McpTool::openai_definition).collect();
let result = manager.call_selected_tool(
    &selected, &tools[0].id, serde_json::json!({}),
    tokio_util::sync::CancellationToken::new(),
).await?;
manager.disconnect(&info.id).await?;
manager.disconnect_all().await;
```

`McpConnectionInfo` contains `id`, `name`, `state`, `protocol_version`, negotiated `server_info` (including capabilities), `tools`, `resources`, `prompts`, `discovery_changed`, and a sanitized optional `error`. States are disconnected, connecting, connected and failed. Polling snapshots detects a closed SDK transport. Failed and disconnected entries remain inspectable in the manager; persistent registry removal is the application's responsibility. Reconnecting the same ID first closes its old connection.

`McpTool` contains `id`, `server_id`, `server_name`, original `name`, optional `description`, and `input_schema`. Its OpenAI definition has `type: function` and the model-safe namespaced ID as `function.name`. UUID-v5 IDs derive from the stable registration UUID and original tool name, so display-name edits, pagination order, and adding tools never silently reroute old selections. Different registrations advertising the same name receive distinct IDs.

`selected_tools(&BTreeSet<String>)` returns only explicitly selected tool IDs from current, connected discovery. Empty means no tools. Dirty or failed discovery is excluded until refreshed. Persist these IDs per session; do not confuse them with registration IDs. The parent may project a selected server into its current tool IDs if its UI selects whole providers.

`call_selected_tool(selected, id, arguments, cancellation)` checks session selection and delegates to `call_tool(id, arguments, cancellation)`. The latter is a low-level routing API: its caller must enforce session permissions. Both validate complete arguments against the server's JSON schema before dispatch. External schema references are rejected without network fetching. No fallback server is selected. The SDK result preserves structured/text content and `isError` provider failures; it is not automatically interpreted as a successful tool outcome.

`read_resource(server_id, uri)` and `get_prompt(server_id, name, arguments)` fetch explicitly requested, currently advertised context. Prompt arguments are an object of strings. These methods return the SDK's typed results. Attach the original server ID/name and resource URI/prompt name when displaying or saving results. Returned context must not grant permissions. Resource templates, resource subscriptions, and interactive input are not exposed in the first backend API.

Discovery follows all tool/resource/prompt pages, bounded at 128 pages per category and 4,096 combined entries. Duplicate tool names from one server, repeated cursors, malformed pages, and overflow fail discovery rather than publishing an incomplete list. Refresh issues typed SDK requests without falling back to stale cached lists. Legacy list-change callbacks and current-protocol subscriptions set `discovery_changed`; call `refresh_changed()` on the application worker to refresh marked connections, or `refresh(id)` explicitly. No background polling network calls occur merely because a registration exists.

## Execution, cancellation, and cleanup

Connect/discovery and individual operations have a 30-second deadline. Disconnect has a 10-second SDK cleanup bound. Only launched stdio children are owned: the SDK closes stdin, waits, terminates/kills and reaps that child as needed. Dropping active SDK services also schedules cleanup, but the application should await `disconnect_all()` before shutting down Tokio. HTTP disconnect closes the client/session and never terminates the HTTP server process. Commands that daemonize or create detached descendants should provide their own lifecycle management; this API does not adopt or kill unrelated processes.

The backend never retries tool calls. HTTP session-expiry replay is explicitly disabled; interactive-input results are rejected using the SDK's single-response call API rather than automatically continuing. Cancellation before dispatch reports a known cancelled operation. Cancellation, timeout, or transport loss after dispatch reports `McpError.outcome_unknown = true`: the operation may have applied side effects. Cancellation does not promise remote rollback or immediate server termination. A JSON-RPC provider rejection is a known failure; a returned tool `isError` is preserved.

The parent must persist intent and call identity before dispatch, record results/unknown outcomes afterward, enforce the session's execution policy and model capability gating, correlate results with the originating model call ID, and apply the shared tool-loop step limit. The manager sends only explicit tool arguments or selected resource/prompt requests, never entire session histories. Recovery must never replay saved calls automatically.

## Verification and boundaries

Module tests use the official SDK server on ephemeral loopback HTTP ports and synthetic fixtures under `fixtures/mcp/`. They cover pagination for all three categories, namespacing, refresh, context reads, schema checks, missing credentials, invalid executable startup, cancellation and unknown outcomes, selection gating, and survival of the external HTTP service after disconnect. No inference models or external servers are required.

Tests also cover incompatible protocol negotiation, malformed wire responses, rejected bearer authentication, real operation/connect deadlines, and reaping an owned stdio child when startup is cancelled. A live remote TLS service, real desktop selection/persistence flows, and successful stdio protocol negotiation with graceful server exit require separate integration verification. Backend tests alone do not complete the combined UI/store/chat OpenSpec tasks.

## Desktop adapter

`crates/desktop/src/mcp_bridge.rs` provides pure `registration(&McpForm)`, `form(&McpRegistration)` and `view(&McpRegistration, Option<&McpConnectionInfo>)` projections. Blank argument/environment editors default to `[]`/`{}`. JSON must contain only argument strings or environment-reference string pairs. Existing IDs survive form edits. Tool tuple IDs are the actual model-visible IDs; the parent persists them directly in per-session selections. Tuple descriptions include original server tool names. Resource/prompt tuples retain their routing URI/name and human label. Selection flags default false because session state belongs to the caller. Disabled, stale, failed, mismatched, and disconnected snapshots expose no executable tools. Adapter tests cover parsing, roundtrips, safe errors and projections.

## Real Rust stdio fixture

The [SDK-native fixture example](../crates/core/examples/mcp_fixture.rs) is a standalone Rust MCP server. It uses the same official SDK's async read/write transport over stdin/stdout, without a shell, custom protocol codec, model, or external service. Existing crate features suffice; `transport-io` is not required. Stdout contains only MCP messages.

Build and run its focused process integration test from the workspace root:

```sh
cargo build -p rapid-mlx-core --example mcp_fixture
cargo test -p rapid-mlx-core mcp::tests::real_stdio_fixture_discovers_calls_cancels_and_exits_gracefully --lib -- --ignored --exact
```

The test is explicitly ignored during ordinary `cargo test` because Cargo does not build a runnable example executable as a unit-test prerequisite. Build it first with the command above. The test resolves the executable relative to the current test binary's target/profile directory; set `RAPID_MCP_FIXTURE_BIN` to an absolute executable path if using a different output location. To test a release build, build the example with `--release` and run the test with `--release`, or provide that executable override.

For desktop registration, choose **Local stdio**, set a label such as `Rust stdio fixture`, and enter the **absolute path** to the built `target/debug/examples/mcp_fixture` executable. Use arguments `[]`, environment references `{}`, and enable the registration. Connect explicitly. Discovery should show:

- `echo`, requiring a string `message`; a call with `{"message":"hello"}` returns `hello`.
- `wait`, accepting `{}`; it remains active until the client cancels the call.
- Resource URI `fixture://readme`, with synthetic text context.
- Prompt `fixture_greeting`, with a synthetic greeting and no required arguments.

Select `echo` for the current session before advertising it to a verified tool-capable model. Select resources/prompts explicitly to include their context. The example contains no credentials and needs no environment references. Its optional `--lifecycle-dir EXISTING_DIRECTORY` argument writes synthetic test markers only to that explicitly supplied directory; ordinary UI registration can leave it off.

The executed integration test proves successful real child-process MCP discovery (including the negotiated 2026-07-28 version and fixture identity), namespaced tool definitions, Unicode echo results, resource/prompt reads, an in-flight cancellation notification reaching the child, a second successful call on the same connection after cancellation, one dispatch of the waiting operation without replay, a graceful-exit marker after SDK EOF handling, and absence of the original child PID after disconnect. It uses a temporary synthetic marker directory and no external services. The separate blocked-startup test covers cleanup before negotiation succeeds.

Cleanup evidence is limited to the launched fixture process on the supported Unix/macOS test path. This fixture spawns no descendants, so it does not establish cleanup guarantees for arbitrary server process trees or servers that daemonize. PID checks plus the fixture's exit marker demonstrate this process exited and was reaped; the manager does not expose its raw OS exit status. Cancellation receipt demonstrates protocol delivery, not rollback of arbitrary server side effects. The caller still records cancelled calls as outcome-unknown. Desktop registration/session persistence and the complete model/tool conversation flow remain parent integration checks.
