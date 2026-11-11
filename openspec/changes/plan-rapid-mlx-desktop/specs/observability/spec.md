# Observability

## Purpose

Show useful system load and generation statistics while making measurement scope, missing data, and session usage accounting clear.

## ADDED Requirements

### Requirement: System and process load
The app SHALL display system-wide CPU utilization and used/total memory with units and sample timestamps. When available it SHALL show app and owned-runtime process usage separately, and GPU utilization only from a verified utilization source.

#### Scenario: Runtime consumes memory
- **WHEN** the owned runtime is running and process sampling succeeds
- **THEN** its memory usage is labeled separately from total system usage and application process usage
- **AND** runtime or Metal memory is not represented as GPU utilization

#### Scenario: GPU utilization cannot be measured
- **WHEN** no supported unprivileged GPU utilization source is available
- **THEN** GPU utilization is labeled unavailable with a short explanation rather than shown as zero or inferred from memory usage

### Requirement: Bounded and nonblocking sampling
Monitoring SHALL sample at a default interval of one second, retain at most five minutes of in-memory samples per metric, avoid overlapping slow samples, and mark readings stale after three missed sampling intervals.

#### Scenario: Sampler is slow
- **WHEN** a telemetry source takes longer than one sampling interval
- **THEN** the UI and chat remain responsive, additional calls to that source do not accumulate, and stale readings are identified

### Requirement: Response token provenance
The app SHALL show available prompt, completion, and total token counts per generation request, identify runtime-reported versus estimated values, and represent missing counts as unknown. A tool-enabled turn SHALL include every model request once, and repeated cumulative updates for one request SHALL replace earlier counts.

#### Scenario: Final usage arrives after content
- **WHEN** a completed stream supplies its final usage record
- **THEN** the response statistics show those runtime-reported counts exactly once
- **AND** any prior provisional display is replaced rather than accumulated again

#### Scenario: Cancelled stream has no usage
- **WHEN** a cancelled attempt ends without reported token counts
- **THEN** its counts remain unknown rather than becoming zero or being inferred from text length

### Requirement: Explicit timing definitions
The app SHALL measure client elapsed time from request dispatch to terminal outcome and client time to first output from dispatch to the first non-empty answer or reasoning delta. It SHALL distinguish these from server-reported timing and label throughput with its calculation scope.

#### Scenario: Reasoning precedes the answer
- **WHEN** a reasoning delta arrives before answer text
- **THEN** client time to first output uses that reasoning delta and is not labeled time to first answer

#### Scenario: Compute client effective throughput
- **WHEN** runtime-reported completion tokens and a positive client elapsed duration are available
- **THEN** effective throughput equals completion tokens divided by total client elapsed seconds and is labeled end-to-end generated tokens per second
- **AND** it is not labeled server decode speed

### Requirement: Session usage across attempts
Session usage SHALL include every distinct model request across all stored attempts and branches, including retries and partial attempts with known counts. Totals SHALL sum only known runtime-reported counts, show the number of requests with unknown counts, and label incomplete totals as partial. Estimates SHALL be separate from reported totals.

#### Scenario: Retry adds another attempt
- **WHEN** a response is retried and both attempts report usage
- **THEN** session totals include both attempts exactly once regardless of which response branch is visible

#### Scenario: A branch contains unknown usage
- **WHEN** at least one attempt has no reported counts
- **THEN** session usage displays the known subtotal and missing-model-request count without implying an exact total

#### Scenario: Reasoning tokens are a subset of completion usage
- **WHEN** the runtime reports reasoning tokens within completion tokens
- **THEN** reasoning counts are shown as a breakdown and are not added to completion totals again

#### Scenario: Turn includes tool-result continuation
- **WHEN** the model makes one request before a tool call and another after receiving its result
- **THEN** the turn and session usage include both model requests exactly once
- **AND** tool execution time and transcription duration are labeled separately from model generation timing and token counts
