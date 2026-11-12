# Design

## Context

See [proposal.md](./proposal.md) for motivation and [the presentation specification](./specs/desktop-presentation/spec.md) for acceptance criteria. This is deferred planning, not authorization to implement now.

The existing app uses Rust egui/eframe. The [frontend](../../../crates/desktop/src/frontend.rs) applies dark blue-gray surfaces and a teal accent, with separate Chat, Models, Transcription, Statistics, OS tools, MCP servers, and Settings pages. Its sidebar contains slogans/implementation labels; the inspector mixes runtime actions and basic load readouts; transcription model selection exposes a long row of lifecycle actions. The [view types](../../../crates/desktop/src/view.rs) and existing command/snapshot boundary already separate presentation from backend work. The [demo](../../../crates/desktop/src/demo.rs) provides synthetic data without real inference or application-state writes.

The active [desktop change](../plan-rapid-mlx-desktop/) still owns functional contracts and unfinished validation. Its pending STT restriction and model-redesign deferral must be respected. Only development-launch is currently a main spec, so this change introduces a presentation capability without prematurely copying the active functional specs into main.

## Goals / Non-Goals

**Goals:** A reusable visual system and an intentional instrument layout; progressive disclosure of configuration; concise, comprehensible operational state; model and telemetry surfaces built on existing backend behavior.

**Non-Goals:** Runtime/API redesign, new model support, new metrics providers, history/accounting changes, storage migration, UI-framework migration, a new implementation language, or implementation during this planning request.

## Decisions

### 1. Translate the supplied industrial references into an app-specific instrument

