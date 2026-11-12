# Desktop Presentation

## Purpose

Provide a distinctive, readable desktop instrument for local models through a consistent industrial visual language, focused workspace, purposeful controls, and clear operational displays.

## ADDED Requirements

### Requirement: Coherent industrial visual language
The desktop SHALL use a shared visual language inspired by the supplied teenage engineering references: subtly grainy gray surfaces at multiple lightness levels, deep directional shadows, white and black elements, sparse orange accents, and deliberate rounded and square geometry. The treatment SHALL extend across the workspace, Settings, model management, and telemetry rather than only chat colors.

#### Scenario: Review the redesigned desktop
- **WHEN** the user opens the workspace, model management, Settings, and statistics
- **THEN** the surfaces, typography, spacing, depth, controls, and restrained accent treatment form one recognizable instrument-like design
- **AND** grain and shadows do not obscure text, focus, or state indicators

### Requirement: Focused workspace and concise copy
The primary workspace SHALL prioritize conversations, the selected model, supported effort selection, composing, send/stop, dictation, and useful status. It SHALL omit decorative slogans, implementation-language labels, redundant headings, and repeated explanatory text. Essential action names, accessible names, errors, and state explanations SHALL remain discoverable.

#### Scenario: Return to an existing conversation
- **WHEN** the user opens a saved session
- **THEN** conversation content, composition, the selected model, and current activity are clear without configuration forms or repeated explanatory labels dominating the window
- **AND** the user can open model management and Settings from recognizable controls

#### Scenario: Recover from missing setup
- **WHEN** no usable model or runtime is configured
- **THEN** a concise empty state explains the blocking condition and links directly to the relevant setup
- **AND** no model is selected, downloaded, or loaded implicitly

### Requirement: Organized and scoped Settings
The app SHALL group infrequent configuration within Settings: runtime endpoints/executable/credentials, model locations, advanced generation, OS-tool policies, MCP registrations, and voice preferences. Each group SHALL identify application or session scope and preserve existing persistence and credential rules. Primary model/effort selection, explicit operational actions, and activity/cancel state SHALL remain accessible in their working context.

#### Scenario: Change generation configuration for one session
- **WHEN** the user opens advanced generation settings from a conversation
- **THEN** the panel identifies the affected session and editing preserves the session's draft and history
- **AND** settings affect subsequent dispatches without changing the recorded settings of earlier or active requests

#### Scenario: Configure services while work continues
- **WHEN** the user opens runtime, voice, tool, or MCP setup during an operation
- **THEN** the active operation retains its original session/model association and remains cancellable
- **AND** viewing Settings does not start, stop, connect, or download anything

### Requirement: Purposeful tactile controls
The interface SHALL provide tactile buttons, clear toggles, and a knob-style control where a supported bounded numeric setting benefits from it. Interactive controls SHALL expose their name, current value or state, visible focus, and keyboard operation. Numeric knobs SHALL also offer precise value entry; decorative gauges SHALL not imply that read-only metrics are adjustable.

#### Scenario: Operate a numeric control without dragging
- **WHEN** the user focuses a knob-style setting with the keyboard
- **THEN** they can adjust and directly enter a valid value within its supported range
- **AND** disabled or unsupported controls explain their unavailability without pretending a change was applied

### Requirement: Instrument-style system widget
The workspace SHALL expose one coherent system widget with distinct system CPU and used/total memory displays, compact recent-history or meter graphics, and concise runtime activity/ownership state. It SHALL provide access to detailed statistics, preserve units and measurement scope, and distinguish live, stale, and unavailable readings without relying on color alone.

#### Scenario: Monitor generation
- **WHEN** a response streams while CPU and memory samples arrive
- **THEN** the widget updates without blocking composition or navigation and labels CPU as system percent and memory with used/total units
- **AND** opening details exposes sample age/source and separates app/owned-runtime process usage from system usage

#### Scenario: Samples stop or GPU measurement is absent
- **WHEN** readings become stale under the existing sampling contract or no verified GPU utilization is available
- **THEN** the corresponding display identifies stale or unavailable data instead of displaying simulated activity or substituting zero
- **AND** Metal memory is never presented as GPU utilization

#### Scenario: Open request and session statistics
- **WHEN** the user expands usage details from the widget or conversation
- **THEN** runtime-reported counts, partial totals, unknown requests, and client timing remain distinguishable under the existing observability contract
- **AND** retries, branches, partial attempts, and tool continuations retain their existing accounting semantics

### Requirement: Clear model workflow
Model management SHALL present searchable Available/Pulled inventory and contextual details/actions with explicit role, exact identity/revision, compatibility, local/served status, and operation progress. Selection, pull, load, unload/disconnect, and removal SHALL remain distinct actions. Chat and transcription choices SHALL remain independent and filtered choices SHALL preserve an existing selection.

#### Scenario: Choose an available model
- **WHEN** the user selects a catalog model without local weights
- **THEN** the interface clearly presents its availability and an explicit Pull action, without claiming it is locally ready
- **AND** loading remains a separate action after local validation

#### Scenario: Select a transcription model
- **WHEN** the user opens transcription model management
- **THEN** the interface identifies the transcription role, preserves the chat selection, and applies the current tested STT policy
- **AND** search/filter controls and details distinguish exact local revisions from catalog or externally served identities

#### Scenario: Operate on a loaded or external model
- **WHEN** the selected model is loaded, busy, externally served, or targeted for removal
- **THEN** contextual actions show what is possible and explain blocked operations
- **AND** deletion retains its scope preview and confirmation, external disconnect leaves the server running, and session deletion remains separate from model deletion

### Requirement: Readable states and accessible operation
The redesigned desktop SHALL preserve readable text selection/copying, Enter/Shift+Enter composition, visible keyboard focus, and keyboard access to primary controls, Settings, model actions, and statistics details. Loading, empty, error, streaming, cancellation, stale, and unavailable states SHALL remain understandable at supported window sizes and display scaling.

#### Scenario: Use a compact window during a failure
- **WHEN** the window is resized or scaled while a model operation fails or a generation is cancelled
- **THEN** content and essential actions remain reachable without overlap or clipped critical controls
- **AND** the user can read the failure or partial-result state, retry explicitly, or continue navigating with the keyboard
