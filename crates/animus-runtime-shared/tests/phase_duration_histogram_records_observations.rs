use std::sync::Mutex;
use std::time::Duration;

use animus_runtime_shared::metrics_hook::{install_histogram_observer, observe_phase_duration};

type RecordedObservation = (String, Vec<(String, String)>, Duration);

static RECORDED: Mutex<Vec<RecordedObservation>> = Mutex::new(Vec::new());

fn recording_observer(name: &str, labels: &[(&str, &str)], duration: Duration) {
    let owned_labels = labels.iter().map(|(k, v)| ((*k).to_string(), (*v).to_string())).collect();
    RECORDED.lock().expect("recorded poisoned").push((name.to_string(), owned_labels, duration));
}

#[tokio::test]
async fn observe_phase_duration_routes_to_installed_observer_in_expected_bucket() {
    assert!(install_histogram_observer(recording_observer), "observer install must succeed");

    let start = std::time::Instant::now();
    tokio::time::sleep(Duration::from_millis(20)).await;
    let measured = start.elapsed();

    observe_phase_duration("triage", measured);

    let recorded = RECORDED.lock().expect("recorded poisoned");
    assert_eq!(recorded.len(), 1, "exactly one observation should be recorded");
    let (name, labels, duration) = &recorded[0];
    assert_eq!(name, "phase_duration_seconds");
    assert_eq!(labels.as_slice(), &[("phase_name".to_string(), "triage".to_string())]);
    assert!(duration.as_millis() >= 20, "duration must reflect the simulated phase work: {duration:?}");
    assert!(duration.as_millis() < 5_000, "duration must not drift into pathological overflow: {duration:?}");

    let bucket_edges_secs = [0.001f64, 0.005, 0.010, 0.050, 0.100, 0.500, 1.0, 2.5, 5.0, 10.0];
    let secs = duration.as_secs_f64();
    let bucket =
        bucket_edges_secs.iter().position(|edge| secs <= *edge).expect("must land in defined bucket, not overflow");
    assert!(
        (3..=5).contains(&bucket),
        "20ms observation must land in 0.050s..=0.500s range (bucket index 3, 4, or 5), got bucket {bucket} (edge {})",
        bucket_edges_secs[bucket]
    );
}
