use anyhow::{bail, Result};
use eventsource_stream::Eventsource;
use futures_util::StreamExt;
use std::collections::HashMap;
use std::io::Write;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use super::types::*;

static PROVIDER_STATES: RwLock<Option<HashMap<String, Arc<ProviderState>>>> = RwLock::new(None);

struct ProviderState {
    consecutive_failures: AtomicU32,
    circuit_open_until: AtomicU64,
    /// Whether a half-open probe request is currently in flight.
    /// Only one probe is allowed at a time; other callers see the circuit as open.
    probe_in_flight: AtomicBool,
}

const CIRCUIT_BREAKER_THRESHOLD: u32 = 5;
const CIRCUIT_BREAKER_COOLDOWN_SECS: u64 = 60;
/// Extended cooldown applied when a half-open probe request fails.
const CIRCUIT_BREAKER_EXTENDED_COOLDOWN_SECS: u64 = 120;

/// Current circuit breaker state for a provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CircuitState {
    /// Normal operation — requests flow through.
    Closed,
    /// Failure threshold exceeded — all requests rejected until cooldown expires.
    Open,
    /// Cooldown expired — a single probe request is allowed to test recovery.
    HalfOpen,
}

fn now_secs() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs()
}

/// Returns the current circuit breaker state without side effects.
fn circuit_state(state: &ProviderState) -> CircuitState {
    let until = state.circuit_open_until.load(Ordering::Relaxed);
    if until == 0 {
        return CircuitState::Closed;
    }
    if now_secs() < until {
        return CircuitState::Open;
    }
    CircuitState::HalfOpen
}

/// Check whether the circuit allows a request through.
///
/// Returns `Ok(())` if the request may proceed, or `Err(CircuitState)` if blocked.
/// When entering half-open, atomically claims the single probe slot via CAS
/// so that only one probe request is in flight at a time.
fn check_circuit(state: &ProviderState) -> Result<(), CircuitState> {
    let until = state.circuit_open_until.load(Ordering::Relaxed);
    if until == 0 {
        return Ok(()); // Closed — allow
    }
    let now = now_secs();
    if now < until {
        return Err(CircuitState::Open); // Still in cooldown
    }
    // Cooldown expired — half-open. Try to claim the single probe slot.
    match state.probe_in_flight.compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed) {
        Ok(_) => Ok(()),                   // Probe slot claimed
        Err(_) => Err(CircuitState::Open), // Another probe already in flight
    }
}

fn get_provider_state(api_base: &str) -> Arc<ProviderState> {
    {
        let read = PROVIDER_STATES.read().unwrap();
        if let Some(map) = read.as_ref() {
            if let Some(state) = map.get(api_base) {
                return state.clone();
            }
        }
    }

    let mut write = PROVIDER_STATES.write().unwrap();
    let map = write.get_or_insert_with(HashMap::new);
    map.entry(api_base.to_string())
        .or_insert_with(|| {
            Arc::new(ProviderState {
                consecutive_failures: AtomicU32::new(0),
                circuit_open_until: AtomicU64::new(0),
                probe_in_flight: AtomicBool::new(false),
            })
        })
        .clone()
}

/// Record a successful request. Resets the failure counter and closes the circuit.
/// If the circuit was half-open (probe succeeded), logs the state transition.
fn record_success(state: &ProviderState, api_base: &str) {
    let prev = circuit_state(state);
    state.consecutive_failures.store(0, Ordering::Relaxed);
    state.circuit_open_until.store(0, Ordering::Relaxed);
    state.probe_in_flight.store(false, Ordering::Relaxed);
    if prev == CircuitState::HalfOpen {
        eprintln!("[oai-runner] Circuit breaker CLOSED for {} — probe succeeded.", api_base);
    }
}

