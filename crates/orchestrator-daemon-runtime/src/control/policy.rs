//! v0.5.8 small-core RBAC policy hook on the control dispatch.
//!
//! Wires together three pieces of the design doc
//! ([`docs/architecture/multi-tenant-rbac-v0.5.5.md`](../../../../docs/architecture/multi-tenant-rbac-v0.5.5.md)):
//!
//! 1. Per-connection peer credential resolution (`getpeereid` /
//!    `SO_PEERCRED` under `cfg(unix)`).
//! 2. The honor-system `--as <principal>` override sent by the CLI as a
//!    `$/setPrincipal` JSON-RPC notification at connection open.
//! 3. The hardcoded role→permission table from
//!    [`orchestrator_core::check_principal_can`].
//!
//! Anything outside chokepoint #1 is **out of scope** for v0.5.8 — see
//! the design doc's "Explicit non-goals" section.
//!
//! ## Single-user default
//!
//! Under [`RbacMode::SingleUser`] the permission check is a no-op so
//! existing single-user installs stay bit-identical (no behavior change
//! on the wire, no peer-cred enforcement, no `--as` rejection).
//!
//! ## Enforce mode
//!
//! When `policy.rbac = "enforce"`:
//!
//! - The peer UID from `getpeereid` is resolved to an OS username and
//!   then to a declared `PrincipalEntry` in `principals.yaml`. A
//!   missing match fails closed (no anonymous access).
//! - `--as <other>` from a non-admin peer is rejected. Admins may
//!   impersonate (the design doc honor-system clause).
//! - Each dispatched method goes through
//!   [`orchestrator_core::check_principal_can`].
//!
//! Permission denials surface as JSON-RPC errors with a stable
//! `permission_denied` data tag so clients can distinguish from
//! unrelated `internal_error` responses.

use std::path::PathBuf;
use std::sync::{Arc, RwLock};

use animus_plugin_protocol::{error_codes, RpcError};
use orchestrator_core::principal::{
    check_principal_can, load_principals_file, resolve_principal_by_id, resolve_principal_for_os_user,
    PermissionDecision, Principal, PrincipalsFile, RbacMode,
};
use serde::Deserialize;
use serde_json::Value;

/// Shared, mostly-immutable policy state set once at daemon startup.
///
/// Cheap to clone via `Arc`. The principals file is loaded eagerly at
/// construction; reload is not currently exposed (the design doc calls
/// for a daemon restart after policy edits — see "Migration path"
/// step 5).
#[derive(Debug, Clone)]
pub struct PolicyState {
    inner: Arc<PolicyInner>,
}

#[derive(Debug)]
struct PolicyInner {
    mode: RbacMode,
    principals_path: Option<PathBuf>,
    principals_file: Option<PrincipalsFile>,
    /// When `Some`, every permission check denies regardless of mode.
    /// Set by [`PolicyState::deny_all`] to keep the control server
    /// running (and rejecting) when `principals.yaml` cannot be
    /// parsed.
    deny_all_reason: Option<String>,
}

impl PolicyState {
    /// Build a permissive [`PolicyState`] — single-user mode, no
    /// principals file. Used by tests and as the daemon default when
    /// the principals file is absent or unreadable.
    #[must_use]
    pub fn single_user() -> Self {
        Self {
            inner: Arc::new(PolicyInner {
                mode: RbacMode::SingleUser,
                principals_path: None,
                principals_file: None,
                deny_all_reason: None,
            }),
        }
    }

    /// Build a deny-all [`PolicyState`] that returns
    /// `permission_denied` for every dispatched method. Used when
    /// `principals.yaml` fails to parse — keeping the control server
    /// up means CLI clients can't escape to in-process fallback paths
    /// that bypass chokepoint #1. (codex round-8 P1)
    #[must_use]
    pub fn deny_all(reason: String) -> Self {
        Self {
            inner: Arc::new(PolicyInner {
                mode: RbacMode::Enforce,
                principals_path: None,
                principals_file: None,
                deny_all_reason: Some(reason),
            }),
        }
    }

