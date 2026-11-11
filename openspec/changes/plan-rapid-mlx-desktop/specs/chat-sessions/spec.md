# Chat Sessions

## Purpose

Keep local conversations durable across application restarts, including their model settings, alternate responses, partial generations, and usage history.

## ADDED Requirements

### Requirement: Persistent session management
The app SHALL create, list, reopen, rename, and delete local sessions. It SHALL persist messages, composer drafts, model identity and revision, generation settings, branch selection, tool interactions, selected MCP server registrations, transcription references, and per-attempt metadata across restarts.

#### Scenario: Reopen a conversation
- **WHEN** the user restarts and opens a saved session
- **THEN** its history, branch selection, model, and settings are restored without starting generation
- **AND** a missing model is shown as unavailable rather than replaced

#### Scenario: Create an independent session
- **WHEN** the user creates a new session
- **THEN** it begins with independent conversation history and editable settings
- **AND** messages from other sessions are not included in its request context

### Requirement: Preserve edits and retries as branches
The app SHALL preserve existing messages and attempts when a user edits a sent prompt or retries a response. Editing SHALL create a new prompt branch from the edited turn's parent; retrying SHALL create a sibling response attempt for the same prompt. Earlier descendants SHALL remain accessible on their original branch.

#### Scenario: Edit an earlier prompt
- **WHEN** the user edits a prompt that already has a response and later turns
- **THEN** a new branch starts at the edited prompt and later turns are absent from its request context
- **AND** the original prompt, response, and descendants remain accessible

#### Scenario: Retry a completed answer
- **WHEN** the user retries a completed answer
- **THEN** a new attempt uses the same prompt and eligible preceding history with the settings shown before retry
- **AND** the original answer and its descendants remain stored on their original branch

### Requirement: Attempt lifecycle and recovery
The app SHALL durably record attempts as pending, streaming, completed, cancelled, failed, or interrupted, checkpoint received content during streaming, and finalize interrupted work after an unclean restart without replaying requests automatically.

#### Scenario: Crash during streaming
- **WHEN** the app restarts with a previously active attempt
- **THEN** the last durable content is restored and the attempt is marked interrupted
- **AND** no request is resubmitted without user action

#### Scenario: Local storage fails
- **WHEN** a session write fails because storage is unavailable or full
- **THEN** the UI states that affected content is not saved and preserves the recoverable in-memory conversation
- **AND** it does not present the write as successful

### Requirement: Eligible request history
Only completed turns on the selected branch SHALL be reused as prior assistant history. Cancelled, failed, and interrupted partial answers and separately displayed reasoning SHALL remain inspectable but SHALL NOT be silently replayed. An unresolved turn SHALL require retry, edit, or branching from an earlier completed turn before appending a new user turn.

#### Scenario: Continue after cancellation
- **WHEN** the last response was cancelled
- **THEN** the UI offers retry, edit, or starting a branch from an earlier completed turn before sending another turn
- **AND** does not treat the partial answer as a completed response

### Requirement: Scoped session deletion
The app SHALL require explicit confirmation to delete a session, block deletion while that session has an active generation, and remove only its app-owned history and usage records without modifying model files or other sessions.

#### Scenario: Delete an inactive session
- **WHEN** the user confirms deletion of an inactive session
- **THEN** that session and its branches and attempts are removed from app storage
- **AND** other sessions and local model files remain intact

#### Scenario: Delete an active session
- **WHEN** the user attempts to delete a session that is generating
- **THEN** the app requires the generation to finish or be stopped before deletion can proceed

### Requirement: Local history privacy
The app SHALL store session history under `~/.rapid-mlx-ui`, exclude conversation content and credentials from routine app logs, and avoid uploading history or treating it as repository content. Only context required by an explicitly enabled tool SHALL be sent to its provider.

#### Scenario: Use and reopen local history offline
- **WHEN** the user creates and reopens sessions without internet access
- **THEN** session management works without a remote account or history synchronization service

### Requirement: Fixed application state root
The app SHALL store its owned persistent and temporary state under the current user's `~/.rapid-mlx-ui`, including its database, preferences, MCP registry, tool records, transcription records, managed audio, backups, and disk logs/caches. Existing shared model caches and explicit user export destinations SHALL retain their configured locations.

#### Scenario: Restore the same state from a different working directory
- **WHEN** the user launches the app from another directory or the desktop app bundle
- **THEN** it restores the same state from the home-relative state root without creating a second working-directory or platform-default data store

#### Scenario: State root cannot be written
- **WHEN** the app cannot create or write its state root
- **THEN** it explains the storage failure and does not silently switch to another persistent location

### Requirement: Durable tool and transcription records
Sessions SHALL retain tool-call identities, arguments/results, terminal outcomes, and linked transcripts with their actual model identity. Restarting SHALL mark unfinished executions interrupted or outcome-unknown and SHALL NOT repeat side effects automatically.

#### Scenario: Restart after an interrupted tool call
- **WHEN** the app reopens a session whose command or MCP call had no durable final result
- **THEN** the call remains visible as interrupted or outcome-unknown and is not re-executed automatically

#### Scenario: Reopen a transcribed draft
- **WHEN** the user reopens a session with a saved transcript or composer draft
- **THEN** the persisted text and its transcription-model reference are restored without re-recording or retranscribing audio