/// Record a failed request. Increments the consecutive failure counter.
/// Only transitions from Closed → Open when the threshold is exceeded.
/// Does not modify circuit state when already in Open or HalfOpen
/// (those transitions are handled by `record_probe_failure`).
fn record_failure(state: &ProviderState, api_base: &str) {
    let count = state.consecutive_failures.fetch_add(1, Ordering::Relaxed) + 1;
    if count >= CIRCUIT_BREAKER_THRESHOLD {
        let current = circuit_state(state);
        if current == CircuitState::Closed {
            let now = now_secs();
            state.circuit_open_until.store(now + CIRCUIT_BREAKER_COOLDOWN_SECS, Ordering::Relaxed);
            eprintln!(
                "[oai-runner] Circuit breaker OPEN for {} after {} consecutive failures. Cooling down for {}s.",
                api_base, count, CIRCUIT_BREAKER_COOLDOWN_SECS
            );
        }
    }
}

/// Record that a half-open probe request has definitively failed (all retries exhausted
/// or a non-retryable error). Re-opens the circuit with an extended cooldown.
fn record_probe_failure(state: &ProviderState, api_base: &str) {
    let now = now_secs();
    state.circuit_open_until.store(now + CIRCUIT_BREAKER_EXTENDED_COOLDOWN_SECS, Ordering::Relaxed);
    state.probe_in_flight.store(false, Ordering::Relaxed);
    eprintln!(
        "[oai-runner] Circuit breaker RE-OPENED for {} — probe failed. Extended cooldown for {}s.",
        api_base, CIRCUIT_BREAKER_EXTENDED_COOLDOWN_SECS
    );
}

pub struct ApiClient {
    http: reqwest::Client,
    api_base: String,
    api_key: String,
}

