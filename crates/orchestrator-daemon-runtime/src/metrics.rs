//! Daemon-side metrics collector.
//!
//! v0.4.12 ships rich durability primitives — workflow events, plugin
//! supervisor restart budgets, subject/log/event broadcast buses — but
//! operators have no quick way to introspect daemon health beyond
//! `daemon health`. This module exposes a lean lock-free counter +
//! gauge + histogram surface that hot paths can poke without contention
//! and that `daemon/metrics` (in-tree RPC method) snapshots on demand.
//!
//! Design notes:
//!
//! - Lock-free counters / gauges via `AtomicU64`. Atomic operations on
//!   the hot path; the snapshot grabs a short `RwLock::read` per
//!   histogram only.
//! - Histograms use fixed log-distributed buckets (1ms..=10s + a final
//!   "infinity" overflow). 10 buckets is enough to spot p50/p99 drift
//!   without pulling in a heavyweight tdigest dependency.
//! - Global singleton via `OnceLock`. The collector is process-wide so
//!   any call site in the daemon (or any crate that depends on it) can
//!   record metrics without threading a handle through every API.
//! - Snapshot output is plain JSON (counters / gauges / histograms). We
//!   deliberately do NOT emit Prometheus exposition format here — operators
//!   who need it can convert with a tiny shim. Keeping this layer
//!   dependency-free is more valuable than format breadth.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock, RwLock};
use std::time::{Duration, Instant};

use orchestrator_session_host::DispatchObserver;
use serde::{Deserialize, Serialize};

/// Fixed bucket upper bounds in seconds, log-distributed from 1ms to 10s.
///
/// 1ms covers in-memory branchwork; 10s is past the human-attention
/// threshold and any sample landing in the overflow row is already
/// pathological. We sample wallclock duration in fractional seconds so a
/// histogram observation that lands above the last bucket goes into the
/// implicit `+Inf` overflow.
const HISTOGRAM_BUCKETS_SECS: &[f64] = &[0.001, 0.005, 0.010, 0.050, 0.100, 0.500, 1.0, 2.5, 5.0, 10.0];

#[derive(Debug, Default, Serialize, Deserialize, Clone)]
pub struct HistogramSummary {
    pub count: u64,
    pub sum_seconds: f64,
    pub buckets: Vec<HistogramBucket>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct HistogramBucket {
    pub le_seconds: f64,
    pub count: u64,
}

#[derive(Debug, Default, Serialize, Deserialize, Clone)]
pub struct MetricsSnapshot {
    pub counters: HashMap<String, u64>,
    pub gauges: HashMap<String, f64>,
    pub histograms: HashMap<String, HistogramSummary>,
    pub captured_at: String,
    pub uptime_seconds: u64,
}

struct Counter {
    value: AtomicU64,
}

impl Counter {
    fn new() -> Self {
        Self { value: AtomicU64::new(0) }
    }

    fn inc_by(&self, n: u64) {
        self.value.fetch_add(n, Ordering::Relaxed);
    }

    fn get(&self) -> u64 {
        self.value.load(Ordering::Relaxed)
    }
}

struct Gauge {
    bits: AtomicU64,
}

impl Gauge {
    fn new() -> Self {
        Self { bits: AtomicU64::new(0f64.to_bits()) }
    }

    fn set(&self, v: f64) {
        self.bits.store(v.to_bits(), Ordering::Relaxed);
    }

    fn get(&self) -> f64 {
        f64::from_bits(self.bits.load(Ordering::Relaxed))
    }
}

struct Histogram {
    buckets: Vec<AtomicU64>,
    overflow: AtomicU64,
    sum_micros: AtomicU64,
    count: AtomicU64,
}

impl Histogram {
    fn new() -> Self {
        let buckets = HISTOGRAM_BUCKETS_SECS.iter().map(|_| AtomicU64::new(0)).collect();
        Self { buckets, overflow: AtomicU64::new(0), sum_micros: AtomicU64::new(0), count: AtomicU64::new(0) }
    }

    fn observe(&self, duration: Duration) {
        let secs = duration.as_secs_f64();
        let mut placed = false;
        for (i, edge) in HISTOGRAM_BUCKETS_SECS.iter().enumerate() {
            if secs <= *edge {
                self.buckets[i].fetch_add(1, Ordering::Relaxed);
                placed = true;
                break;
            }
        }
        if !placed {
            self.overflow.fetch_add(1, Ordering::Relaxed);
        }
        // sum_micros stores accumulated microseconds; converting back to
        // fractional seconds at snapshot time keeps the hot path integer-only.
        let micros = duration.as_micros() as u64;
        self.sum_micros.fetch_add(micros, Ordering::Relaxed);
        self.count.fetch_add(1, Ordering::Relaxed);
    }

