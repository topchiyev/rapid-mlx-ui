# Runtime Management

## Purpose

Let users configure and supervise a local rapid-mlx runtime while preserving externally managed processes and existing model files.

## ADDED Requirements

### Requirement: Runtime discovery and compatibility
The app SHALL detect rapid-mlx from a configured executable or the environment, show its version, and explain missing or incompatible capabilities before enabling dependent actions.

#### Scenario: Executable is missing
- **WHEN** no usable rapid-mlx executable can be found
- **THEN** managed startup is unavailable and the UI provides configuration guidance
- **AND** existing sessions and external connection settings remain accessible

#### Scenario: Download prevention is unsupported
- **WHEN** the installed runtime cannot enforce local-only model loading
- **THEN** managed startup is blocked with an explanation rather than risking an implicit download

### Requirement: Managed runtime lifecycle
The app SHALL manage at most one owned chat service and one owned transcription service, each using an explicitly selected local model, loopback address, and distinct configured port. Each service SHALL distinguish stopped, starting, ready, stopping, and failed states and require bounded readiness checks before use.

#### Scenario: Start a local model
- **WHEN** the user starts a compatible local model with a free configured port
- **THEN** the app shows startup progress and marks the runtime ready only after service readiness and served-model identity are verified

#### Scenario: Port is occupied
- **WHEN** managed startup encounters an occupied configured port
- **THEN** startup fails visibly without killing the occupying process or silently connecting to it

#### Scenario: Startup times out or exits
- **WHEN** the owned process exits or fails to become ready before the configured startup deadline
- **THEN** the app shows a failure with recent diagnostic output and offers retry
- **AND** cleans up the owned startup process if it is still running

#### Scenario: Start a transcription service
- **WHEN** the user activates a pulled transcription model
- **THEN** the transcription service verifies its model and transcription readiness on its configured port without changing the selected chat model

### Requirement: Runtime ownership
The app SHALL distinguish owned and external runtimes. Normal shutdown SHALL stop and reap owned processes and SHALL disconnect from external processes without stopping them.

#### Scenario: Attach to an existing local server
- **WHEN** the user explicitly connects to a compatible loopback rapid-mlx server
- **THEN** the UI identifies it as external and exposes its verified served models
- **AND** owned-process stop controls are unavailable

#### Scenario: Close the app
- **WHEN** the user closes the application with an owned runtime active
- **THEN** active requests are finalized as interrupted when needed and the owned runtime is shut down within a bounded deadline
- **AND** an externally managed server is never terminated

### Requirement: Explicit model activation
The app SHALL activate a selected local model only through an explicit user action and SHALL verify the served identity before sending requests. It SHALL serialize model switching with active generation or transcription and block deletion while a model is loaded, downloading, or in use.

#### Scenario: Switch an owned runtime to a different model
- **WHEN** the user chooses a different model while generation is active
- **THEN** the UI requires the generation to finish or be stopped before replacing the owned runtime
- **AND** switching preserves existing messages and their original model metadata

#### Scenario: Requested model is absent on an external server
- **WHEN** the selected model is not served by the attached external runtime
- **THEN** the app explains the mismatch and blocks sending for that selection
- **AND** does not restart the external server or substitute another model

### Requirement: Local operation and diagnostic output
The app SHALL prevent implicit downloads and unsolicited telemetry or update checks in runtimes it launches. It SHALL expose bounded diagnostic logs and redact credentials and user text from application-generated logs.

#### Scenario: Offline model load fails
- **WHEN** a selected model is missing required local files
- **THEN** managed startup reports the missing-file failure without fetching files from the network

#### Scenario: Runtime produces excessive logs
- **WHEN** the runtime produces more diagnostic lines than the configured retention limit
- **THEN** older lines are discarded and the UI remains responsive
- **AND** no unbounded log file is created by the app

#### Scenario: Inspect third-party runtime output
- **WHEN** the user opens runtime diagnostics
- **THEN** the UI identifies the content as runtime output and does not upload it automatically

### Requirement: Serialized inference across roles
The initial release SHALL run only one chat generation or transcription inference job at a time, identify the active role, and require it to finish or stop before beginning inference in another role. Waiting users SHALL retain their drafts and captured audio.

#### Scenario: Transcribe while chat is generating
- **WHEN** the user tries to begin transcription while a chat generation is active
- **THEN** the UI identifies the active chat and requires it to finish or stop first
- **AND** the selected audio and existing conversation remain available
