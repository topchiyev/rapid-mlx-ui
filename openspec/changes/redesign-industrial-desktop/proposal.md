# Proposal

## Why

The app should feel like a distinctive local computing instrument, inspired by teenage engineering industrial design, instead of another generic chat window. Capture this direction now for later implementation: the current dark teal interface, repeated explanatory labels, exposed configuration, basic statistics, and dispersed model controls need a coordinated redesign.

## What Changes

- Establish a visual language based on the [teenage engineering products](https://teenage.engineering/products) and the user's [local design references](../../../design-references/): subtly grainy grays at different lightness levels, deep shadows, white and black elements, restrained orange accents, deliberate rounded and square corners, and precise spacing.
- Recompose the desktop as an intentional instrument with useful status displays and tactile buttons, toggles, and appropriate knob controls; make the layout and interactions distinctive rather than merely recoloring chat.
- Remove redundant headings, slogans, implementation labels, and repeated helper text. Keep necessary action names, accessible names, error guidance, model identity, metric units, and state explanations.
- Consolidate infrequent runtime, paths, credentials, generation, OS-tool, MCP, and voice configuration into organized Settings, with explicit application/session scope. Keep model/effort selection and live operational actions close to the work.
- Replace the basic system-load presentation with a coherent instrument-style widget for CPU, memory, and runtime activity, with a route to detailed metrics and honest stale/unavailable states.
- Rework model browsing, selection, pull/load/unload, and removal into a clear workflow for chat and independently selected transcription models, preserving exact identity and existing operation safeguards.
- Record concrete implementation and validation tasks, all pending. This request creates planning artifacts only; implementation is deferred.

## Capabilities

### New Capabilities

- `desktop-presentation`: The shared visual language, workspace hierarchy, Settings disclosure, telemetry widget, and presentation of model operations, with keyboard and readable-state acceptance criteria.

### Modified Capabilities

None in the current main-spec inventory. The existing `desktop-shell`, `observability`, `model-management`, and `local-model-catalog` contracts live in the active [desktop change](../plan-rapid-mlx-desktop/), rather than main specs. This change adds presentation requirements alongside them without duplicating or replacing their functional contracts. Reconcile against their latest versions before implementation; the existing STT allowlist work and other unfinished gates remain independent.

## Impact

- Primarily the existing Rust egui/eframe desktop [frontend](../../../crates/desktop/src/frontend.rs), [view state](../../../crates/desktop/src/view.rs), [demo fixtures](../../../crates/desktop/src/demo.rs), and frontend/integration tests. Shared style and widget modules may be extracted during implementation.
- Reuse existing backend commands, runtime adapters, persistence, and telemetry. No UI-framework migration, additional implementation language, new runtime capability, model download, or storage migration is proposed.
- The referenced [runtime catalog fixture](../../../fixtures/runtime/available.json) supplies synthetic protocol data; it is context for model-state validation, not a design asset or evidence that every listed audio family is supported. Preserve the verified STT policy.
- Future implementation updates user-facing documentation and validation evidence. Existing session history, actual model/settings attribution, usage accounting, explicit network actions, runtime ownership, and local storage contracts continue to apply.