    fn summary(&self) -> HistogramSummary {
        let mut buckets = Vec::with_capacity(HISTOGRAM_BUCKETS_SECS.len() + 1);
        for (i, edge) in HISTOGRAM_BUCKETS_SECS.iter().enumerate() {
            buckets.push(HistogramBucket { le_seconds: *edge, count: self.buckets[i].load(Ordering::Relaxed) });
        }
        buckets.push(HistogramBucket { le_seconds: f64::INFINITY, count: self.overflow.load(Ordering::Relaxed) });
        let sum_micros = self.sum_micros.load(Ordering::Relaxed) as f64;
        HistogramSummary { count: self.count.load(Ordering::Relaxed), sum_seconds: sum_micros / 1_000_000.0, buckets }
    }
}

pub struct Metrics {
    started_at: Instant,
    counters: RwLock<HashMap<String, Counter>>,
    gauges: RwLock<HashMap<String, Gauge>>,
    histograms: RwLock<HashMap<String, Histogram>>,
}

impl Default for Metrics {
    fn default() -> Self {
        Self::new()
    }
}

impl Metrics {
    pub fn new() -> Self {
        Self {
            started_at: Instant::now(),
            counters: RwLock::new(HashMap::new()),
            gauges: RwLock::new(HashMap::new()),
            histograms: RwLock::new(HashMap::new()),
        }
    }

    pub fn incr_counter(&self, name: &str) {
        self.incr_counter_by(name, 1);
    }

    pub fn incr_counter_by(&self, name: &str, n: u64) {
        if let Ok(guard) = self.counters.read() {
            if let Some(c) = guard.get(name) {
                c.inc_by(n);
                return;
            }
        }
        let mut guard = self.counters.write().expect("metrics counters poisoned");
        guard.entry(name.to_string()).or_insert_with(Counter::new).inc_by(n);
    }

    pub fn set_gauge(&self, name: &str, value: f64) {
        if let Ok(guard) = self.gauges.read() {
            if let Some(g) = guard.get(name) {
                g.set(value);
                return;
            }
        }
        let mut guard = self.gauges.write().expect("metrics gauges poisoned");
        guard.entry(name.to_string()).or_insert_with(Gauge::new).set(value);
    }

    pub fn observe_histogram(&self, name: &str, duration: Duration) {
        if let Ok(guard) = self.histograms.read() {
            if let Some(h) = guard.get(name) {
                h.observe(duration);
                return;
            }
        }
        let mut guard = self.histograms.write().expect("metrics histograms poisoned");
        guard.entry(name.to_string()).or_insert_with(Histogram::new).observe(duration);
    }

    pub fn snapshot(&self) -> MetricsSnapshot {
        let counters =
            self.counters.read().map(|g| g.iter().map(|(k, v)| (k.clone(), v.get())).collect()).unwrap_or_default();
        let mut gauges: HashMap<String, f64> =
            self.gauges.read().map(|g| g.iter().map(|(k, v)| (k.clone(), v.get())).collect()).unwrap_or_default();
        let histograms = self
            .histograms
            .read()
            .map(|g| g.iter().map(|(k, v)| (k.clone(), v.summary())).collect())
            .unwrap_or_default();

        let uptime_seconds = self.started_at.elapsed().as_secs();
        gauges.insert("daemon_uptime_seconds".to_string(), uptime_seconds as f64);

        MetricsSnapshot { counters, gauges, histograms, captured_at: chrono::Utc::now().to_rfc3339(), uptime_seconds }
    }
}

static GLOBAL_METRICS: OnceLock<Metrics> = OnceLock::new();

pub fn global() -> &'static Metrics {
    GLOBAL_METRICS.get_or_init(Metrics::new)
}

/// Convenience helpers — every call site uses one of these so naming
/// stays consistent across crates that emit metrics.
pub fn incr(name: &str) {
    global().incr_counter(name);
}

pub fn incr_by(name: &str, n: u64) {
    global().incr_counter_by(name, n);
}

pub fn set_gauge(name: &str, value: f64) {
    global().set_gauge(name, value);
}

pub fn observe(name: &str, duration: Duration) {
    global().observe_histogram(name, duration);
}

pub fn observe_labeled(name: &str, labels: &[(&str, &str)], duration: Duration) {
    global().observe_histogram(&labeled(name, labels), duration);
}

pub fn snapshot() -> MetricsSnapshot {
    global().snapshot()
}

