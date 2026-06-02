use std::sync::OnceLock;
use std::time::Duration;

pub type HistogramObserver = fn(name: &str, labels: &[(&str, &str)], duration: Duration);

static HISTOGRAM_OBSERVER: OnceLock<HistogramObserver> = OnceLock::new();

pub fn install_histogram_observer(observer: HistogramObserver) -> bool {
    HISTOGRAM_OBSERVER.set(observer).is_ok()
}

pub fn observe_histogram(name: &str, labels: &[(&str, &str)], duration: Duration) {
    if let Some(observer) = HISTOGRAM_OBSERVER.get() {
        observer(name, labels, duration);
    }
}

pub fn observe_phase_duration(phase_id: &str, duration: Duration) {
    observe_histogram("phase_duration_seconds", &[("phase_name", phase_id)], duration);
}
