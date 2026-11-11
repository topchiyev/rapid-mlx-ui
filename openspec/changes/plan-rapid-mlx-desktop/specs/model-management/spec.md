# Model Management

## Purpose

Let users distinguish available models from locally pulled weights, download models explicitly, and remove pulled models without damaging unrelated cache contents.

## ADDED Requirements

### Requirement: Available and pulled model views
The app SHALL list models available in the supported runtime catalog separately from models present locally. It SHALL identify source, task/modality, identity, size when known, download status, and compatibility, and support search and task filtering without implying an unpulled model can run offline.

#### Scenario: Browse available models
- **WHEN** the user opens Available models
- **THEN** the app shows the runtime's supported catalog with text-chat and transcription categories and marks which entries are already pulled
- **AND** the served-model list is not mistaken for the entire available catalog

#### Scenario: Browse pulled models offline
- **WHEN** the user opens Pulled models without internet access
- **THEN** all locally discovered usable and incomplete models remain visible with local status
- **AND** unavailable remote metadata does not hide the local inventory

### Requirement: Explicit model pulls
The app SHALL download a selected model only after an explicit Pull action, show destination and available size information, track progress or indeterminate activity, and support cancellation and retry. Starting chat or transcription SHALL NOT silently download missing model assets.

#### Scenario: Pull a catalog model
- **WHEN** the user requests Pull for an available model
- **THEN** the UI shows the target cache and transfer state and refreshes pulled status only after local validation succeeds

#### Scenario: Download fails or is cancelled
- **WHEN** a transfer fails, runs out of space, or is cancelled
- **THEN** the UI retains the failure or cancellation state, does not mark incomplete files ready, and offers explicit retry
- **AND** existing complete cache entries remain usable

### Requirement: Confirmed pulled-model deletion
The app SHALL offer deletion for an explicitly identified pulled cache model, preview its repository scope and estimated reclaimable size when known, and require confirmation. Deletion SHALL preserve blobs referenced by other retained models/revisions and SHALL NOT delete sessions or arbitrary user-selected source folders.

#### Scenario: Delete a pulled model
- **WHEN** the user confirms deletion after seeing all affected cached revisions
- **THEN** the selected cache model is removed and local inventory refreshes
- **AND** unrelated cache entries and stored conversations/transcripts remain intact

#### Scenario: Selected model shares cached files
- **WHEN** files associated with the deleted model are still referenced by a retained cache entry
- **THEN** those shared files remain and reclaimed-space reporting reflects what was actually removed

### Requirement: Active model protection
Deletion SHALL be unavailable while a model is loaded, generating, transcribing, downloading, or in another app-owned operation. The app SHALL explain how to stop/unload owned use and SHALL NOT stop an external process to enable deletion.

#### Scenario: Delete the selected transcription model
- **WHEN** the user tries to delete a loaded or active transcription model
- **THEN** the app requires the job to finish or stop and the owned model to unload before deletion
- **AND** no deletion command is dispatched while the use lock remains active

#### Scenario: Remove an unloaded model referenced in a session
- **WHEN** an inactive model referenced by saved sessions is deleted
- **THEN** those sessions retain the original identity and show the model as unavailable until it is pulled or selected again