/// Implements [`DispatchObserver`] by routing per-call duration samples
/// emitted by [`orchestrator_session_host::PluginSessionBackend`] into the
/// daemon-runtime global metrics registry as
/// `plugin_request_duration_seconds{plugin=...,method=...}` histogram
/// observations. The session-host crate cannot depend on daemon-runtime
/// directly, so the observer trait lives there and the metrics-aware
/// implementation is wired here.
#[derive(Debug, Default, Clone, Copy)]
pub struct MetricsObserver;

impl DispatchObserver for MetricsObserver {
    fn observe_duration(&self, plugin: &str, method: &str, elapsed: Duration) {
        observe_labeled("plugin_request_duration_seconds", &[("plugin", plugin), ("method", method)], elapsed);
    }
}

/// Shared metrics-recording observer. Use this at every
/// `PluginSessionBackend::with_dispatch_observer(...)` site so all
/// `agent/run`, `agent/cancel`, etc. plugin round-trips land in the
/// `plugin_request_duration_seconds` histogram.
pub fn metrics_dispatch_observer() -> Arc<dyn DispatchObserver> {
    Arc::new(MetricsObserver)
}

/// Bridge: route histogram observations emitted from workflow-runner-v2
/// (or any other crate that depends on it) into the daemon's global
/// metrics registry. Called once at daemon startup. Idempotent — a second
/// install is a no-op because the underlying registry uses `OnceLock`.
pub fn install_workflow_runner_metrics_bridge() {
    fn observer(name: &str, labels: &[(&str, &str)], duration: Duration) {
        observe_labeled(name, labels, duration);
    }
    let _ = animus_runtime_shared::metrics_hook::install_histogram_observer(observer);
}

/// Build a label-formatted metric key, e.g.
/// `labeled("workflow_runs_total", &[("status", "completed")])`
/// → `workflow_runs_total{status=completed}`.
pub fn labeled(name: &str, labels: &[(&str, &str)]) -> String {
    if labels.is_empty() {
        return name.to_string();
    }
    let body = labels.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join(",");
    format!("{name}{{{body}}}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counter_inc_and_snapshot() {
        let m = Metrics::new();
        m.incr_counter("workflow_runs_total");
        m.incr_counter("workflow_runs_total");
        m.incr_counter_by("phase_executions_total", 5);
        let snap = m.snapshot();
        assert_eq!(snap.counters.get("workflow_runs_total").copied(), Some(2));
        assert_eq!(snap.counters.get("phase_executions_total").copied(), Some(5));
    }

    #[test]
    fn gauge_set_and_uptime_present() {
        let m = Metrics::new();
        m.set_gauge("workflow_in_flight", 3.0);
        m.set_gauge("workflow_in_flight", 1.0);
        let snap = m.snapshot();
        assert_eq!(snap.gauges.get("workflow_in_flight").copied(), Some(1.0));
        assert!(snap.gauges.contains_key("daemon_uptime_seconds"));
    }

    #[test]
    fn histogram_bucketing_places_observations() {
        let m = Metrics::new();
        m.observe_histogram("phase_duration_seconds", Duration::from_millis(3));
        m.observe_histogram("phase_duration_seconds", Duration::from_millis(50));
        m.observe_histogram("phase_duration_seconds", Duration::from_secs(20));
        let snap = m.snapshot();
        let h = snap.histograms.get("phase_duration_seconds").expect("histogram present");
        assert_eq!(h.count, 3);
        assert!(h.sum_seconds > 20.0);
        let overflow = h.buckets.iter().find(|b| b.le_seconds.is_infinite()).expect("overflow bucket");
        assert_eq!(overflow.count, 1);
    }

    #[test]
    fn metrics_observer_increments_global_histogram() {
        let label = labeled("plugin_request_duration_seconds", &[("plugin", "test-plugin"), ("method", "agent/run")]);
        let before = global().snapshot().histograms.get(&label).map(|h| h.count).unwrap_or(0);

        let observer = MetricsObserver;
        observer.observe_duration("test-plugin", "agent/run", Duration::from_millis(7));
        observer.observe_duration("test-plugin", "agent/run", Duration::from_millis(42));

        let after = global().snapshot().histograms.get(&label).map(|h| h.count).unwrap_or(0);
        assert_eq!(after, before + 2);
    }

    #[test]
    fn labeled_key_formats_consistently() {
        assert_eq!(labeled("plugin_invocations_total", &[]), "plugin_invocations_total");
        assert_eq!(
            labeled("plugin_invocations_total", &[("plugin", "linear"), ("status", "success")]),
            "plugin_invocations_total{plugin=linear,status=success}"
        );
    }
}
