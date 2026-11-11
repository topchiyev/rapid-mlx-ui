# Desktop Shell

## Purpose

Provide a coherent desktop workspace for local model conversations, runtime controls, session history, and performance information.

## ADDED Requirements

### Requirement: Unified desktop workspace
The app SHALL provide session navigation, chat, model and effort controls, runtime controls, statistics, available/pulled model views, OS tool settings, MCP server registration, and transcription controls in a desktop window on macOS with Apple Silicon.

#### Scenario: Launch with no existing data
- **WHEN** the user launches the app without saved sessions or discovered models
- **THEN** the window shows clear empty states and actions to create a session, configure local model locations, and configure the runtime
- **AND** it does not download a model or start inference automatically

#### Scenario: Navigate during a conversation
- **WHEN** the user opens model management or statistics while a response is streaming
- **THEN** the active request remains associated with its original session and navigation does not discard its content

### Requirement: Responsive background operations
The UI SHALL remain interactive during model discovery/downloads, runtime startup, persistence, streaming, tools, MCP connections, transcription, and telemetry sampling, with progress and actionable failures for each operation.

#### Scenario: Slow model scan
- **WHEN** model discovery is delayed by a slow or unreadable directory
- **THEN** the user can continue navigating and composing a draft
- **AND** the catalog shows scanning progress or the directory error

### Requirement: Keyboard and readable interaction
The app SHALL provide keyboard navigation, visible focus, standard text selection and copying, multiline composition, and readable layouts at supported window sizes and display scaling.

#### Scenario: Compose with the keyboard
- **WHEN** the user focuses the composer and presses Enter without modifiers
- **THEN** the app sends a valid non-empty message when sending is available
- **AND** Shift+Enter inserts a newline without sending

#### Scenario: Operate primary controls without a mouse
- **WHEN** the user navigates sessions, selectors, send, and stop controls with the keyboard
- **THEN** focus is visible and the focused control can be activated without losing the draft

### Requirement: Local preferences
The app SHALL persist local runtime configuration, model locations, display preferences, tool settings, MCP registrations, and transcription-model selection under `~/.rapid-mlx-ui`, validate changes before use, and report storage failures without claiming success.

#### Scenario: Reopen configured application
- **WHEN** the user restarts after saving valid preferences
- **THEN** the saved values are restored without automatically launching the runtime

#### Scenario: Invalid endpoint preference
- **WHEN** the user supplies an invalid or non-loopback inference server endpoint
- **THEN** the app explains the validation error and does not attempt a connection

#### Scenario: Register a remote MCP endpoint
- **WHEN** the user registers a valid remote HTTPS MCP endpoint
- **THEN** MCP configuration accepts it independently of the loopback-only inference endpoint restriction

### Requirement: Visible operational state
The UI SHALL distinguish loading, empty, ready, unavailable, and error states using text as well as visual indicators, and SHALL avoid presenting unavailable data as successful activity.

#### Scenario: Runtime becomes unavailable
- **WHEN** the runtime disconnects while the desktop window is open
- **THEN** runtime status explains the loss of availability and dependent controls reflect it
- **AND** stored sessions remain viewable