impl ApiClient {
    pub fn new(api_base: String, api_key: String, timeout_secs: u64) -> Self {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(timeout_secs))
            .build()
            .expect("failed to build HTTP client");
        Self { http, api_base, api_key }
    }

    pub async fn stream_chat(
        &self,
        request: &ChatRequest,
        on_text_chunk: &mut dyn FnMut(&str),
    ) -> Result<(ChatMessage, Option<UsageInfo>)> {
        let state = get_provider_state(&self.api_base);
        if let Err(_blocked) = check_circuit(&state) {
            bail!(
                "Circuit breaker is open for {} — too many consecutive API failures. Waiting for cooldown.",
                self.api_base
            );
        }
        let is_probe = circuit_state(&state) == CircuitState::HalfOpen;
        if is_probe {
            eprintln!("[oai-runner] Circuit breaker HALF-OPEN for {} — sending probe request.", self.api_base);
        }

        let url = format!("{}/chat/completions", self.api_base);

        let mut last_err = None;
        let mut emitted_any = false;

        for attempt in 0..3 {
            if attempt > 0 {
                if emitted_any {
                    // If we already sent chunks to the user, retrying the whole request
                    // will lead to duplicate output. Better to fail or implement resume.
                    // OpenAI-style APIs usually don't support resume mid-stream.
                    if is_probe {
                        record_probe_failure(&state, &self.api_base);
                    }
                    return Err(
                        last_err.unwrap_or_else(|| anyhow::anyhow!("Stream interrupted after emitting content"))
                    );
                }
                let delay = Duration::from_millis(500 * 2u64.pow(attempt as u32));
                tokio::time::sleep(delay).await;
            }

            let mut chunk_interceptor = |chunk: &str| {
                emitted_any = true;
                on_text_chunk(chunk);
            };

            match self.do_stream(&url, request, &mut chunk_interceptor).await {
                Ok(result) => {
                    record_success(&state, &self.api_base);
                    return Ok(result);
                }
                Err(e) => {
                    let err_str = e.to_string();
                    let is_rate_limit = err_str.contains("429");
                    let is_server_error = err_str.contains(" 5") && attempt < 2;
                    let is_transient = err_str.contains("EOF")
                        || err_str.contains("connection closed")
                        || err_str.contains("broken pipe")
                        || err_str.contains("reset by peer");

                    if is_rate_limit || is_server_error || is_transient {
                        record_failure(&state, &self.api_base);
                        let reason = if is_rate_limit {
                            "rate limited (429)"
                        } else if is_transient {
                            "transient connection error"
                        } else {
                            "server error"
                        };
                        eprintln!("[oai-runner] Retry {}/3: {}", attempt + 1, reason);
                        last_err = Some(e);
                        continue;
                    }
                    // Non-retryable error — record and return
                    record_failure(&state, &self.api_base);
                    if is_probe {
                        record_probe_failure(&state, &self.api_base);
                    }
                    return Err(e);
                }
            }
        }

        // All retries exhausted
        if is_probe {
            record_probe_failure(&state, &self.api_base);
        }
        Err(last_err.unwrap_or_else(|| anyhow::anyhow!("stream_chat failed after retries")))
    }

    async fn do_stream(
        &self,
        url: &str,
        request: &ChatRequest,
        on_text_chunk: &mut dyn FnMut(&str),
    ) -> Result<(ChatMessage, Option<UsageInfo>)> {
        let resp = self
            .http
            .post(url)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .header("User-Agent", "claude-code/2.1.80")
            .json(request)
            .send()
            .await?;

        let status = resp.status();
        if !status.is_success() {
            if status.as_u16() == 429 {
                if let Some(retry_after) = resp.headers().get("retry-after") {
                    if let Ok(secs) = retry_after.to_str().unwrap_or("0").parse::<u64>() {
                        let wait = secs.min(120);
                        eprintln!("[oai-runner] Rate limited. Retry-After: {}s", wait);
                        tokio::time::sleep(Duration::from_secs(wait)).await;
                    }
                }
            }
            let body = resp.text().await.unwrap_or_default();
            bail!("API returned {} {}: {}", status.as_u16(), status.as_str(), body);
        }

        let mut content = String::new();
        let mut tool_calls: Vec<ToolCall> = Vec::new();
        let mut usage: Option<UsageInfo> = None;

        let mut stream = resp.bytes_stream().eventsource();

        while let Some(event_result) = stream.next().await {
            let event = match event_result {
                Ok(event) => event,
                Err(e) => {
                    // If we fail mid-stream, we should probably return an error so stream_chat can decide to retry
                    bail!("SSE stream error: {}", e);
                }
            };

            if event.data == "[DONE]" {
                std::io::stdout().flush().ok();
                let msg = ChatMessage {
                    role: "assistant".to_string(),
                    content: if content.is_empty() { None } else { Some(content) },
                    reasoning_content: None,
                    tool_calls: if tool_calls.is_empty() { None } else { Some(tool_calls) },
                    tool_call_id: None,
                };
                return Ok((msg, usage));
            }

            let parsed: StreamChunk = match serde_json::from_str(&event.data) {
                Ok(c) => c,
                Err(_) => continue,
            };

            if let Some(u) = parsed.usage {
                usage = Some(u);
            }

            // Only care about the first choice for agent loop
            if let Some(choice) = parsed.choices.first() {
                if let Some(text) = &choice.delta.content {
                    content.push_str(text);
                    on_text_chunk(text);
                }

                if let Some(tc_deltas) = &choice.delta.tool_calls {
                    for tc_delta in tc_deltas {
                        let idx = tc_delta.index;

                        while tool_calls.len() <= idx {
                            tool_calls.push(ToolCall {
                                id: String::new(),
                                type_: "function".to_string(),
                                function: FunctionCall { name: String::new(), arguments: String::new() },
                            });
                        }

                        if let Some(id) = &tc_delta.id {
                            tool_calls[idx].id = id.clone();
                        }
                        if let Some(fc) = &tc_delta.function {
                            if let Some(name) = &fc.name {
                                tool_calls[idx].function.name = name.clone();
                            }
                            if let Some(args) = &fc.arguments {
                                tool_calls[idx].function.arguments.push_str(args);
                            }
                        }
                    }
                }
            }
        }

        std::io::stdout().flush().ok();
        let msg = ChatMessage {
            role: "assistant".to_string(),
            content: if content.is_empty() { None } else { Some(content) },
            reasoning_content: None,
            tool_calls: if tool_calls.is_empty() { None } else { Some(tool_calls) },
            tool_call_id: None,
        };
        Ok((msg, usage))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper to create a ProviderState in closed (initial) state.
    fn new_closed_state() -> ProviderState {
        ProviderState {
            consecutive_failures: AtomicU32::new(0),
            circuit_open_until: AtomicU64::new(0),
            probe_in_flight: AtomicBool::new(false),
        }
    }

    /// Helper to create a ProviderState in open state with the given cooldown end time.
    fn new_open_state(open_until: u64) -> ProviderState {
        ProviderState {
            consecutive_failures: AtomicU32::new(CIRCUIT_BREAKER_THRESHOLD),
            circuit_open_until: AtomicU64::new(open_until),
            probe_in_flight: AtomicBool::new(false),
        }
    }

    // ── circuit_state ──────────────────────────────────────────────

    #[test]
    fn circuit_starts_closed() {
        let state = new_closed_state();
        assert_eq!(circuit_state(&state), CircuitState::Closed);
    }

    #[test]
    fn circuit_is_open_during_cooldown() {
        let state = new_open_state(now_secs() + 60);
        assert_eq!(circuit_state(&state), CircuitState::Open);
    }

    #[test]
    fn circuit_is_half_open_after_cooldown_expires() {
        // circuit_open_until is in the past → half-open
        let state = new_open_state(now_secs() - 1);
        assert_eq!(circuit_state(&state), CircuitState::HalfOpen);
    }

    #[test]
    fn circuit_is_half_open_at_exact_expiry() {
        let state = new_open_state(now_secs());
        assert_eq!(circuit_state(&state), CircuitState::HalfOpen);
    }

    // ── check_circuit ──────────────────────────────────────────────

    #[test]
    fn check_circuit_allows_when_closed() {
        let state = new_closed_state();
        assert!(check_circuit(&state).is_ok());
    }

    #[test]
    fn check_circuit_blocks_when_open() {
        let state = new_open_state(now_secs() + 60);
        assert_eq!(check_circuit(&state), Err(CircuitState::Open));
    }

    #[test]
    fn check_circuit_allows_single_probe_when_half_open() {
        let state = new_open_state(now_secs() - 1);
        // First caller gets through (claims probe slot)
        assert!(check_circuit(&state).is_ok());
        // Second caller is blocked
        assert_eq!(check_circuit(&state), Err(CircuitState::Open));
    }

    #[test]
    fn check_circuit_probe_slot_released_on_success() {
        let state = new_open_state(now_secs() - 1);
        assert!(check_circuit(&state).is_ok());
        record_success(&state, "test-provider");
        // After success, circuit is closed — probe_in_flight cleared
        assert_eq!(circuit_state(&state), CircuitState::Closed);
        assert!(check_circuit(&state).is_ok());
    }

    #[test]
    fn check_circuit_probe_slot_released_on_probe_failure() {
        let state = new_open_state(now_secs() - 1);
        assert!(check_circuit(&state).is_ok());
        record_probe_failure(&state, "test-provider");
        // After probe failure, circuit is re-opened with extended cooldown
        assert_eq!(circuit_state(&state), CircuitState::Open);
    }

    // ── record_failure ─────────────────────────────────────────────

    #[test]
    fn record_failure_increments_count() {
        let state = new_closed_state();
        record_failure(&state, "test");
        record_failure(&state, "test");
        assert_eq!(state.consecutive_failures.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn record_failure_opens_circuit_at_threshold() {
        let state = new_closed_state();
        for _ in 0..CIRCUIT_BREAKER_THRESHOLD {
            record_failure(&state, "test");
        }
        assert_eq!(circuit_state(&state), CircuitState::Open);
    }

    #[test]
    fn record_failure_does_not_reopen_when_already_open() {
        let state = new_open_state(now_secs() + 60);
        let prev_until = state.circuit_open_until.load(Ordering::Relaxed);
        record_failure(&state, "test");
        // circuit_open_until should not have changed
        assert_eq!(state.circuit_open_until.load(Ordering::Relaxed), prev_until);
    }

    #[test]
    fn record_failure_does_not_reopen_when_half_open() {
        let state = new_open_state(now_secs() - 1);
        // Enter half-open by claiming probe
        assert!(check_circuit(&state).is_ok());
        let prev_until = state.circuit_open_until.load(Ordering::Relaxed);
        record_failure(&state, "test");
        // circuit_open_until should not change from intermediate failure
        assert_eq!(state.circuit_open_until.load(Ordering::Relaxed), prev_until);
        assert_eq!(circuit_state(&state), CircuitState::HalfOpen);
    }

    // ── record_success ─────────────────────────────────────────────

    #[test]
    fn record_success_resets_failures_in_closed_state() {
        let state = new_closed_state();
        record_failure(&state, "test");
        record_failure(&state, "test");
        record_success(&state, "test");
        assert_eq!(state.consecutive_failures.load(Ordering::Relaxed), 0);
        assert_eq!(circuit_state(&state), CircuitState::Closed);
    }

    #[test]
    fn record_success_closes_circuit_from_half_open() {
        let state = new_open_state(now_secs() - 1);
        // Enter half-open
        assert!(check_circuit(&state).is_ok());
        assert_eq!(circuit_state(&state), CircuitState::HalfOpen);
        // Probe succeeds
        record_success(&state, "test-provider");
        assert_eq!(circuit_state(&state), CircuitState::Closed);
        assert_eq!(state.consecutive_failures.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn record_success_clears_probe_in_flight() {
        let state = new_open_state(now_secs() - 1);
        assert!(check_circuit(&state).is_ok());
        assert!(state.probe_in_flight.load(Ordering::Relaxed));
        record_success(&state, "test");
        assert!(!state.probe_in_flight.load(Ordering::Relaxed));
    }

    // ── record_probe_failure ───────────────────────────────────────

    #[test]
    fn record_probe_failure_reopens_with_extended_cooldown() {
        let state = new_open_state(now_secs() - 1);
        // Enter half-open
        assert!(check_circuit(&state).is_ok());
        record_probe_failure(&state, "test-provider");
        assert_eq!(circuit_state(&state), CircuitState::Open);
        // Verify extended cooldown duration
        let until = state.circuit_open_until.load(Ordering::Relaxed);
        let expected = now_secs() + CIRCUIT_BREAKER_EXTENDED_COOLDOWN_SECS;
        // Allow 1-second tolerance for test execution time
        assert!(until >= expected && until <= expected + 2, "Expected cooldown end ~{expected}, got {until}");
    }

    #[test]
    fn record_probe_failure_clears_probe_in_flight() {
        let state = new_open_state(now_secs() - 1);
        assert!(check_circuit(&state).is_ok());
        assert!(state.probe_in_flight.load(Ordering::Relaxed));
        record_probe_failure(&state, "test");
        assert!(!state.probe_in_flight.load(Ordering::Relaxed));
    }

    // ── Full lifecycle: closed → open → half-open → closed ───────

    #[test]
    fn full_lifecycle_probe_succeeds() {
        let state = new_closed_state();
        let api = "https://api.example.com";

        // 1. Accumulate failures until threshold → Open
        for _ in 0..CIRCUIT_BREAKER_THRESHOLD {
            record_failure(&state, api);
        }
        assert_eq!(circuit_state(&state), CircuitState::Open);

        // 2. Requests are blocked while open
        assert_eq!(check_circuit(&state), Err(CircuitState::Open));

        // 3. Simulate cooldown expiry
        state.circuit_open_until.store(now_secs() - 1, Ordering::Relaxed);
        assert_eq!(circuit_state(&state), CircuitState::HalfOpen);

        // 4. Probe request allowed
        assert!(check_circuit(&state).is_ok());

        // 5. Other requests blocked while probe in flight
        assert_eq!(check_circuit(&state), Err(CircuitState::Open));

        // 6. Probe succeeds → circuit closes
        record_success(&state, api);
        assert_eq!(circuit_state(&state), CircuitState::Closed);

        // 7. Normal requests flow again
        assert!(check_circuit(&state).is_ok());
        assert!(check_circuit(&state).is_ok());
    }

    #[test]
    fn full_lifecycle_probe_fails_then_succeeds() {
        let state = new_closed_state();
        let api = "https://api.example.com";

        // 1. Open circuit
        for _ in 0..CIRCUIT_BREAKER_THRESHOLD {
            record_failure(&state, api);
        }
        assert_eq!(circuit_state(&state), CircuitState::Open);

        // 2. Cooldown expires → half-open
        state.circuit_open_until.store(now_secs() - 1, Ordering::Relaxed);

        // 3. Probe allowed
        assert!(check_circuit(&state).is_ok());

        // 4. Intermediate retry failure (should NOT re-open)
        record_failure(&state, api);
        assert_eq!(circuit_state(&state), CircuitState::HalfOpen);

        // 5. Probe ultimately fails → re-open with extended cooldown
        record_probe_failure(&state, api);
        assert_eq!(circuit_state(&state), CircuitState::Open);
        let until = state.circuit_open_until.load(Ordering::Relaxed);
        let expected = now_secs() + CIRCUIT_BREAKER_EXTENDED_COOLDOWN_SECS;
        assert!(until >= expected - 1);

        // 6. Extended cooldown expires → half-open again
        state.circuit_open_until.store(now_secs() - 1, Ordering::Relaxed);

        // 7. New probe allowed
        assert!(check_circuit(&state).is_ok());

        // 8. Probe succeeds → circuit closes
        record_success(&state, api);
        assert_eq!(circuit_state(&state), CircuitState::Closed);
        assert_eq!(state.consecutive_failures.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn failure_count_preserved_across_open_half_open_cycle() {
        let state = new_closed_state();
        let api = "https://api.example.com";

        // Open circuit
        for _ in 0..CIRCUIT_BREAKER_THRESHOLD {
            record_failure(&state, api);
        }
        let count_at_open = state.consecutive_failures.load(Ordering::Relaxed);
        assert_eq!(count_at_open, CIRCUIT_BREAKER_THRESHOLD as u32);

        // Enter half-open and record intermediate failure
        state.circuit_open_until.store(now_secs() - 1, Ordering::Relaxed);
        assert!(check_circuit(&state).is_ok());
        record_failure(&state, api);
        // Count should have incremented
        assert_eq!(state.consecutive_failures.load(Ordering::Relaxed), count_at_open + 1);

        // Probe fails
        record_probe_failure(&state, api);

        // Enter half-open again, probe succeeds
        state.circuit_open_until.store(now_secs() - 1, Ordering::Relaxed);
        assert!(check_circuit(&state).is_ok());
        record_success(&state, api);

        // Count reset on success
        assert_eq!(state.consecutive_failures.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn intermediate_probe_failures_do_not_reopen_circuit() {
        let state = new_open_state(now_secs() - 1);
        let api = "https://api.example.com";

        // Claim probe
        assert!(check_circuit(&state).is_ok());
        assert_eq!(circuit_state(&state), CircuitState::HalfOpen);

        // Multiple intermediate failures (simulating retries)
        record_failure(&state, api);
        record_failure(&state, api);
        record_failure(&state, api);

        // Still half-open — record_failure doesn't re-open
        assert_eq!(circuit_state(&state), CircuitState::HalfOpen);
    }
}
