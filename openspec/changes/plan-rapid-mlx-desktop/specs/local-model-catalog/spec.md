# Local Model Catalog

## Purpose

Make already downloaded Hugging Face models and explicitly configured local model folders discoverable without changing their contents.

## ADDED Requirements

### Requirement: Read-only local discovery
The app SHALL discover models in the configured Hugging Face cache and additional user-selected directories. Discovery SHALL honor configured cache overrides, require no network access, and avoid modifying model files or cache metadata.

#### Scenario: Discover cached models offline
- **WHEN** the user refreshes discovery with a populated cache and no internet connection
- **THEN** the catalog lists locally inspectable models without requesting remote metadata or downloads

#### Scenario: Scan a custom location
- **WHEN** the user adds a readable directory containing a local model
- **THEN** the catalog discovers that model and retains the configured location across restarts

### Requirement: Stable snapshot identity
The catalog SHALL identify cached models by repository and revision when available and retain their resolved local load paths. It SHALL distinguish revisions and deduplicate repeated discovery of the same model location.

#### Scenario: Two cached revisions
- **WHEN** two revisions of one repository are present
- **THEN** the user can distinguish and select either revision without the app silently choosing the other

#### Scenario: Overlapping scan roots
- **WHEN** a model is reachable through two configured roots
- **THEN** the same resolved model location appears once rather than as duplicate selectable entries

### Requirement: Model information and compatibility
The catalog SHALL show model identity, local location, revision when known, logical file size, and available format and quantization metadata. It SHALL distinguish load candidates, incomplete models, unsupported models, and models with unknown compatibility.

#### Scenario: Unsupported format
- **WHEN** discovery finds weights in a format the supported runtime cannot load
- **THEN** the catalog explains the incompatibility and disables activation
- **AND** leaves the files untouched

#### Scenario: Incomplete or unreadable model
- **WHEN** required indexed shards, configuration, or referenced files are missing or unreadable
- **THEN** the model is marked incomplete with a useful explanation and other models remain visible

#### Scenario: Compatibility cannot be established locally
- **WHEN** local metadata is insufficient to establish runtime compatibility
- **THEN** the UI labels compatibility as unknown rather than claiming the model is supported
- **AND** an explicit load attempt can report the runtime's actual result

#### Scenario: MLX Whisper NPZ checkpoint
- **WHEN** a recognized ASR checkpoint contains local NPZ weights supported by the verified audio loader
- **THEN** discovery records the NPZ format and logical weight size for read-only inventory while the tested STT allowlist blocks activation
- **AND** missing audio processors or dependencies remain runtime validation errors, while NPZ-only chat or unknown-family checkpoints remain unsupported

#### Scenario: Explain a blocked load
- **WHEN** the user requests activation of an incomplete or unsupported model
- **THEN** the UI error includes the catalog's specific reason rather than only a generic unsupported-model message

### Requirement: Refresh and selection integrity
The app SHALL refresh discovery without clearing a still-valid selection. Before activation it SHALL revalidate that the selected local path is still usable, and it SHALL never replace a missing selection with another model automatically.

#### Scenario: Model is removed outside the app
- **WHEN** the selected model disappears before activation
- **THEN** activation is blocked with a missing-model explanation and the user can refresh or choose another model

#### Scenario: Cache directory is unavailable
- **WHEN** a configured cache location cannot be read
- **THEN** the catalog reports the affected location while retaining results from other readable locations
