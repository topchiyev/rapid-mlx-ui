# Model Chat

## Purpose

Provide interactive text conversations with the selected local model while preserving generation settings, partial output, and honest capability reporting.

## ADDED Requirements

### Requirement: Explicit model and effort selection
The app SHALL let users select a model and offer only reasoning effort controls supported by the verified runtime/model combination. Each request SHALL retain its exact model, requested settings, serialized settings, and any known runtime normalization.

#### Scenario: Supported effort is selected
- **WHEN** the user selects an advertised, verified effort value and sends a message
- **THEN** the request uses its verified parameter mapping and the response displays the selected effort
- **AND** any known mapping to a native level or token budget is explained rather than represented as an identical native setting

#### Scenario: Effort capability is absent or unknown
- **WHEN** a selected model lacks verified effort support
- **THEN** the UI explains that effort selection is unavailable and sends no invented effort parameter
- **AND** basic text chat remains available if its capabilities are verified

### Requirement: Configurable text requests
The app SHALL support a session system prompt and supported temperature, top-p, and output-token limits. It SHALL validate settings and non-empty input before sending and SHALL prevent overlapping generations in the initial release.

#### Scenario: Send a valid turn
- **WHEN** the user submits a non-empty message with a ready matching model and valid settings
- **THEN** one generation begins using the selected session branch and a snapshot of its current settings
- **AND** those request settings remain fixed if controls are later changed

#### Scenario: Context exceeds model capacity
- **WHEN** the app detects a context limit or the runtime rejects a request for exceeding it
- **THEN** the user sees an actionable error without silent removal of prior messages

#### Scenario: Another session is already generating
- **WHEN** the user tries to send while any session has an active generation
- **THEN** the UI identifies the active session and requires that generation to finish or be stopped

### Requirement: Streaming response presentation
The app SHALL render answer content incrementally with Markdown, selectable text, and copyable code blocks. It SHALL present any runtime-provided reasoning content in a separately labeled collapsible area, and SHALL render model output without executing embedded content.

#### Scenario: Receive answer and reasoning deltas
- **WHEN** the runtime streams separate answer and reasoning fields
- **THEN** each field appears in its corresponding area without duplication or mixing
- **AND** answer content remains readable while generation continues

#### Scenario: Incomplete code block during streaming
- **WHEN** a streamed response temporarily contains an unclosed Markdown code fence
- **THEN** the UI remains usable and the completed code block can be copied when it arrives

### Requirement: Cancellation and partial output
The app SHALL let users stop an active response, stop consuming its stream, and retain received output as a cancelled attempt. It SHALL request server-side cancellation when the runtime supplies a verified cancellation mechanism and request identity.

#### Scenario: User stops a response
- **WHEN** the user presses Stop during streaming
- **THEN** no later chunks change the cancelled attempt and its received output remains visible
- **AND** the UI distinguishes a server-confirmed stop from a local stream closure whose server status is unknown

### Requirement: Recoverable generation failures
The app SHALL preserve the submitted message and any partial response when HTTP errors, malformed streams, authentication errors, runtime exits, or disconnections occur. It SHALL show an actionable failure and offer explicit retry without automatically replaying the request.

#### Scenario: Stream breaks after partial output
- **WHEN** the connection closes before successful completion
- **THEN** the attempt is labeled failed or interrupted, the partial content remains, and the user can retry explicitly

#### Scenario: Request is rejected
- **WHEN** the runtime rejects a setting, authentication credential, or model selection
- **THEN** the UI shows the reason without dropping settings, switching models, or resending silently

### Requirement: Editing and retrying conversations
The app SHALL support editing a prior user prompt and retrying a response through preserved conversation branches as defined by the chat-sessions capability. New requests SHALL use only eligible completed history from the selected branch.

#### Scenario: Retry a failed turn
- **WHEN** the user retries a turn with partial failed output
- **THEN** a new attempt is created from the turn's eligible prior context
- **AND** the failed partial response is not silently submitted as completed assistant history

### Requirement: Tool-enabled conversation turns
For verified tool-capable models, the app SHALL advertise enabled built-in and MCP tools, assemble complete structured tool calls, dispatch them through the tool execution layer, and return matched tool results for continued generation. Partial tool arguments or tool-like plain text SHALL NOT trigger execution.

#### Scenario: Model requests an enabled tool
- **WHEN** a completed structured call references an enabled tool and satisfies its argument schema and session policy
- **THEN** the UI shows the tool call and its result and the model can continue with the matched result in context

#### Scenario: Tool support is unavailable
- **WHEN** the selected model/runtime lacks verified tool-calling support
- **THEN** the UI explains the limitation and omits tool definitions while preserving ordinary text chat

#### Scenario: Stop while a tool is running
- **WHEN** the user stops a turn during tool execution
- **THEN** the app requests cancellation, records whether the outcome is known, and does not automatically rerun the tool or claim its side effects were undone
