# Tasks

Deferred work: do not implement until the user asks to apply this change. All implementation tasks remain unchecked. Acceptance criteria are in [the presentation spec](./specs/desktop-presentation/spec.md); decisions and preserved functional contracts are in [design.md](./design.md).

## 1. Visual composition and shared controls

- [ ] 1.1 Reconcile this presentation plan against the latest active/main desktop, model, audio, and observability contracts, especially the pending STT allowlist. Record any necessary planning adjustments and verify no presentation requirement weakens runtime ownership, explicit model operations, session identity, or metric definitions.
- [ ] 1.2 Compose representative workspace, Settings, model-management, and statistics layouts from the [supplied references](../../../design-references/) and [product page](https://teenage.engineering/products), including review of the AVIF images. Record final gray/orange palette, grain, shadows, typography, spacing, corner geometry, meter arrangement, and compact-window behavior; verify the compositions express the industrial direction across all four surfaces and keep prose readable.
- [ ] 1.3 Implement reusable Rust egui style and surface/control components with cached grain, raised/inset depth, white/black contrast, restrained orange, and measured rounded/square geometry. Verify representative screens at supported scaling and ensure texture/rendering does not trigger repeated expensive work per frame.
- [ ] 1.4 Implement tactile buttons/toggles and a knob for an existing supported bounded numeric generation setting, with precise entry, keyboard increments, names, value/state, and focus. Verify meaningful keyboard/range/disabled-state interactions using frontend tests and a native walkthrough; document the control conventions alongside visual validation evidence.

## 2. Workspace and Settings hierarchy

- [ ] 2.1 Recompose session navigation, model/effort strip, conversation/composer, send/stop/dictation, and secondary panels; remove slogans, implementation labels, repeated subtitles, and redundant headings. Verify loading/empty/streaming/error/cancelled views and compact sizing retain essential actions, clear model identity, and readable content; retain a visible synthetic demo indicator.
- [ ] 2.2 Consolidate runtime, paths, credentials, generation, tool policy, MCP registration, and voice preferences into scoped Settings sections with contextual entry points. Verify session/application scope, existing save/validation/credential rules, and independent chat/ASR selection; retain transcription operations and live permissions/cancellation outside setup forms.
- [ ] 2.3 Extend frontend/integration fixtures to verify navigation through Settings during streaming and dictation preserves original session attribution, unsent drafts, selected models, settings, and historical request metadata, and does not dispatch lifecycle actions merely by opening a panel. Run the focused tests and update README/chat/tool/MCP/audio guidance for the changed navigation.

## 3. System instrument and statistics details

- [ ] 3.1 Replace fragmented load readouts with one system widget showing scoped CPU percent, memory used/total, bounded history/meters, and managed/external runtime activity. Reuse existing worker samples and expose structured snapshot fields only where needed; verify synthetic live/empty/stale/unavailable states never invent values or GPU utilization.
- [ ] 3.2 Connect the widget to detailed source/age/process and request/session usage views. Verify fixture-backed presentation preserves units, system/process separation, timing definitions, cumulative replacement, retry/branch/tool-continuation accounting, and unknown/partial totals; update [metric guidance](../../../docs/metrics.md) and record the visual state checks.

## 4. Model browsing and lifecycle presentation

- [ ] 4.1 Rework Available/Pulled inventory, task/role filters, search, concise selection rows, exact revision/served-identity details, and compatibility/size/status into a coherent browser with independent chat/transcription context. Verify offline inventory, empty/no-match states, hidden-selection preservation, duplicate revisions, and the current supported-STT policy using synthetic UI data and existing catalog fixtures.
- [ ] 4.2 Replace unconditional lifecycle button rows with contextual Pull/Load/Unload/Disconnect and secondary removal actions. Verify loading/downloading/busy/failed/cancelled states, explicit retries/cancellation, fresh deletion preview/confirmation, active-model protection, and external disconnect semantics using frontend/backend command fixtures; selection alone must dispatch no pull/load/delete.
- [ ] 4.3 Run focused model/ASR frontend and integration regressions, including the parser/category tests using [available.json](../../../fixtures/runtime/available.json), without widening supported model families. Update [model guidance](../../../docs/models.md) and [audio guidance](../../../docs/audio.md) to match the new flow and preserve explicit-action and exact-identity explanations.

## 5. Integrated validation

- [ ] 5.1 Exercise the redesigned app with synthetic fixtures at normal and compact window sizes and supported scaling. Record screenshot and native keyboard/accessibility evidence for loading, empty, ready, error, streaming, cancellation, stale/unavailable metrics, and model lifecycle states; verify visible focus, precise knob entry, text/code copying, Enter/Shift+Enter, Settings access, and responsive telemetry during streaming.
- [ ] 5.2 Run `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`, and `npx --yes @fission-ai/openspec@1.17.0 validate --all --strict --no-interactive`. Record actual results and any remaining native verification limits in [validation documentation](../../../docs/validation.md); mark tasks complete only after their stated checks succeed.

## Workflow follow-up

- Keep this change open as deferred work until implementation is requested; planning completeness does not mean implementation is complete.
- Once implemented and validated, archive using the repository's OpenSpec workflow and verify the presentation capability is synchronized without overwriting the functional desktop/model/observability contracts.
