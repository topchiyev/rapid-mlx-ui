# Audio Transcription

## Purpose

Transcribe user-provided audio locally, manage the chosen speech-to-text model, and preserve editable text and transcription metadata for desktop sessions.

## ADDED Requirements

### Requirement: File and microphone input
The app SHALL accept supported audio files and user-initiated microphone recordings for transcription, show recording state, request OS microphone permission when needed, and provide stop/cancel controls. It SHALL explain unsupported formats, denied permissions, empty recordings, and size/duration limits.

#### Scenario: Transcribe a selected file
- **WHEN** the user chooses a supported audio file and starts transcription with a ready compatible local model
- **THEN** a visible job begins without modifying the source file

#### Scenario: Microphone permission is denied
- **WHEN** the OS denies microphone access
- **THEN** recording does not begin and the UI explains how to resolve the permission or select an audio file instead

### Requirement: Managed transcription model selection
The app SHALL list available and pulled speech-to-text models separately from chat and speech-synthesis models, persist the selected transcription-model identity/revision under `~/.rapid-mlx-ui`, and provide explicit pull, load, unload, and deletion controls through model management.

#### Scenario: Select an unpulled transcription model
- **WHEN** the user selects a speech-to-text model whose required files are not local
- **THEN** the app offers an explicit pull and does not transcribe or substitute another model automatically

#### Scenario: Restore the transcription model
- **WHEN** the app restarts
- **THEN** the saved transcription selection is restored independently of the selected chat model
- **AND** a missing or deleted model is shown as unavailable

### Requirement: Local transcription execution
The app SHALL use a verified local rapid-mlx transcription interface with the selected model and supported language/options. It SHALL expose loading, transcribing, completed, failed, and cancelled states and report missing runtime/audio dependencies without installing them or downloading assets implicitly.

#### Scenario: Transcription succeeds
- **WHEN** the local service returns a transcript
- **THEN** the UI shows editable text and the actual model identity, available language/timing metadata, and job outcome

#### Scenario: Audio runtime is unavailable
- **WHEN** the configured runtime lacks transcription capability or a required local auxiliary asset
- **THEN** the app explains the missing dependency or asset and preserves the selected model and session
- **AND** does not fall back to cloud transcription or a different model

### Requirement: Durable transcripts and explicit chat insertion
The app SHALL persist transcript text, user edits, job status, and model metadata under `~/.rapid-mlx-ui`. It SHALL support inserting selected transcript text into a session composer without sending it automatically or replacing an existing draft silently.

#### Scenario: Insert text into an existing draft
- **WHEN** the user inserts a completed transcript into a composer that already has text
- **THEN** the UI preserves the existing draft through an explicit append/replace choice and waits for a separate Send action

#### Scenario: Reopen transcript history
- **WHEN** the user restarts and reopens a saved transcript
- **THEN** its text, edits, outcome, and model reference remain available without rerunning transcription

### Requirement: Recording data and cancellation
App-managed audio staging SHALL stay under `~/.rapid-mlx-ui` and be cleaned after terminal jobs by default unless the user enables retention. Imported source files SHALL remain untouched. Cancellation SHALL retain available transcript text and distinguish local cancellation from a confirmed server stop.

#### Scenario: Stop a transcription job
- **WHEN** the user cancels an active transcription
- **THEN** the app stops local waiting/capture, requests supported server cancellation, and records the known outcome
- **AND** received text is preserved and managed temporary audio is cleaned according to the retention preference
