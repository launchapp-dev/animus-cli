use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

pub struct RunnerMetrics {
    started_at: Instant,
    runs_started: AtomicU64,
    runs_completed: AtomicU64,
    runs_failed: AtomicU64,
    runs_timed_out: AtomicU64,
    runs_cancelled: AtomicU64,
    total_duration_ms: AtomicU64,
}

impl Default for RunnerMetrics {
    fn default() -> Self {
        Self::new()
    }
}

impl RunnerMetrics {
    pub fn new() -> Self {
        Self {
            started_at: Instant::now(),
            runs_started: AtomicU64::new(0),
            runs_completed: AtomicU64::new(0),
            runs_failed: AtomicU64::new(0),
            runs_timed_out: AtomicU64::new(0),
            runs_cancelled: AtomicU64::new(0),
            total_duration_ms: AtomicU64::new(0),
        }
    }

    pub fn record_start(&self) {
        self.runs_started.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_completion(&self, duration_ms: u64) {
        self.runs_completed.fetch_add(1, Ordering::Relaxed);
        self.total_duration_ms.fetch_add(duration_ms, Ordering::Relaxed);
    }

    pub fn record_failure(&self, duration_ms: u64) {
        self.runs_failed.fetch_add(1, Ordering::Relaxed);
        self.total_duration_ms.fetch_add(duration_ms, Ordering::Relaxed);
    }

    pub fn record_timeout(&self) {
        self.runs_timed_out.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_cancellation(&self) {
        self.runs_cancelled.fetch_add(1, Ordering::Relaxed);
    }

    pub fn snapshot(&self) -> MetricsSnapshot {
        let total_runs = self.runs_started.load(Ordering::Relaxed);
        let completed = self.runs_completed.load(Ordering::Relaxed);
        let total_ms = self.total_duration_ms.load(Ordering::Relaxed);
        MetricsSnapshot {
            uptime_secs: self.started_at.elapsed().as_secs(),
            runs_started: total_runs,
            runs_completed: completed,
            runs_failed: self.runs_failed.load(Ordering::Relaxed),
            runs_timed_out: self.runs_timed_out.load(Ordering::Relaxed),
            runs_cancelled: self.runs_cancelled.load(Ordering::Relaxed),
            avg_duration_ms: if completed > 0 { total_ms / completed } else { 0 },
        }
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct MetricsSnapshot {
    pub uptime_secs: u64,
    pub runs_started: u64,
    pub runs_completed: u64,
    pub runs_failed: u64,
    pub runs_timed_out: u64,
    pub runs_cancelled: u64,
    pub avg_duration_ms: u64,
}
