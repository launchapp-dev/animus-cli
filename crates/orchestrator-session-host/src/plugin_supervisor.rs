use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use orchestrator_plugin_host::HostError;

#[derive(Debug, Clone)]
pub struct SupervisorConfig {
    pub max_restarts_per_window: u32,
    pub window_duration: Duration,
    pub disable_cooldown: Duration,
}

impl Default for SupervisorConfig {
    // WHY 3-per-60s + 5min cooldown: a healthy plugin should never restart at
    // all; a single crash + retry is acceptable; three crashes within a minute
    // indicates a structurally broken plugin (bad binary, env issue, OOM loop)
    // that will only get worse if we keep respawning it. The 5-minute cooldown
    // matches the typical CI/dev iteration cycle so an operator who fixes the
    // root cause doesn't have to restart the daemon to re-enable the plugin.
    fn default() -> Self {
        Self {
            max_restarts_per_window: 3,
            window_duration: Duration::from_mins(1),
            disable_cooldown: Duration::from_mins(5),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SupervisorError {
    #[error("plugin '{plugin}' exceeded restart budget ({count} restarts in {window:?}); marked disabled")]
    TooManyRestarts { plugin: String, count: u32, window: Duration },
    #[error("plugin '{plugin}' is currently disabled; retry after {retry_after:?}")]
    PluginDisabled { plugin: String, retry_after: Duration },
}

pub struct PluginSupervisor {
    plugin_name: String,
    restart_count: AtomicU32,
    window_start: Mutex<Instant>,
    disabled_until: Mutex<Option<Instant>>,
    config: SupervisorConfig,
}

impl PluginSupervisor {
    pub fn new(plugin_name: impl Into<String>, config: SupervisorConfig) -> Self {
        Self {
            plugin_name: plugin_name.into(),
            restart_count: AtomicU32::new(0),
            window_start: Mutex::new(Instant::now()),
            disabled_until: Mutex::new(None),
            config,
        }
    }

    pub fn with_defaults(plugin_name: impl Into<String>) -> Self {
        Self::new(plugin_name, SupervisorConfig::default())
    }

    pub fn plugin_name(&self) -> &str {
        &self.plugin_name
    }

    pub fn config(&self) -> &SupervisorConfig {
        &self.config
    }

    pub fn is_disabled(&self) -> bool {
        let mut guard = self.disabled_until.lock().expect("disabled_until mutex poisoned");
        match *guard {
            Some(deadline) if Instant::now() < deadline => true,
            Some(_) => {
                *guard = None;
                self.restart_count.store(0, Ordering::SeqCst);
                *self.window_start.lock().expect("window_start mutex poisoned") = Instant::now();
                false
            }
            None => false,
        }
    }

    pub fn disabled_remaining(&self) -> Option<Duration> {
        let guard = self.disabled_until.lock().expect("disabled_until mutex poisoned");
        match *guard {
            Some(deadline) => {
                let now = Instant::now();
                if now < deadline {
                    Some(deadline - now)
                } else {
                    None
                }
            }
            None => None,
        }
    }

    pub fn record_restart(&self) -> Result<(), SupervisorError> {
        if let Some(retry_after) = self.disabled_remaining() {
            if !self.is_disabled() {
                // cooldown elapsed between calls; fall through.
            } else {
                return Err(SupervisorError::PluginDisabled { plugin: self.plugin_name.clone(), retry_after });
            }
        }

        let mut window_start = self.window_start.lock().expect("window_start mutex poisoned");
        let now = Instant::now();
        if now.duration_since(*window_start) > self.config.window_duration {
            *window_start = now;
            self.restart_count.store(0, Ordering::SeqCst);
        }
        drop(window_start);

        let new_count = self.restart_count.fetch_add(1, Ordering::SeqCst) + 1;
        if new_count > self.config.max_restarts_per_window {
            let mut disabled = self.disabled_until.lock().expect("disabled_until mutex poisoned");
            *disabled = Some(Instant::now() + self.config.disable_cooldown);
            return Err(SupervisorError::TooManyRestarts {
                plugin: self.plugin_name.clone(),
                count: new_count,
                window: self.config.window_duration,
            });
        }
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn restart_count_for_test(&self) -> u32 {
        self.restart_count.load(Ordering::SeqCst)
    }

    #[doc(hidden)]
    pub fn restart_count_for_test_public(&self) -> u32 {
        self.restart_count.load(Ordering::SeqCst)
    }

    #[doc(hidden)]
    pub fn force_disable_for_test_public(&self, cooldown: Duration) {
        let mut disabled = self.disabled_until.lock().expect("disabled_until mutex poisoned");
        *disabled = Some(Instant::now() + cooldown);
    }
}

/// Classify an `RpcError` returned from a plugin request. Codes in the
/// well-known JSON-RPC range (-32700..-32600) are structured errors emitted
/// BY THE PLUGIN; the plugin process is still alive and a retry would just
/// re-elicit the same error. Anything else (typically our internal
/// `INTERNAL_ERROR` wrapping a `ConnectionLost` / I/O error) means the
/// process died mid-call and a fresh spawn could succeed.
pub fn is_structured_jsonrpc_error(code: i32) -> bool {
    (-32700..=-32600).contains(&code)
}

/// What the dispatcher should do about a failed plugin request.
///
/// Returned by [`classify`]; replaces the brittle string-substring matching
/// that the now-removed `is_death_like_error` helper used to perform on
/// `RpcError.message`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetryDecision {
    /// The plugin process is presumed dead (connection lost, timeout,
    /// process-exited, or any other unclassified host-level failure). A
    /// fresh spawn could succeed; the dispatcher's retry-once policy
    /// applies.
    DeathLike,
    /// The plugin returned a structured JSON-RPC error frame. The process
    /// is still alive; retrying would re-elicit the same error. The
    /// dispatcher must surface this to the caller without consuming any
    /// of the supervisor's restart budget.
    StructuredError,
}

/// Typed classifier for plugin host failures. Pattern-matches on the
/// structural [`HostError`] enum instead of parsing error-message
/// substrings — so upstream phrasing changes (e.g. tokio renaming "broken
/// pipe" to something else, or `serde_json` reformatting an I/O message)
/// can't silently regress the retry policy.
///
/// - `HostError::ConnectionLost` / `HostError::Timeout` /
///   `HostError::ProcessExited` are unambiguously death-like.
/// - `HostError::Rpc(_)` is a structured plugin-side error.
/// - Other variants (`IncompatibleProtocol`, `CapabilityNotSupported`)
///   would never reach the retry path in production — they fire before a
///   request is sent — so the conservative default is `DeathLike` (the
///   dispatcher will burn a restart slot rather than silently dropping
///   the failure).
pub fn classify(err: &HostError) -> RetryDecision {
    match err {
        HostError::Rpc(rpc_err) if is_structured_jsonrpc_error(rpc_err.code) => RetryDecision::StructuredError,
        HostError::Rpc(_) => RetryDecision::DeathLike,
        HostError::ConnectionLost | HostError::Timeout(_) | HostError::ProcessExited(_) => RetryDecision::DeathLike,
        HostError::IncompatibleProtocol(_) | HostError::CapabilityNotSupported(_) => RetryDecision::DeathLike,
    }
}

/// Receives a duration sample for every plugin dispatch round-trip. The
/// dispatch path calls [`DispatchObserver::observe_duration`] after every
/// `request_typed` returns (success OR failure) so out-of-tree consumers
/// (e.g. the daemon-runtime metrics layer) can wire a
/// `plugin_request_duration_seconds` histogram without
/// `orchestrator-session-host` having to depend on the daemon runtime
/// directly.
///
/// The default [`NoopDispatchObserver`] is a no-op so callers that don't
/// care about observability pay zero overhead.
pub trait DispatchObserver: Send + Sync {
    fn observe_duration(&self, plugin: &str, method: &str, elapsed: Duration);
}

/// Default observer that discards every sample. Used when the dispatcher
/// has not been wired to a real metrics sink.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoopDispatchObserver;

impl DispatchObserver for NoopDispatchObserver {
    fn observe_duration(&self, _plugin: &str, _method: &str, _elapsed: Duration) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> SupervisorConfig {
        SupervisorConfig {
            max_restarts_per_window: 3,
            window_duration: Duration::from_mins(1),
            disable_cooldown: Duration::from_millis(200),
        }
    }

    #[test]
    fn supervisor_records_restart_within_window() {
        let sup = PluginSupervisor::new("plug", test_config());
        sup.record_restart().expect("first restart ok");
        sup.record_restart().expect("second restart ok");
        sup.record_restart().expect("third restart ok");
        assert_eq!(sup.restart_count_for_test(), 3);
        assert!(!sup.is_disabled());
    }

    #[test]
    fn supervisor_disables_plugin_after_3_restarts_in_60s() {
        let sup = PluginSupervisor::new("plug", test_config());
        sup.record_restart().expect("1");
        sup.record_restart().expect("2");
        sup.record_restart().expect("3");
        let err = sup.record_restart().expect_err("4th restart must fail");
        match err {
            SupervisorError::TooManyRestarts { plugin, count, .. } => {
                assert_eq!(plugin, "plug");
                assert_eq!(count, 4);
            }
            SupervisorError::PluginDisabled { .. } => panic!("expected TooManyRestarts, got PluginDisabled"),
        }
        assert!(sup.is_disabled(), "supervisor must be disabled after exceeding budget");
    }

    #[test]
    fn supervisor_re_enables_after_cooldown() {
        let sup = PluginSupervisor::new("plug", test_config());
        sup.record_restart().expect("1");
        sup.record_restart().expect("2");
        sup.record_restart().expect("3");
        let _ = sup.record_restart().expect_err("4th must trip disable");
        assert!(sup.is_disabled());
        std::thread::sleep(Duration::from_millis(250));
        assert!(!sup.is_disabled(), "cooldown should clear the disabled flag");
        assert_eq!(sup.restart_count_for_test(), 0, "counter resets on re-enable");
        sup.record_restart().expect("first restart in new window ok");
    }

    #[test]
    fn classify_jsonrpc_errors() {
        assert!(is_structured_jsonrpc_error(-32700));
        assert!(is_structured_jsonrpc_error(-32600));
        assert!(is_structured_jsonrpc_error(-32603));
        assert!(!is_structured_jsonrpc_error(-32003));
        assert!(!is_structured_jsonrpc_error(-32000));
        assert!(!is_structured_jsonrpc_error(0));
    }

    #[test]
    fn typed_classify_treats_connection_lost_as_death_like() {
        assert_eq!(classify(&HostError::ConnectionLost), RetryDecision::DeathLike);
    }

    #[test]
    fn typed_classify_treats_timeout_as_death_like() {
        assert_eq!(classify(&HostError::Timeout(Duration::from_secs(5))), RetryDecision::DeathLike);
    }

    #[test]
    fn typed_classify_treats_process_exited_as_death_like() {
        assert_eq!(classify(&HostError::ProcessExited("status=99".into())), RetryDecision::DeathLike);
    }

    #[test]
    fn typed_classify_treats_structured_rpc_error_as_no_retry() {
        let err =
            HostError::Rpc(animus_plugin_protocol::RpcError { code: -32602, message: "bad params".into(), data: None });
        assert_eq!(classify(&err), RetryDecision::StructuredError);
    }

    #[test]
    fn typed_classify_treats_internal_error_inside_well_known_range_as_no_retry() {
        // -32603 is the JSON-RPC "internal error" code; once the host has
        // already constructed a HostError::Rpc(_) variant, that means the
        // plugin actually returned a structured error frame (vs the host
        // synthesizing one from a transport failure — which would surface
        // as HostError::ConnectionLost). So this counts as structured.
        let err = HostError::Rpc(animus_plugin_protocol::RpcError {
            code: -32603,
            message: "plugin handler raised: KeyError".into(),
            data: None,
        });
        assert_eq!(classify(&err), RetryDecision::StructuredError);
    }

    #[test]
    fn typed_classify_treats_out_of_range_rpc_code_as_death_like() {
        // Application-specific error codes outside the well-known JSON-RPC
        // range fall through to DeathLike — the dispatcher's retry-once
        // logic gets a chance to discover whether a fresh spawn helps.
        let err = HostError::Rpc(animus_plugin_protocol::RpcError {
            code: -1,
            message: "custom plugin failure".into(),
            data: None,
        });
        assert_eq!(classify(&err), RetryDecision::DeathLike);
    }

    #[test]
    fn noop_dispatch_observer_swallows_samples() {
        let observer = NoopDispatchObserver;
        // Smoke test: the no-op observer must accept calls without panicking
        // or affecting any global state.
        observer.observe_duration("any-plugin", "any/method", Duration::from_millis(1));
    }
}