    /// Build an enforcing [`PolicyState`] from an already-parsed
    /// principals file. Used by tests; production callers go through
    /// [`PolicyState::load`].
    #[must_use]
    pub fn enforce(file: PrincipalsFile) -> Self {
        Self {
            inner: Arc::new(PolicyInner {
                mode: RbacMode::Enforce,
                principals_path: None,
                principals_file: Some(file),
                deny_all_reason: None,
            }),
        }
    }

    /// Load policy state from `principals.yaml`. Missing file falls
    /// back to single-user; parse errors are surfaced.
    pub fn load(path: PathBuf) -> Result<Self, orchestrator_core::PrincipalsError> {
        let file = load_principals_file(&path)?;
        let (mode, file) = match file {
            Some(file) => (file.policy.rbac, Some(file)),
            None => (RbacMode::SingleUser, None),
        };
        Ok(Self {
            inner: Arc::new(PolicyInner {
                mode,
                principals_path: Some(path),
                principals_file: file,
                deny_all_reason: None,
            }),
        })
    }

    /// Active enforcement mode.
    #[must_use]
    pub fn mode(&self) -> RbacMode {
        self.inner.mode
    }

    /// Reference to the loaded principals file, if any.
    #[must_use]
    pub fn principals_file(&self) -> Option<&PrincipalsFile> {
        self.inner.principals_file.as_ref()
    }

    /// Resolved path the file was loaded from (debug surface).
    #[must_use]
    pub fn principals_path(&self) -> Option<&std::path::Path> {
        self.inner.principals_path.as_deref()
    }
}

impl Default for PolicyState {
    fn default() -> Self {
        Self::single_user()
    }
}

/// Per-connection mutable principal state.
///
/// Initialized from peer credentials at accept time and may be
/// overridden via `$/setPrincipal` (honor-system `--as`). Wrapped in an
/// `RwLock` so streaming-driver tasks can read it concurrently while a
/// fresh `$/setPrincipal` notification updates it.
#[derive(Debug)]
pub struct ConnectionPrincipal {
    state: RwLock<ConnectionPrincipalInner>,
}

#[derive(Debug, Clone)]
struct ConnectionPrincipalInner {
    /// Principal currently in effect for permission checks.
    effective: Principal,
    /// OS user resolved from peer credentials, when available. Used to
    /// gate `--as` overrides under enforce.
    peer_os_user: Option<String>,
}

/// Sentinel principal id for connections whose peer credentials could
/// not be resolved (peer-cred syscall failure, unsupported Unix target,
/// UID without a passwd entry). Designed to be impossible to match an
/// admin-roled entry in `principals.yaml` — the leading `:` is rejected
/// by the design-doc slug validation, so under `RbacMode::Enforce` the
/// `check_principal_can` lookup will always deny.
pub const UNRESOLVED_PEER_PRINCIPAL_ID: &str = ":unresolved-peer";

impl ConnectionPrincipal {
    /// Build with no peer-cred info (used by non-Unix builds and tests
    /// and as the fail-closed fallback when peer-cred resolution fails
    /// at accept time). The effective principal is a sentinel that
    /// cannot match any declared `PrincipalEntry`, so under
    /// `RbacMode::Enforce` every dispatched RPC fails closed with
    /// `permission_denied`.
    #[must_use]
    pub fn anonymous() -> Self {
        Self {
            state: RwLock::new(ConnectionPrincipalInner {
                effective: Principal::User {
                    os_user: UNRESOLVED_PEER_PRINCIPAL_ID.to_string(),
                    principal_id: UNRESOLVED_PEER_PRINCIPAL_ID.to_string(),
                },
                peer_os_user: None,
            }),
        }
    }