Use the [products page](https://teenage.engineering/products) and preserve links to all six [reference images](../../../design-references/). Planning visually reviewed [1.webp](../../../design-references/1.webp), [2.webp](../../../design-references/2.webp), [3.webp](../../../design-references/3.webp), and [4.webp](../../../design-references/4.webp): gray physical panels, sparse orange controls, black inset displays, purposeful grids, and pronounced shadows. [5.avif](../../../design-references/5.avif) and [6.avif](../../../design-references/6.avif) are supplied references retained for implementation review; the current image viewer could not decode them.

Define shared surface, text, accent, border, spacing, radius, shadow, focus, and typography tokens. Use multiple lightness levels of neutral gray, subtle stable grain, white/black contrast, and scarce orange highlights. Raised controls and inset status displays get consistent directional depth. Combine small measured radii with square panel/display edges; avoid turning every region into a rounded card. Keep conversation text on a quiet readable surface. Use compact technical typography for meters and values, with comfortable prose typography for reading.

Extract reusable Rust style/widget helpers within the desktop crate as needed; retain egui/eframe and the existing backend boundary. Render textures once or cache them, and draw simple meters/shapes locally. Product photos remain references rather than interface backgrounds. A color-only reskin would miss the requested spatial and interaction changes; reproducing hardware at literal scale would compromise desktop readability.

### 2. Organize the workspace around work and operational state

Retain session navigation and a generous conversation/composer area, with a compact model/effort strip, tactile send/stop/dictation controls, and a single system instrument module. Model management and Settings remain easy to open. At compact sizes, secondary surfaces collapse into accessible panels rather than squeezing text between permanent inspectors.

Remove decorative copy such as "LOCAL INTELLIGENCE" and "Rust desktop · Local by default", plus repeated page subtitles/section labels where hierarchy already supplies meaning. Keep a clear synthetic-fixture indicator in demo mode. Keep concise names for actions and state; use tooltips/details for explanations that are useful occasionally. Never make essential status or errors hover-only. This avoids both the current crowded labels and an unexplained icon-only interface.

### 3. Consolidate configuration with explicit scope

Use Settings sections for Runtime, Model locations, Generation, Tools, MCP, and Voice. Global sections edit application preferences; Generation and session tool policies identify the session they affect. Registration is global while enabled tools remain session-scoped. Voice model selection remains independent of chat and can deep-link into role-specific model management. Preserve each setting's current save/validation and secret-handling behavior; no settings migration is needed.

Keep model selection, supported effort, lifecycle actions, permission requests, tool-call outcomes, and recording/generation cancellation in their work context. Put credentials, endpoints, paths, sampling/system-prompt fields, policies, registration forms, retention, and voice defaults behind Settings. Keep operational transcription import/history/editing available independently of configuration. Opening Settings never changes processes or active request parameters. This preserves useful live controls while removing setup forms from daily work.

### 4. Make tactile controls functional and accessible

Build raised/pressed buttons and toggles with distinct selected/disabled/focused states. Use a knob with precise numeric entry for a suitable existing bounded generation parameter, such as temperature, subject to the existing runtime capability validation. Support keyboard increments and direct value entry. Do not invent an "effort dial" beyond the runtime's supported discrete options. Read-only CPU/memory graphics use meters, not adjustable knobs. Prefer familiar pointer and keyboard interaction over hardware-like gestures that require learning.

### 5. Create one honest system instrument

Compose CPU percent, memory used/total, bounded recent history, and runtime activity into an inset display module. Keep units and system scope visible; expose source, age, app/owned-process breakdowns, and usage/timing definitions in details. Runtime state is categorical and clearly identifies managed/external ownership. Use actual samples for graphics, never decorative motion that resembles measurement.

Reuse [the existing metrics contract](../../../docs/metrics.md): one-second default sampling, at most 300 samples/five minutes, stale after three seconds. GPU utilization remains unavailable without a verified source; Metal memory stays memory. UI state may need structured metric fields where existing formatted strings cannot support accurate presentation; derive them from existing providers without adding UI-thread sampling.

History and accounting remain unchanged: request counts are runtime-reported; cumulative updates replace prior values; totals include each distinct request across retries, branches, partial attempts, and tool continuations once; missing counts remain unknown and totals partial. Client elapsed time runs from dispatch through terminal outcome, first output includes reasoning, and effective generated tokens/s uses completion tokens divided by total elapsed seconds. No metric relabeling may change these meanings.

### 6. Present model operations as an explicit lifecycle

Use searchable Available/Pulled inventory with task/role filters, concise rows, a details area, and contextual actions. Show short names in compact controls with full exact identity/revision in accessible details and selection context. Local cache revision, catalog identity, and advertised served identity remain distinct; selection does not start any operation. Show compatibility, size if known, status, progress, and actionable errors without making the row a wall of buttons.

Expose Pull for missing weights, Load for usable local choices, Unload for idle app-owned runtimes, Disconnect for external services, and preview-confirmed removal in a secondary action area. Reflect queued/loading/downloading/ready/busy/failed/cancelled states from the backend and preserve explicit cancellation/retry. Respect active-model protection and deletion scope. Chat and ASR use the same presentation vocabulary with independent selections. Search never clears a hidden selection or substitutes another model.

The [available catalog fixture](../../../fixtures/runtime/available.json) is a protocol example containing chat, STT, TTS, and aligner entries; it does not grant support to these families. Use it for existing parser/category regressions and separate UI state fixtures as needed. The new presentation must consume whatever tested STT policy is current when implementation begins.

Verified integration boundary: existing Rust backend commands already provide refresh, pull/cancel, deletion preview/confirmation, runtime start/connect/stop, generation, and transcription. This proposal adds no rapid-mlx endpoint, CLI option, stream format, or reasoning parameter. No new runtime-capability investigation is required; exact runtime behavior remains governed by [compatibility documentation](../../../docs/compatibility.md) and the active functional specs.

## Risks / Trade-offs

- Texture and deep shadows reduce contrast or increase repaint cost → keep text surfaces quiet, cache grain, bound graphics, and verify live streaming responsiveness and scaling.
- Knobs or reduced copy obscure meaning → use clear accessible names, concise state text, keyboard/value entry, and discoverable details; retain error guidance and destructive-action confirmation.
- Consolidating configuration blurs session/global scope → label scope explicitly and test switching sessions with drafts, selected models, and settings intact.
- Visually polished screenshots hide missing states → validate empty/loading/error/streaming/cancelled/stale/unavailable screens and model lifecycle transitions using fixtures.
- Functional work evolves before this deferred redesign starts → reconcile the active/main functional contracts, especially STT eligibility, before touching presentation code.

## Migration Plan

When implementation is requested, reconcile functional specs and finalize representative layouts/token values using the supplied references. Implement shared components, then workspace/Settings, then telemetry and model flows. Preserve command contracts, stored data, history identities, and settings scopes throughout. Update the README and affected user guides, record visual/native keyboard checks, and run configured Rust and OpenSpec validation before marking tasks complete. Archive only after implementation and validation are complete.

No data migration is planned. Presentation changes can be reverted without rewriting sessions, preferences, model caches, or runtime state.

## Open Questions

- Exact gray/orange values, grain strength, typography sizes, corner radii, and shadow parameters will be selected together during the future visual composition task.
- Choose the final meter arrangement and compact-window panel breakpoint through visual and keyboard validation within the specified layout and metric contracts.
