# Metrics backend

`core::metrics` separates OS sampling, optional verified runtime status fields, monotonic client timing, and runtime-reported usage. The desktop owns background worker handles and Store persistence; the module has no database coupling and no network polling of its own.

## OS sampling

`SystemProvider::new(app_pid, owned_chat_pid, owned_asr_pid)` uses sysinfo 0.40. Supply only process IDs currently owned by the application supervisor; do not supply an external server PID or discover ownership by name. Recreate the provider when owned PIDs change. Call `sample()` on a worker. Initialization errors propagate through `Result`. sysinfo refresh APIs do not expose a native error result, so this adapter cannot report a refresh failure that sysinfo itself suppresses. Missing processes produce `None` readings; worker panics are observable by joining the sampler handle. Values are:

| Field | Scope/source | Units |
| --- | --- | --- |
| `system_cpu_percent` | System-wide, sysinfo, average across logical CPUs | Percent, 0–100; first sample unknown because CPU requires an interval |
| `system_used_bytes`, `system_total_bytes` | System-wide memory, sysinfo | Bytes |
| `processes[].cpu_percent_one_core` | Individual app/owned chat/owned ASR process, sysinfo | Percent of one logical CPU; can exceed 100%; first sample unknown |
| `processes[].resident_bytes` | Individual process resident memory, sysinfo | Bytes, unknown when process absent |
| `timestamp_unix_ms` | UTC wall clock at sample construction | Unix milliseconds |
| `gpu_utilization_percent` | No verified unprivileged provider | Unavailable, never zero or inferred from Metal memory |

Samples represent individual processes, not descendant trees. External runtimes may supply HTTP status metrics separately but are not treated as OS-owned processes. OS CPU values are interval averages, not instantaneous values.

`spawn_sampler(FnMut() -> LoadSample, Arc<Mutex<SampleHistory>>, CancellationToken)` returns a dedicated thread handle. One serial provider invocation runs at a time; slow sampling never queues missed intervals. Default interval is one second. The UI should copy snapshots while briefly holding the history lock. Cancellation interrupts interval waiting within approximately 20 ms; a running OS sample completes before the thread exits. Cancel and join the worker on shutdown. A custom provider must itself terminate in bounded time; the worker cannot interrupt arbitrary blocking providers.

`SampleHistory::push(sample, received_instant)` retains at most 300 samples and drops wall-clock samples older than five minutes relative to the newest sample. `is_stale(now)` uses monotonic receipt time: unknown/empty histories are stale, and a reading becomes stale at three seconds without a sample. Thus UI age is unaffected by ordinary wall-clock adjustments. Caller-driven sampling intervals do not alter the default three-second threshold.

## Optional server status

Read-only inspection of rapid-mlx 1.0.2 `routes/health.py` verifies `/v1/status` fields `generation_tps`, `prompt_tps`, and `metal.active_memory_gb`, `metal.peak_memory_gb`, `metal.cache_memory_gb`. Scheduler source divides memory bytes by **1e9**, so GB here means decimal gigabytes, not GiB. `RuntimeMetrics::from_status` parses only finite nonnegative numeric values and preserves missing/invalid fields as unknown. Parsing is not a capability probe; call only for a negotiated status profile. These rates are server scheduler aggregate statistics, not request timings. The server may report an idle zero; label it as server-reported rather than measuring client generation from it. Metal active/peak/cache memory is never GPU utilization. No per-request server-duration fields have been verified or exposed by this adapter.

## Usage and timing

`ReportedUsage` contains optional prompt, completion, total, and reasoning token counts from a runtime response. No text-length estimator exists. Reasoning is a subset of completion and is never added to total again. Missing values remain unknown, including cancelled or failed generations. Do not fill missing totals by assuming zero.

`ModelRequestUsage` identifies the session, turn, attempt, actual model, and unique model-request step. Generate one request identity per actual inference dispatch. `UsageLedger::replace` replaces the entire cumulative row for that identity; it never adds repeated cumulative updates. `session_totals` and `turn_totals` include every matching distinct step, regardless of branch visibility, terminal state, retries, or tool continuations. Known fields are summed independently with saturating arithmetic; `requests_with_unknown_counts` counts requests missing any prompt/completion/total field and `partial` makes that incompleteness explicit. Reasoning breakdown absence alone does not make total counts partial. The module accepts reported values only; estimates must be stored/displayed separately.

Persist the per-request records using the main Store, and rebuild/deserialize the ledger on restart. Do not feed an attempt aggregate back in as another request row. Replacing a row requires a complete cumulative snapshot; if a transport supplies sparse updates, merge fields upstream before replacement. Session deletion should remove its rows through the Store transaction. The serde ledger supports round trips, but does not independently enforce database/session identity constraints.

`RequestClock::new(dispatch)` starts at dispatch. `output(nonempty_text, now)` records the first nonempty answer **or reasoning** delta. `finish(terminal)` measures through completed, cancelled, failed, or interrupted terminal outcome. `ClientTiming` fields use milliseconds. Client time to first output is not time to first answer. `end_to_end_generated_tokens_per_second` divides reported completion tokens by positive total elapsed seconds; missing tokens or zero/missing duration produces unavailable. Display it as **end-to-end generated tokens/s**, never server decode speed. Per-request elapsed time includes waiting/network overhead; tool execution and transcription duration are separate metrics, not generation tokens.

Automated tests cover retention/staleness using injected clocks, serial worker shutdown, cumulative replacement and independent request steps, retries/unknown counts, reasoning subsets, serde restart, and malformed optional server fields. Live GPU utilization remains unavailable. The later live text test and its client timing/usage results are recorded in [implementation validation](./validation.md).