    /// Build from a resolved peer OS username. Looks the user up
    /// against `principals.yaml` if available to pick the declared
    /// principal id.
    ///
    /// **Enforce-mode fail-closed (codex round-4 P1):** when a
    /// `principals.yaml` is loaded and no entry declares this OS user
    /// in its `os_users` list, the effective principal id is set to
    /// the [`UNRESOLVED_PEER_PRINCIPAL_ID`] sentinel. This prevents the
    /// peer from accidentally inheriting the roles of a principal
    /// whose `id` happens to match the raw OS username (e.g. a service
    /// account id colliding with an OS account, or a typo in
    /// `os_users`).
    ///
    /// Under [`RbacMode::SingleUser`] the check is a no-op so the
    /// fallback mirrors the OS username into the id — single-user
    /// installs remain bit-identical.
    #[must_use]
    pub fn from_peer_os_user(os_user: String, policy: &PolicyState) -> Self {
        let effective = match policy.principals_file() {
            Some(file) => match resolve_principal_for_os_user(file, &os_user) {
                Some(entry) => Principal::User { os_user: os_user.clone(), principal_id: entry.id.clone() },
                None if policy.mode().is_single_user() => Principal::local_for_os_user(os_user.clone()),
                None => {
                    Principal::User { os_user: os_user.clone(), principal_id: UNRESOLVED_PEER_PRINCIPAL_ID.to_string() }
                }
            },
            None => Principal::local_for_os_user(os_user.clone()),
        };
        Self { state: RwLock::new(ConnectionPrincipalInner { effective, peer_os_user: Some(os_user) }) }
    }

    /// Snapshot of the effective principal.
    #[must_use]
    pub fn effective(&self) -> Principal {
        self.state.read().expect("connection principal lock poisoned").effective.clone()
    }

    /// Snapshot of the peer OS user, when peer-cred was available.
    #[must_use]
    pub fn peer_os_user(&self) -> Option<String> {
        self.state.read().expect("connection principal lock poisoned").peer_os_user.clone()
    }

    /// Apply an honor-system `--as <principal_id>` override.
    ///
    /// Under [`RbacMode::SingleUser`] every override is accepted with a
    /// `tracing::warn!` (matches the design-doc requirement that
    /// `--as` warn loudly when used).
    ///
    /// Under [`RbacMode::Enforce`]:
    ///
    /// - The requested principal id must be declared in `principals.yaml`.
    /// - The peer OS user must either match the target's `os_users`
    ///   list (self-claim is always fine) or have the `admin` role
    ///   (impersonation by admins is the design-doc honor-system clause).
    pub fn apply_as_override(&self, requested_id: &str, policy: &PolicyState) -> Result<(), AsOverrideError> {
        tracing::warn!(
            target: "animus.control.policy",
            requested = requested_id,
            "control connection accepted --as override (honor-system; logged loudly)"
        );

        if policy.mode().is_single_user() {
            self.commit_override(Principal::User {
                os_user: self.peer_os_user().unwrap_or_else(|| {
                    orchestrator_core::current_os_username().unwrap_or_else(|| "unknown".to_string())
                }),
                principal_id: requested_id.to_string(),
            });
            return Ok(());
        }

        let file =
            policy.principals_file().ok_or(AsOverrideError::UnknownPrincipal { id: requested_id.to_string() })?;
        let entry = resolve_principal_by_id(file, requested_id)
            .ok_or_else(|| AsOverrideError::UnknownPrincipal { id: requested_id.to_string() })?;

        let peer = self.peer_os_user();
        let is_self_claim = peer.as_deref().is_some_and(|os| entry.os_users.iter().any(|u| u == os));
        let peer_is_admin = peer
            .as_deref()
            .and_then(|os| resolve_principal_for_os_user(file, os))
            .map(|e| e.roles.iter().any(|r| r == "admin"))
            .unwrap_or(false);

        if !is_self_claim && !peer_is_admin {
            return Err(AsOverrideError::ImpersonationDenied {
                requested: requested_id.to_string(),
                peer: peer.unwrap_or_else(|| "unknown".to_string()),
            });
        }

        let os_user = if is_self_claim {
            peer.unwrap_or_else(|| entry.os_users.first().cloned().unwrap_or_default())
        } else {
            entry.os_users.first().cloned().unwrap_or_else(|| peer.clone().unwrap_or_default())
        };

        let principal = match entry.kind {
            orchestrator_core::PrincipalKind::Service => Principal::ServiceAccount { id: entry.id.clone() },
            orchestrator_core::PrincipalKind::User => Principal::User { os_user, principal_id: entry.id.clone() },
        };
        self.commit_override(principal);
        Ok(())
    }

