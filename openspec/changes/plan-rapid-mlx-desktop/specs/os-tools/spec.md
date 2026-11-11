# OS Tools

## Purpose

Give tool-capable local models basic operating-system interaction through visible, scoped filesystem and command tools in desktop chat.

## ADDED Requirements

### Requirement: Session tool configuration
The app SHALL let users enable or disable OS tools per session and choose a working directory, allowed filesystem locations, and execution policy. Disabled or out-of-scope tools SHALL NOT execute, and grants SHALL NOT be broadened by model output or tool results.

#### Scenario: Enable tools for a working directory
- **WHEN** the user enables filesystem tools for a selected directory
- **THEN** the model can request those tools within that scope and the UI shows the active working directory and permissions

#### Scenario: Tool request escapes its allowed scope
- **WHEN** a path resolves outside the granted roots, including through a symlink
- **THEN** the app denies execution or asks for a new explicit grant according to the user's policy
- **AND** does not widen access automatically

### Requirement: Basic filesystem tools
The app SHALL provide directory listing, bounded file reading, filename/text search, file creation, and file writing within allowed roots. Results SHALL identify paths and truncation, and write operations SHALL honor the session's mutation policy.

#### Scenario: Read and search files
- **WHEN** the model requests an enabled read, list, or search operation on an allowed path
- **THEN** the tool returns structured results with bounded output and explicit errors or truncation

#### Scenario: Write a file
- **WHEN** a complete write request targets an allowed path and meets the mutation policy
- **THEN** the UI shows the target and outcome and the new contents are written without changing unrelated files

### Requirement: Bounded command execution
The app SHALL provide a command tool with explicit executable, arguments, working directory, timeout, cancellation, bounded stdout/stderr, and exit status. Command execution SHALL use the session's configured policy and SHALL NOT request elevated OS privileges implicitly.

#### Scenario: Run a command
- **WHEN** an enabled command request satisfies the session policy
- **THEN** it runs in the displayed working directory and returns its exit status and bounded output

#### Scenario: Command exceeds its timeout
- **WHEN** a command runs longer than its execution limit
- **THEN** the app stops its owned command process, records a timeout, and keeps chat responsive
- **AND** does not claim any completed side effects were rolled back

### Requirement: Structured and visible tool execution
The app SHALL validate complete structured tool calls and argument schemas before dispatch, match every result to its call ID, and display pending, running, completed, denied, failed, or cancelled outcomes. It SHALL bound tool-loop steps and surface model/tool failures without executing partial calls.

#### Scenario: Stream contains incomplete tool arguments
- **WHEN** a streamed tool call ends before valid complete arguments are available
- **THEN** no OS operation runs and the turn shows a tool-call error

#### Scenario: Tool loop reaches its step limit
- **WHEN** repeated model/tool exchanges reach the configured limit
- **THEN** execution stops with an explanation and preserves the completed calls and results

### Requirement: Durable outcomes without repeated side effects
The app SHALL persist tool calls and available results in their originating session. On interruption or restart it SHALL retain uncertain outcomes explicitly and require a new user action before re-executing a command or write.

#### Scenario: Restart after an uncertain command outcome
- **WHEN** a command may have run but no final result was committed before the app exited
- **THEN** the session shows an outcome-unknown call and the command is not automatically retried
