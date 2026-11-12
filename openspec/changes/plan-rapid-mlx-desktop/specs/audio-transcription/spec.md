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
The app SHALL list only the tested `mlx-community/Qwen3-ASR-0.6B-5bit` and `mlx-community/nemotron-3.5-asr-streaming-0.6b` repositories as supported speech-to-text choices, separately from chat and speech-synthesis models, persist the selected transcription-model identity/revision under `~/.rapid-mlx-ui`, and provide explicit pull, load, unload, and deletion controls through model management.

#### Scenario: Select an unpulled transcription model
- **WHEN** the user selects a speech-to-text model whose required files are not local
- **THEN** the app offers an explicit pull and does not transcribe or substitute another model automatically

#### Scenario: Restore the transcription model
- **WHEN** the app restarts
- **THEN** the saved transcription selection is restored independently of the selected chat model
- **AND** a missing or deleted model is shown as unavailable

### Requirement: Tested STT models only
The app SHALL restrict STT catalog choices, managed startup, external attachment, and jobs to the two tested repositories above. It SHALL match exact repository IDs case insensitively, optionally with a revision, rather than approving whole families. Unsupported saved selections SHALL remain unavailable without substitution. Existing model files and transcripts SHALL remain untouched. The Voxtral batch adapter and catalog supplement SHALL be removed.

#### Scenario: Unsupported STT model is discovered
- **WHEN** discovery or the runtime catalog contains Whisper, Voxtral, Parakeet, another untested STT model, or a different Qwen/Nemotron variant
- **THEN** it is excluded from supported STT choices and cannot load, connect, or transcribe
- **AND** local files remain available for read-only inventory and explicit deletion

#### Scenario: Restore a previously supported selection
- **WHEN** the saved STT selection is outside the allowlist
- **THEN** the original identity remains saved and is shown as unavailable
- **AND** the user must explicitly select a tested model before recording or transcription

### Requirement: Transcription model filtering
The transcription model picker SHALL combine case-insensitive name/repository/identity search with All (default), Pulled, Available, and Served filters, excluding chat and speech-synthesis models. Filtering SHALL preserve saved selection, operate independently of the Models page, and perform no runtime or network actions. No-match feedback SHALL offer Clear filters.

#### Scenario: Find a transcription model
- **WHEN** the user enters a search and chooses an availability filter
- **THEN** only speech-to-text entries matching both controls appear
- **AND** Pulled includes exact local entries, Available includes catalog entries, and Served includes externally advertised entries
- **AND** cached catalog entries are not represented as exact pulled revisions

#### Scenario: Filtering hides the selected model
- **WHEN** the selected model does not match the filters or has disappeared from inventory
- **THEN** the saved selection remains visible and unchanged
- **AND** no-match feedback and a Clear filters action let the user restore the full list

### Requirement: Local transcription execution
The app SHALL use a verified local rapid-mlx transcription interface with the selected model and supported language/options. It SHALL expose loading, transcribing, completed, failed, and cancelled states and report missing runtime/audio dependencies without installing them or downloading assets implicitly.

#### Scenario: Transcription succeeds
- **WHEN** the local service returns a transcript
- **THEN** the UI shows editable text and the actual model identity, available language/timing metadata, and job outcome

#### Scenario: Audio runtime is unavailable
- **WHEN** the configured runtime lacks transcription capability or a required local auxiliary asset
- **THEN** the app explains the missing dependency or asset and preserves the selected model and session
- **AND** does not fall back to cloud transcription or a different model

#### Scenario: Common ASR response contains multiple language labels
- **WHEN** a successful response returns language metadata as a string, an array of strings, null, or an omitted field
- **THEN** the shared transcription adapter preserves the text, duration, and segments without model-specific response parsing
- **AND** arrays are displayed as comma-separated language labels, with an empty array treated as unavailable

#### Scenario: Malformed response diagnostics
- **WHEN** an ASR response is invalid JSON or has an unsupported field type
- **THEN** the error identifies JSON syntax/location or the invalid field without echoing response text or credentials

### Requirement: Durable transcripts and explicit chat insertion
The app SHALL persist transcript text, user edits, job status, and model metadata under `~/.rapid-mlx-ui`. It SHALL support inserting selected transcript text into a session composer without sending it automatically or replacing an existing draft silently.

#### Scenario: Insert text into an existing draft
- **WHEN** the user inserts a completed transcript into a composer that already has text
- **THEN** the UI preserves the existing draft through an explicit append/replace choice and waits for a separate Send action

#### Scenario: Reopen transcript history
- **WHEN** the user restarts and reopens a saved transcript
- **THEN** its text, edits, outcome, and model reference remain available without rerunning transcription

### Requirement: Voice input in the chat composer
The chat composer SHALL provide explicit Record voice, Stop recording, and Cancel dictation controls using the selected ready ASR model. Successful dictation SHALL append once to the originating chat draft, preserve existing edits and model settings, and wait for Send. Recording SHALL be unavailable while inference is active or the ASR model is unready, with setup guidance.

#### Scenario: Dictate a message
- **WHEN** the user records voice from chat and stops recording
- **THEN** the recording is transcribed locally and its text is appended to that chat's editable draft
- **AND** the transcript and its session reference are saved without sending a message or changing either model selection

#### Scenario: Navigate or type during dictation
- **WHEN** the user edits the draft or navigates to another chat before transcription completes
- **THEN** text is appended to the latest draft in the originating session, never redirected to the newly focused chat
- **AND** repeated snapshots or insertion commands do not duplicate the transcript

#### Scenario: Cancel or fail dictation
- **WHEN** recording or transcription is cancelled or fails, including microphone denial or unready ASR
- **THEN** the draft remains unchanged and the UI reports the outcome while preserving any received transcript in history
- **AND** late successful capture or transcription events do not insert text after cancellation

#### Scenario: Restart after dictation
- **WHEN** the app restarts after a completed or interrupted dictation
- **THEN** persisted draft text and linked transcripts are restored without automatically recording, transcribing, or reinserting text

### Requirement: Recording data and cancellation
App-managed audio staging SHALL stay under `~/.rapid-mlx-ui` and be cleaned after terminal jobs by default unless the user enables retention. Imported source files SHALL remain untouched. Cancellation SHALL retain available transcript text and distinguish local cancellation from a confirmed server stop.

#### Scenario: Stop a transcription job
- **WHEN** the user cancels an active transcription
- **THEN** the app stops local waiting/capture, requests supported server cancellation, and records the known outcome
- **AND** received text is preserved and managed temporary audio is cleaned according to the retention preference