    fn commit_override(&self, principal: Principal) {
        let mut guard = self.state.write().expect("connection principal lock poisoned");
        guard.effective = principal;
    }
}

/// Failure modes for [`ConnectionPrincipal::apply_as_override`].
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum AsOverrideError {
    /// The requested id is not declared under
    /// `principals.yaml#principals[].id`.
    #[error("--as {id:?} refers to an undeclared principal")]
    UnknownPrincipal {
        /// The id the client asked for.
        id: String,
    },
    /// The peer OS user is not allowed to impersonate the target.
    #[error("--as {requested:?} refused: peer {peer:?} is not admin and not self-claim")]
    ImpersonationDenied {
        /// Requested principal id.
        requested: String,
        /// Resolved peer OS user.
        peer: String,
    },
}

impl AsOverrideError {
    /// Convert to a wire-shaped JSON-RPC error.
    #[must_use]
    pub fn into_rpc_error(self) -> RpcError {
        RpcError {
            code: error_codes::INVALID_PARAMS,
            message: self.to_string(),
            data: Some(serde_json::json!({ "category": "permission_denied" })),
        }
    }
}

/// `$/setPrincipal` notification payload sent by the CLI to carry
/// `--as <principal>` to the daemon without bumping the upstream
/// control-protocol surface. The hook is honor-system; under
/// [`RbacMode::Enforce`] peer credentials are still used to gate it.
#[derive(Debug, Deserialize)]
pub struct SetPrincipalParams {
    /// Requested principal id.
    pub principal: String,
}

/// JSON-RPC notification method for the honor-system `--as` carrier.
pub const METHOD_SET_PRINCIPAL: &str = "$/setPrincipal";

/// Build a JSON-RPC permission_denied error for the dispatch hook.
#[must_use]
pub fn permission_denied_error(reason: String) -> RpcError {
    RpcError {
        code: error_codes::INVALID_PARAMS,
        message: reason,
        data: Some(serde_json::json!({ "category": "permission_denied" })),
    }
}

/// v0.5.8 chokepoint #1: per-method permission gate.
///
/// Returns `None` on allow; returns `Some(RpcError)` on deny so the
/// caller can short-circuit the dispatch with a clean wire error.
#[must_use]
pub fn check_method(policy: &PolicyState, principal: &ConnectionPrincipal, method: &str) -> Option<RpcError> {
    // Deny-all short-circuit (codex round-8 P1): when the policy is in
    // deny-all (principals.yaml parse failure), every method denies
    // regardless of role. Keeps the control server up so CLI clients
    // don't escape to in-process fallback paths that bypass the hook.
    if let Some(reason) = policy.inner.deny_all_reason.as_ref() {
        return Some(permission_denied_error(format!("control server in deny-all mode: {reason}")));
    }
    let effective = principal.effective();
    let decision = check_principal_can(policy.mode(), &effective, method, policy.principals_file());
    match decision {
        PermissionDecision::Allow => None,
        PermissionDecision::Deny(reason) => Some(permission_denied_error(reason)),
    }
}

