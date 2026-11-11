//! Bounded worker telemetry and cumulative per-model-request usage accounting.
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, VecDeque},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use sysinfo::{Pid, ProcessesToUpdate, System};
use tokio_util::sync::CancellationToken;

pub const SAMPLE_INTERVAL: Duration = Duration::from_secs(1);
pub const HISTORY_LIMIT: usize = 300;
pub const STALE_AFTER: Duration = Duration::from_secs(3);

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ProcessSample {
    pub role: String,
    pub pid: u32,
    /// sysinfo process CPU percent: 100% means one logical CPU, may exceed 100%.
    pub cpu_percent_one_core: Option<f32>,
    pub resident_bytes: Option<u64>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct LoadSample {
    pub timestamp_unix_ms: i64,
    pub system_cpu_percent: Option<f32>,
    pub system_used_bytes: u64,
    pub system_total_bytes: u64,
    pub processes: Vec<ProcessSample>,
    pub gpu_utilization_percent: Option<f32>,
    pub gpu_unavailable_reason: String,
}

/// Supply only app PID and supervisor-owned PIDs; external runtimes are omitted.
pub struct SystemProvider {
    system: System,
    processes: Vec<(String, u32)>,
    primed: bool,
}
impl SystemProvider {
    pub fn new(app_pid: u32, owned_chat: Option<u32>, owned_asr: Option<u32>) -> Result<Self> {
        let mut processes = vec![("app".into(), app_pid)];
        for (role, pid) in [("owned_chat", owned_chat), ("owned_asr", owned_asr)] {
            if let Some(pid) = pid {
                processes.push((role.into(), pid));
            }
        }
        Ok(Self {
            system: System::new()?,
            processes,
            primed: false,
        })
    }
    /// Run on the sampling worker, never on the rendering thread.
    pub fn sample(&mut self) -> LoadSample {
        self.system.refresh_cpu_usage();
        self.system.refresh_memory();
        let pids: Vec<_> = self
            .processes
            .iter()
            .map(|(_, pid)| Pid::from_u32(*pid))
            .collect();
        self.system
            .refresh_processes(ProcessesToUpdate::Some(&pids), true);
        let sample = LoadSample {
            timestamp_unix_ms: chrono::Utc::now().timestamp_millis(),
            system_cpu_percent: self.primed.then(|| self.system.global_cpu_usage()),
            system_used_bytes: self.system.used_memory(),
            system_total_bytes: self.system.total_memory(),
            processes: self
                .processes
                .iter()
                .map(|(role, pid)| {
                    let process = self.system.process(Pid::from_u32(*pid));
                    ProcessSample {
                        role: role.clone(),
                        pid: *pid,
                        cpu_percent_one_core: if self.primed {
                            process.map(|p| p.cpu_usage())
                        } else {
                            None
                        },
                        resident_bytes: process.map(|p| p.memory()),
                    }
                })
                .collect(),
            gpu_utilization_percent: None,
            gpu_unavailable_reason: "No verified unprivileged GPU utilization provider".into(),
        };
        self.primed = true;
        sample
    }
}

#[derive(Default)]
pub struct SampleHistory {
    samples: VecDeque<LoadSample>,
    last_received: Option<Instant>,
}
impl SampleHistory {
    pub fn push(&mut self, sample: LoadSample, received: Instant) {
        self.samples.push_back(sample);
        self.last_received = Some(received);
        while self.samples.len() > HISTORY_LIMIT {
            self.samples.pop_front();
        }
        // Retain at most five minutes even when a caller supplies a faster clock.
        let cutoff = self
            .samples
            .back()
            .map(|s| s.timestamp_unix_ms.saturating_sub(300_000));
        while self
            .samples
            .front()
            .is_some_and(|s| Some(s.timestamp_unix_ms) < cutoff)
        {
            self.samples.pop_front();
        }
    }
    pub fn samples(&self) -> &VecDeque<LoadSample> {
        &self.samples
    }
    pub fn is_stale(&self, now: Instant) -> bool {
        self.last_received
            .is_none_or(|last| now.saturating_duration_since(last) >= STALE_AFTER)
    }
}

/// One dedicated worker calls the provider serially; slow samples never queue up.
/// Cancel and join on shutdown. A currently executing OS sample finishes first.
pub fn spawn_sampler<F>(
    mut provider: F,
    history: Arc<Mutex<SampleHistory>>,
    cancel: CancellationToken,
) -> std::thread::JoinHandle<()>
where
    F: FnMut() -> LoadSample + Send + 'static,
{
    std::thread::spawn(move || {
        while !cancel.is_cancelled() {
            let start = Instant::now();
            let sample = provider();
            if cancel.is_cancelled() {
                break;
            }
            if let Ok(mut history) = history.lock() {
                history.push(sample, Instant::now());
            }
            while !cancel.is_cancelled() && start.elapsed() < SAMPLE_INTERVAL {
                std::thread::sleep(
                    Duration::from_millis(20).min(SAMPLE_INTERVAL.saturating_sub(start.elapsed())),
                );
            }
        }
    })
}

/// Verified /v1/status fields from rapid-mlx 1.0.2 routes/health.py.
/// Server aggregate rates are not per-request timing or client throughput.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct RuntimeMetrics {
    pub metal_active_memory_gb: Option<f64>,
    pub metal_peak_memory_gb: Option<f64>,
    pub metal_cache_memory_gb: Option<f64>,
    pub server_generation_tokens_per_second: Option<f64>,
    pub server_prompt_tokens_per_second: Option<f64>,
}
impl RuntimeMetrics {
    pub fn from_status(value: &serde_json::Value) -> Self {
        fn number(v: &serde_json::Value) -> Option<f64> {
            v.as_f64().filter(|x| x.is_finite() && *x >= 0.0)
        }
        Self {
            metal_active_memory_gb: number(&value["metal"]["active_memory_gb"]),
            metal_peak_memory_gb: number(&value["metal"]["peak_memory_gb"]),
            metal_cache_memory_gb: number(&value["metal"]["cache_memory_gb"]),
            server_generation_tokens_per_second: number(&value["generation_tps"]),
            server_prompt_tokens_per_second: number(&value["prompt_tps"]),
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReportedUsage {
    pub prompt_tokens: Option<u64>,
    pub completion_tokens: Option<u64>,
    pub total_tokens: Option<u64>,
    /// Subset of completion_tokens; never added to totals.
    pub reasoning_tokens: Option<u64>,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct ClientTiming {
    pub elapsed_ms: Option<u64>,
    pub time_to_first_output_ms: Option<u64>,
}
impl ClientTiming {
    pub fn end_to_end_generated_tokens_per_second(&self, usage: &ReportedUsage) -> Option<f64> {
        let ms = self.elapsed_ms.filter(|ms| *ms > 0)?;
        Some(usage.completion_tokens? as f64 / (ms as f64 / 1000.0))
    }
}
/// Monotonic timing: first nonempty answer OR reasoning delta, ending at any terminal outcome.
pub struct RequestClock {
    dispatch: Instant,
    first_output: Option<Instant>,
}
impl RequestClock {
    pub fn new(dispatch: Instant) -> Self {
        Self {
            dispatch,
            first_output: None,
        }
    }
    pub fn output(&mut self, text: &str, now: Instant) {
        if !text.is_empty() && self.first_output.is_none() {
            self.first_output = Some(now);
        }
    }
    pub fn finish(&self, terminal: Instant) -> ClientTiming {
        fn ms(d: Duration) -> u64 {
            d.as_millis().min(u64::MAX as u128) as u64
        }
        ClientTiming {
            elapsed_ms: Some(ms(terminal.saturating_duration_since(self.dispatch))),
            time_to_first_output_ms: self
                .first_output
                .map(|t| ms(t.saturating_duration_since(self.dispatch))),
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ModelRequestUsage {
    pub request_id: String,
    pub session_id: String,
    pub turn_id: String,
    pub attempt_id: String,
    pub model: String,
    pub usage: ReportedUsage,
    pub timing: ClientTiming,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct UsageLedger {
    pub requests: BTreeMap<String, ModelRequestUsage>,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct UsageTotals {
    pub model_requests: usize,
    pub prompt_tokens_known: u64,
    pub completion_tokens_known: u64,
    pub total_tokens_known: u64,
    pub reasoning_tokens_known: u64,
    pub requests_with_unknown_counts: usize,
    pub partial: bool,
}
impl UsageLedger {
    /// Replace cumulative values, including a late final update; caller persists this same identity.
    pub fn replace(&mut self, request: ModelRequestUsage) {
        self.requests.insert(request.request_id.clone(), request);
    }
    pub fn session_totals(&self, session: &str) -> UsageTotals {
        self.totals(|r| r.session_id == session)
    }
    pub fn turn_totals(&self, turn: &str) -> UsageTotals {
        self.totals(|r| r.turn_id == turn)
    }
    fn totals(&self, include: impl Fn(&ModelRequestUsage) -> bool) -> UsageTotals {
        let mut totals = UsageTotals::default();
        for row in self.requests.values().filter(|r| include(r)) {
            totals.model_requests += 1;
            let usage = &row.usage;
            if usage.prompt_tokens.is_none()
                || usage.completion_tokens.is_none()
                || usage.total_tokens.is_none()
            {
                totals.requests_with_unknown_counts += 1;
            }
            totals.prompt_tokens_known = totals
                .prompt_tokens_known
                .saturating_add(usage.prompt_tokens.unwrap_or(0));
            totals.completion_tokens_known = totals
                .completion_tokens_known
                .saturating_add(usage.completion_tokens.unwrap_or(0));
            totals.total_tokens_known = totals
                .total_tokens_known
                .saturating_add(usage.total_tokens.unwrap_or(0));
            totals.reasoning_tokens_known = totals
                .reasoning_tokens_known
                .saturating_add(usage.reasoning_tokens.unwrap_or(0));
        }
        totals.partial = totals.requests_with_unknown_counts > 0;
        totals
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn sample(t: i64) -> LoadSample {
        LoadSample {
            timestamp_unix_ms: t,
            system_cpu_percent: None,
            system_used_bytes: 1,
            system_total_bytes: 2,
            processes: vec![],
            gpu_utilization_percent: None,
            gpu_unavailable_reason: "unavailable".into(),
        }
    }
    #[test]
    fn clock_retention_and_staleness() {
        let now = Instant::now();
        let mut h = SampleHistory::default();
        for n in 0..400 {
            h.push(sample(n * 1000), now);
        }
        assert_eq!(h.samples().len(), 300);
        assert!(!h.is_stale(now + Duration::from_secs(2)));
        assert!(h.is_stale(now + STALE_AFTER));
        h.push(sample(900_000), now);
        assert_eq!(h.samples().len(), 1);
        let mut clock = RequestClock::new(now);
        clock.output("", now);
        clock.output("reasoning", now + Duration::from_millis(20));
        clock.output("answer", now + Duration::from_millis(40));
        assert_eq!(
            clock
                .finish(now + Duration::from_secs(1))
                .time_to_first_output_ms,
            Some(20)
        );
    }
    #[test]
    fn replace_steps_retries_unknown_and_restart() {
        let mut ledger = UsageLedger::default();
        let mut row = ModelRequestUsage {
            request_id: "step1".into(),
            session_id: "s".into(),
            turn_id: "t".into(),
            attempt_id: "a".into(),
            model: "local".into(),
            usage: ReportedUsage {
                prompt_tokens: Some(5),
                completion_tokens: Some(10),
                total_tokens: Some(15),
                reasoning_tokens: Some(4),
            },
            timing: ClientTiming {
                elapsed_ms: Some(2000),
                time_to_first_output_ms: None,
            },
        };
        ledger.replace(row.clone());
        ledger.replace(row.clone());
        row.request_id = "step2".into();
        ledger.replace(row.clone());
        row.request_id = "retry".into();
        row.attempt_id = "b".into();
        row.usage = ReportedUsage::default();
        ledger.replace(row);
        let ledger: UsageLedger =
            serde_json::from_str(&serde_json::to_string(&ledger).unwrap()).unwrap();
        assert_eq!(
            ledger.requests["step1"]
                .timing
                .end_to_end_generated_tokens_per_second(&ledger.requests["step1"].usage),
            Some(5.0)
        );
        let total = ledger.session_totals("s");
        assert_eq!(total.total_tokens_known, 30);
        assert_eq!(total.completion_tokens_known, 20);
        assert_eq!(total.reasoning_tokens_known, 8);
        assert_eq!(total.requests_with_unknown_counts, 1);
        assert!(total.partial);
    }
    #[test]
    fn unsupported_runtime_metrics_are_unknown() {
        let metrics = RuntimeMetrics::from_status(
            &serde_json::json!({"metal":{"active_memory_gb":-1},"generation_tps":"bad"}),
        );
        assert_eq!(metrics, RuntimeMetrics::default());
    }
    #[test]
    fn worker_is_serial_and_stops() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let calls = Arc::new(AtomicUsize::new(0));
        let active = Arc::new(AtomicUsize::new(0));
        let c = calls.clone();
        let a = active.clone();
        let cancel = CancellationToken::new();
        let worker_cancel = cancel.clone();
        let handle = spawn_sampler(
            move || {
                assert_eq!(a.fetch_add(1, Ordering::SeqCst), 0);
                c.fetch_add(1, Ordering::SeqCst);
                std::thread::sleep(Duration::from_millis(1100));
                a.fetch_sub(1, Ordering::SeqCst);
                worker_cancel.cancel();
                sample(0)
            },
            Arc::new(Mutex::new(SampleHistory::default())),
            cancel,
        );
        handle.join().unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(active.load(Ordering::SeqCst), 0);
    }
}
