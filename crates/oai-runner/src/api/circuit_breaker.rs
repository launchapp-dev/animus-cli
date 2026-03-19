use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CircuitState {
    Closed,
    Open,
    HalfOpen,
}

pub struct CircuitBreakerConfig {
    pub failure_threshold: u32,
    pub cooldown_secs: u64,
}

impl Default for CircuitBreakerConfig {
    fn default() -> Self {
        Self { failure_threshold: 5, cooldown_secs: 60 }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CircuitCheck {
    Allow,
    AllowProbe,
    Reject { open_until_secs: u64 },
}

struct ProviderState {
    state: CircuitState,
    consecutive_failures: u32,
    open_until_secs: u64,
    half_open_probe_in_flight: bool,
}

impl Default for ProviderState {
    fn default() -> Self {
        Self {
            state: CircuitState::Closed,
            consecutive_failures: 0,
            open_until_secs: 0,
            half_open_probe_in_flight: false,
        }
    }
}

static REGISTRY: OnceLock<Mutex<HashMap<String, ProviderState>>> = OnceLock::new();

fn registry() -> &'static Mutex<HashMap<String, ProviderState>> {
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

pub fn check_circuit(api_base: &str, _config: &CircuitBreakerConfig) -> CircuitCheck {
    let mut map = registry().lock().expect("circuit breaker registry lock");
    let state = map.entry(api_base.to_string()).or_default();
    match state.state {
        CircuitState::Closed => CircuitCheck::Allow,
        CircuitState::Open => {
            let now = now_secs();
            if now >= state.open_until_secs {
                state.state = CircuitState::HalfOpen;
                state.half_open_probe_in_flight = true;
                CircuitCheck::AllowProbe
            } else {
                CircuitCheck::Reject { open_until_secs: state.open_until_secs }
            }
        }
        CircuitState::HalfOpen => {
            if state.half_open_probe_in_flight {
                CircuitCheck::Reject { open_until_secs: state.open_until_secs }
            } else {
                state.half_open_probe_in_flight = true;
                CircuitCheck::AllowProbe
            }
        }
    }
}

pub fn record_success(api_base: &str) {
    let mut map = registry().lock().expect("circuit breaker registry lock");
    let state = map.entry(api_base.to_string()).or_default();
    state.state = CircuitState::Closed;
    state.consecutive_failures = 0;
    state.open_until_secs = 0;
    state.half_open_probe_in_flight = false;
}

pub fn record_failure(api_base: &str, config: &CircuitBreakerConfig) {
    let mut map = registry().lock().expect("circuit breaker registry lock");
    let state = map.entry(api_base.to_string()).or_default();
    match state.state {
        CircuitState::Closed => {
            state.consecutive_failures += 1;
            if state.consecutive_failures >= config.failure_threshold {
                let now = now_secs();
                state.state = CircuitState::Open;
                state.open_until_secs = now + config.cooldown_secs;
                eprintln!(
                    "[oai-runner] Circuit breaker OPEN for {} after {} consecutive failures. Cooldown: {}s.",
                    api_base, state.consecutive_failures, config.cooldown_secs
                );
            }
        }
        CircuitState::HalfOpen => {
            let now = now_secs();
            state.state = CircuitState::Open;
            state.open_until_secs = now + config.cooldown_secs;
            state.half_open_probe_in_flight = false;
            eprintln!(
                "[oai-runner] Circuit breaker probe FAILED for {}. Re-opening. Cooldown: {}s.",
                api_base, config.cooldown_secs
            );
        }
        CircuitState::Open => {
            let now = now_secs();
            state.open_until_secs = now + config.cooldown_secs;
        }
    }
}

#[cfg(test)]
pub fn get_state(api_base: &str) -> CircuitState {
    let map = registry().lock().expect("circuit breaker registry lock");
    map.get(api_base).map(|s| s.state.clone()).unwrap_or(CircuitState::Closed)
}

#[cfg(test)]
pub fn force_open_for_test(api_base: &str, open_until_secs: u64) {
    let mut map = registry().lock().expect("circuit breaker registry lock");
    let state = map.entry(api_base.to_string()).or_default();
    state.state = CircuitState::Open;
    state.open_until_secs = open_until_secs;
    state.consecutive_failures = 5;
    state.half_open_probe_in_flight = false;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn provider(tag: &str) -> String {
        format!("https://cb-test-{}.example.com/v1", tag)
    }

    #[test]
    fn starts_closed_for_unknown_provider() {
        assert_eq!(get_state(&provider("unknown")), CircuitState::Closed);
    }

    #[test]
    fn check_allows_when_closed() {
        let p = provider("check-closed");
        let cfg = CircuitBreakerConfig { failure_threshold: 5, cooldown_secs: 60 };
        assert_eq!(check_circuit(&p, &cfg), CircuitCheck::Allow);
    }

    #[test]
    fn trips_open_at_failure_threshold() {
        let p = provider("trips-at-threshold");
        let cfg = CircuitBreakerConfig { failure_threshold: 3, cooldown_secs: 60 };

        record_failure(&p, &cfg);
        assert_eq!(get_state(&p), CircuitState::Closed);
        record_failure(&p, &cfg);
        assert_eq!(get_state(&p), CircuitState::Closed);
        record_failure(&p, &cfg);
        assert_eq!(get_state(&p), CircuitState::Open);
    }

    #[test]
    fn rejects_requests_when_open() {
        let p = provider("rejects-when-open");
        let cfg = CircuitBreakerConfig { failure_threshold: 1, cooldown_secs: 3600 };

        record_failure(&p, &cfg);
        assert_eq!(get_state(&p), CircuitState::Open);

        let check = check_circuit(&p, &cfg);
        assert!(matches!(check, CircuitCheck::Reject { .. }));
    }

    #[test]
    fn transitions_to_half_open_after_cooldown_expires() {
        let p = provider("half-open-transition");
        force_open_for_test(&p, 0);

        let cfg = CircuitBreakerConfig { failure_threshold: 5, cooldown_secs: 60 };
        let check = check_circuit(&p, &cfg);
        assert_eq!(check, CircuitCheck::AllowProbe);
        assert_eq!(get_state(&p), CircuitState::HalfOpen);
    }

    #[test]
    fn half_open_rejects_concurrent_probe() {
        let p = provider("half-open-concurrent");
        force_open_for_test(&p, 0);

        let cfg = CircuitBreakerConfig { failure_threshold: 5, cooldown_secs: 60 };
        let first = check_circuit(&p, &cfg);
        assert_eq!(first, CircuitCheck::AllowProbe);

        let second = check_circuit(&p, &cfg);
        assert!(matches!(second, CircuitCheck::Reject { .. }));
    }

    #[test]
    fn closes_on_successful_probe() {
        let p = provider("closes-on-success");
        force_open_for_test(&p, 0);

        let cfg = CircuitBreakerConfig { failure_threshold: 5, cooldown_secs: 60 };
        let probe = check_circuit(&p, &cfg);
        assert_eq!(probe, CircuitCheck::AllowProbe);

        record_success(&p);
        assert_eq!(get_state(&p), CircuitState::Closed);
        assert_eq!(check_circuit(&p, &cfg), CircuitCheck::Allow);
    }

    #[test]
    fn reopens_on_failed_probe() {
        let p = provider("reopens-on-fail");
        force_open_for_test(&p, 0);

        let cfg = CircuitBreakerConfig { failure_threshold: 5, cooldown_secs: 60 };
        let probe = check_circuit(&p, &cfg);
        assert_eq!(probe, CircuitCheck::AllowProbe);
        assert_eq!(get_state(&p), CircuitState::HalfOpen);

        record_failure(&p, &cfg);
        assert_eq!(get_state(&p), CircuitState::Open);
    }

    #[test]
    fn success_resets_failure_count() {
        let p = provider("reset-on-success");
        let cfg = CircuitBreakerConfig { failure_threshold: 3, cooldown_secs: 60 };

        record_failure(&p, &cfg);
        record_failure(&p, &cfg);
        assert_eq!(get_state(&p), CircuitState::Closed);

        record_success(&p);

        record_failure(&p, &cfg);
        record_failure(&p, &cfg);
        assert_eq!(get_state(&p), CircuitState::Closed);
        record_failure(&p, &cfg);
        assert_eq!(get_state(&p), CircuitState::Open);
    }

    #[test]
    fn providers_are_isolated_from_each_other() {
        let pa = provider("isolated-a");
        let pb = provider("isolated-b");
        let cfg = CircuitBreakerConfig { failure_threshold: 2, cooldown_secs: 60 };

        record_failure(&pa, &cfg);
        record_failure(&pa, &cfg);
        assert_eq!(get_state(&pa), CircuitState::Open);

        assert_eq!(get_state(&pb), CircuitState::Closed);
        assert_eq!(check_circuit(&pb, &cfg), CircuitCheck::Allow);
    }
}