/// Parse a `$/setPrincipal` notification payload.
pub fn parse_set_principal(params: &Value) -> Result<SetPrincipalParams, RpcError> {
    serde_json::from_value::<SetPrincipalParams>(params.clone()).map_err(|err| RpcError {
        code: error_codes::INVALID_PARAMS,
        message: format!("$/setPrincipal: invalid params: {err}"),
        data: Some(serde_json::json!({ "category": "invalid_params" })),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use orchestrator_core::principal::{PrincipalEntry, PrincipalKind, PrincipalsPolicy};

    fn enforce_file(principals: Vec<PrincipalEntry>) -> PrincipalsFile {
        PrincipalsFile { policy: PrincipalsPolicy { rbac: RbacMode::Enforce, default_principal: None }, principals }
    }

    #[test]
    fn single_user_check_method_always_allows() {
        let policy = PolicyState::single_user();
        let principal = ConnectionPrincipal::anonymous();
        assert!(check_method(&policy, &principal, "plugin/install").is_none());
        assert!(check_method(&policy, &principal, "daemon/stop").is_none());
    }

    #[test]
    fn deny_all_policy_rejects_every_method() {
        // Codex round-8 P1: deny-all keeps the wire surface up so CLI
        // helpers cannot fall back to in-process services that bypass
        // chokepoint #1.
        let policy = PolicyState::deny_all("test: principals.yaml unparseable".to_string());
        let principal = ConnectionPrincipal::anonymous();
        let err = check_method(&policy, &principal, "daemon/status").expect("deny-all must reject reads too");
        assert!(err.message.contains("deny-all"), "msg={}", err.message);
        let err = check_method(&policy, &principal, "plugin/install").expect("deny-all must reject writes");
        assert!(err.message.contains("deny-all"));
    }

    #[test]
    fn enforce_denies_unmapped_peer_with_colliding_id() {
        // Codex round-4 P1: a peer OS user `alice` whose username does
        // not appear in any `os_users` list must NOT inherit the roles
        // of a `principals.yaml` entry whose id happens to be `alice`.
        let policy = PolicyState::enforce(enforce_file(vec![PrincipalEntry {
            id: "alice".to_string(),
            display_name: None,
            kind: PrincipalKind::User,
            // Note: alice's id matches the OS user "alice" but the
            // os_users list does not include alice — for example a
            // service-account row or a typo.
            os_users: vec![],
            roles: vec!["admin".to_string()],
        }]));
        let principal = ConnectionPrincipal::from_peer_os_user("alice".to_string(), &policy);
        let err = check_method(&policy, &principal, "workflow/list")
            .expect("unmapped peer must not inherit colliding id roles");
        assert!(err.message.contains(":unresolved-peer"), "msg={}", err.message);
    }

    #[test]
    fn enforce_denies_anonymous_peer_even_if_daemon_owner_has_admin() {
        // Codex round-2 fix: ConnectionPrincipal::anonymous() seeds a
        // sentinel principal id that cannot match a declared entry, so
        // an unresolved peer cannot inherit the daemon owner's admin
        // role under enforce. Without this guard the daemon was
        // effectively fail-open on platforms where peer-cred is
        // unsupported.
        let policy = PolicyState::enforce(enforce_file(vec![PrincipalEntry {
            id: orchestrator_core::current_os_username().unwrap_or_else(|| "tester".to_string()),
            display_name: None,
            kind: PrincipalKind::User,
            os_users: vec![orchestrator_core::current_os_username().unwrap_or_else(|| "tester".to_string())],
            roles: vec!["admin".to_string()],
        }]));
        let principal = ConnectionPrincipal::anonymous();
        let err = check_method(&policy, &principal, "workflow/list").expect("anonymous must deny under enforce");
        assert!(err.message.contains(":unresolved-peer"), "msg={}", err.message);
    }

    #[test]
    fn enforce_check_method_denies_undeclared_principal() {
        let policy = PolicyState::enforce(enforce_file(vec![]));
        let principal = ConnectionPrincipal::from_peer_os_user("alice".to_string(), &policy);
        let err = check_method(&policy, &principal, "workflow/list").expect("undeclared principal must be denied");
        assert_eq!(err.code, error_codes::INVALID_PARAMS);
        assert!(err.message.contains("not declared"));
    }

    #[test]
    fn enforce_admin_role_passes_writes() {
        let policy = PolicyState::enforce(enforce_file(vec![PrincipalEntry {
            id: "alice".to_string(),
            display_name: None,
            kind: PrincipalKind::User,
            os_users: vec!["alice".to_string()],
            roles: vec!["admin".to_string()],
        }]));
        let principal = ConnectionPrincipal::from_peer_os_user("alice".to_string(), &policy);
        assert!(check_method(&policy, &principal, "plugin/install").is_none());
    }

    #[test]
    fn enforce_viewer_role_passes_reads_blocks_writes() {
        let policy = PolicyState::enforce(enforce_file(vec![PrincipalEntry {
            id: "bob".to_string(),
            display_name: None,
            kind: PrincipalKind::User,
            os_users: vec!["bob".to_string()],
            roles: vec!["viewer".to_string()],
        }]));
        let principal = ConnectionPrincipal::from_peer_os_user("bob".to_string(), &policy);
        assert!(check_method(&policy, &principal, "workflow/list").is_none());
        let err = check_method(&policy, &principal, "workflow/run").expect("viewer must not run workflows");
        assert!(err.message.contains("workflow/run"));
    }

    #[test]
    fn enforce_blocks_impersonation_by_non_admin() {
        let policy = PolicyState::enforce(enforce_file(vec![
            PrincipalEntry {
                id: "alice".to_string(),
                display_name: None,
                kind: PrincipalKind::User,
                os_users: vec!["alice".to_string()],
                roles: vec!["admin".to_string()],
            },
            PrincipalEntry {
                id: "bob".to_string(),
                display_name: None,
                kind: PrincipalKind::User,
                os_users: vec!["bob".to_string()],
                roles: vec!["viewer".to_string()],
            },
        ]));
        let principal = ConnectionPrincipal::from_peer_os_user("bob".to_string(), &policy);
        let err = principal.apply_as_override("alice", &policy).expect_err("viewer may not impersonate admin");
        assert!(matches!(err, AsOverrideError::ImpersonationDenied { .. }));
    }

    #[test]
    fn enforce_allows_self_claim_via_as() {
        let policy = PolicyState::enforce(enforce_file(vec![PrincipalEntry {
            id: "alice".to_string(),
            display_name: None,
            kind: PrincipalKind::User,
            os_users: vec!["alice".to_string()],
            roles: vec!["viewer".to_string()],
        }]));
        let principal = ConnectionPrincipal::from_peer_os_user("alice".to_string(), &policy);
        principal.apply_as_override("alice", &policy).expect("self-claim must succeed");
        match principal.effective() {
            Principal::User { principal_id, .. } => assert_eq!(principal_id, "alice"),
            other => panic!("expected user principal, got {other:?}"),
        }
    }

    #[test]
    fn enforce_admin_may_impersonate_other_principal() {
        let policy = PolicyState::enforce(enforce_file(vec![
            PrincipalEntry {
                id: "alice".to_string(),
                display_name: None,
                kind: PrincipalKind::User,
                os_users: vec!["alice".to_string()],
                roles: vec!["admin".to_string()],
            },
            PrincipalEntry {
                id: "ci".to_string(),
                display_name: None,
                kind: PrincipalKind::Service,
                os_users: vec![],
                roles: vec!["viewer".to_string()],
            },
        ]));
        let principal = ConnectionPrincipal::from_peer_os_user("alice".to_string(), &policy);
        principal.apply_as_override("ci", &policy).expect("admin may impersonate service");
        assert!(matches!(principal.effective(), Principal::ServiceAccount { .. }));
        assert_eq!(principal.effective().id(), "ci");
    }

    #[test]
    fn single_user_as_override_always_accepted() {
        let policy = PolicyState::single_user();
        let principal = ConnectionPrincipal::anonymous();
        principal.apply_as_override("anyone", &policy).expect("single-user accepts any --as");
        assert_eq!(principal.effective().id(), "anyone");
    }

    #[test]
    fn parse_set_principal_round_trip() {
        let params = serde_json::json!({ "principal": "alice" });
        let parsed = parse_set_principal(&params).unwrap();
        assert_eq!(parsed.principal, "alice");
    }
}
