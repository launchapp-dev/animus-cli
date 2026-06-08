//! v0.5.8 small-core principal + RBAC primitives.
//!
//! Implements the four-chokepoint RBAC design from
//! [`docs/architecture/multi-tenant-rbac-v0.5.5.md`](../../../docs/architecture/multi-tenant-rbac-v0.5.5.md)
//! for chokepoint #1 only (control-dispatch hook). The other three
//! chokepoints (plugin mutation, secret read, audit write) are explicitly
//! deferred to v0.6 per the design doc.
//!
//! ## What's in
//!
//! - [`Principal`] enum with `User { os_user, principal_id }`, `Daemon`,
//!   and `ServiceAccount { id }` variants.
//! - [`RbacMode`] (single-user default | enforce) + [`RbacConfig`].
//! - [`PrincipalsFile`] parser for `~/.animus/principals.yaml`.
//! - [`bootstrap_principals_file_if_absent`] — write a default file on
//!   first boot mapping the current OS user to an `admin`-roled `local`
//!   principal. Never overwrites an existing file (collision guard from
//!   the design doc, §Risks #5).
//! - [`check_principal_can`] — the hardcoded role→permission table used
//!   by chokepoint #1. v0.5.8 ships `admin` (`*`) and `viewer`
//!   (read-only) only; future versions parse roles from
//!   `principals.yaml`.
//!
//! ## Compatibility
//!
//! `RbacMode::SingleUser` is the default. Under it, every permission
//! check is a no-op and existing single-user installs behave
//! bit-identically — see the [`check_principal_can`] short-circuit and
//! the matching test in `tests::single_user_mode_allows_everything`.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Who initiated a control RPC.
///
/// Threaded onto each control connection via the v0.5.8 dispatch hook.
/// Under [`RbacMode::SingleUser`] every connection resolves to the
/// bootstrapped local principal; the typed value still flows through so
/// audit logs can stamp `principal.id`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Principal {
    /// A human caller mapped from an OS user via `principals.yaml`.
    User {
        /// The OS user the caller authenticated as (peer-cred UID
        /// resolved to a username, or the daemon-owning user under
        /// single-user).
        os_user: String,
        /// The declared principal id from `principals.yaml`. Falls back
        /// to `os_user` under single-user when no declaration exists.
        principal_id: String,
    },
    /// The daemon itself (scheduler ticks, supervised plugin restarts,
    /// background reconciliation). Permission checks are bypassed for
    /// `Daemon` — the daemon is trusted.
    Daemon,
    /// A non-human caller (CI, MCP client) identified by a service
    /// account id declared in `principals.yaml`.
    ServiceAccount {
        /// Service-account id (e.g. `ci-runner`).
        id: String,
    },
}

impl Principal {
    /// Stable string id used for audit-log `principal.id`. Returns the
    /// declared principal id for users, `"daemon"` for the daemon, and
    /// the service-account id otherwise.
    #[must_use]
    pub fn id(&self) -> &str {
        match self {
            Self::User { principal_id, .. } => principal_id,
            Self::Daemon => "daemon",
            Self::ServiceAccount { id } => id,
        }
    }

    /// Stable kind label used for audit-log `principal.kind`. Matches
    /// the serde tag (`user` / `daemon` / `service_account`).
    #[must_use]
    pub fn kind(&self) -> &'static str {
        match self {
            Self::User { .. } => "user",
            Self::Daemon => "daemon",
            Self::ServiceAccount { .. } => "service_account",
        }
    }

    /// Build the canonical single-user principal: a `User` with both
    /// `os_user` and `principal_id` set to the current OS username,
    /// admin-roled by virtue of `RbacMode::SingleUser` short-circuit.
    #[must_use]
    pub fn local_for_os_user(os_user: impl Into<String>) -> Self {
        let os_user = os_user.into();
        Self::User { os_user: os_user.clone(), principal_id: os_user }
    }
}

/// Whether RBAC is enforced on the control dispatch hook.
///
/// `SingleUser` is the v0.5.8 default — every permission check is a
/// no-op so existing single-user installs are bit-identical. `Enforce`
/// consults the hardcoded role→permission table built into
/// [`check_principal_can`] and rejects any verb the principal's roles
/// don't allow.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RbacMode {
    /// Default. RBAC checks are no-ops. Existing single-user installs
    /// are unaffected.
    #[default]
    SingleUser,
    /// Opt-in. Every control RPC must match a permission allowed by at
    /// least one of the principal's roles.
    Enforce,
}

impl RbacMode {
    /// `true` when permission checks should be skipped.
    #[must_use]
    pub fn is_single_user(self) -> bool {
        matches!(self, Self::SingleUser)
    }
}

/// Project-level RBAC config. Lives under
/// [`crate::workflow_config::WorkflowConfig`] as the optional
/// `policy.rbac` block and falls back to defaults when absent.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RbacConfig {
    /// Enforcement mode. Defaults to [`RbacMode::SingleUser`].
    #[serde(default)]
    pub mode: RbacMode,
    /// Override for `~/.animus/principals.yaml`. Used by tests; ops
    /// should leave it unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub principals_file: Option<PathBuf>,
}

/// Parsed `~/.animus/principals.yaml`. Hand-editable (per the design
/// doc) — no CLI add/remove verbs land in v0.5.8.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PrincipalsFile {
    /// Top-level `policy:` block.
    #[serde(default)]
    pub policy: PrincipalsPolicy,
    /// Declared principals.
    #[serde(default)]
    pub principals: Vec<PrincipalEntry>,
}

/// `policy:` block of `principals.yaml`.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PrincipalsPolicy {
    /// RBAC enforcement mode. Mirrors [`RbacMode`].
    #[serde(default)]
    pub rbac: RbacMode,
    /// Fallback principal id when resolution misses (peer-cred UID has
    /// no `os_users` match). Only consulted under [`RbacMode::SingleUser`].
    #[serde(default)]
    pub default_principal: Option<String>,
}

/// One declared principal.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrincipalEntry {
    /// Stable id (slug). Surfaced in audit lines and `--as` args.
    pub id: String,
    /// Optional display name (currently unused; kept for forward-compat
    /// with the v0.6 web-UI principal switcher).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    /// `user` (default) or `service`.
    #[serde(default = "default_principal_kind")]
    pub kind: PrincipalKind,
    /// OS usernames that resolve to this principal via peer-cred.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub os_users: Vec<String>,
    /// Built-in roles assigned to this principal. v0.5.8 understands
    /// `admin` and `viewer` only.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub roles: Vec<String>,
}

/// `kind:` discriminator on a `PrincipalEntry`. v0.5.8 only distinguishes
/// human users from service accounts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PrincipalKind {
    /// Human user. Maps to [`Principal::User`].
    User,
    /// Non-human caller. Maps to [`Principal::ServiceAccount`].
    Service,
}

fn default_principal_kind() -> PrincipalKind {
    PrincipalKind::User
}

/// Errors surfaced while loading or bootstrapping the principals file.
#[derive(Debug, thiserror::Error)]
pub enum PrincipalsError {
    /// IO failure reading or writing `principals.yaml`.
    #[error("principals.yaml io error at {path}: {source}")]
    Io {
        /// Resolved file path.
        path: PathBuf,
        /// Underlying IO error.
        #[source]
        source: std::io::Error,
    },
    /// YAML parse failure.
    #[error("principals.yaml parse error at {path}: {source}")]
    Parse {
        /// Resolved file path.
        path: PathBuf,
        /// Underlying yaml error.
        #[source]
        source: serde_yaml::Error,
    },
    /// Could not determine current OS username for bootstrap.
    #[error("could not determine current OS username for principals.yaml bootstrap")]
    UnknownOsUser,
    /// Two or more `principals[].id` entries share the same id —
    /// rejected to keep `resolve_principal_by_id` deterministic.
    #[error("principals.yaml at {path} has duplicate principal id {id:?}")]
    DuplicateId {
        /// File path that contained the duplicate.
        path: PathBuf,
        /// The repeated id.
        id: String,
    },
}

/// Default path: `~/.animus/principals.yaml`. Respects the same
/// `ANIMUS_CONFIG_DIR` override as `protocol::Config::global_config_dir()`.
#[must_use]
pub fn default_principals_path() -> PathBuf {
    protocol::Config::global_config_dir().join("principals.yaml")
}

/// Load and parse `principals.yaml`. Returns `Ok(None)` when the file
/// does not exist.
///
/// Rejects files with duplicate `principals[].id` values — under
/// enforce, an `id`-keyed lookup that returns the wrong duplicate
/// would silently fail open. (codex round-9 P2.)
pub fn load_principals_file(path: &Path) -> Result<Option<PrincipalsFile>, PrincipalsError> {
    if !path.exists() {
        return Ok(None);
    }
    let body = fs::read_to_string(path).map_err(|source| PrincipalsError::Io { path: path.to_path_buf(), source })?;
    let parsed: PrincipalsFile =
        serde_yaml::from_str(&body).map_err(|source| PrincipalsError::Parse { path: path.to_path_buf(), source })?;
    let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
    for entry in &parsed.principals {
        if !seen.insert(entry.id.as_str()) {
            return Err(PrincipalsError::DuplicateId { path: path.to_path_buf(), id: entry.id.clone() });
        }
    }
    Ok(Some(parsed))
}

/// Write a default `principals.yaml` if and only if the file is absent.
///
/// The bootstrap content is the minimal viable single-user mapping:
/// one principal with id `local`, mapped to the current OS user, with
/// the `admin` role. Returns `Ok(true)` when a file was written,
/// `Ok(false)` when one already existed (collision guard from the
/// design doc, §Risks #5: never overwrite an existing file).
pub fn bootstrap_principals_file_if_absent(path: &Path) -> Result<bool, PrincipalsError> {
    let os_user = current_os_username().ok_or(PrincipalsError::UnknownOsUser)?;
    bootstrap_principals_file_if_absent_for(path, &os_user)
}

/// Variant of [`bootstrap_principals_file_if_absent`] with an explicit
/// OS username. Used by tests to avoid mutating the process-wide
/// `USER` env var, which would race against other tests that read
/// `scoped_state_root` via `protocol::scoped_state_root`.
pub fn bootstrap_principals_file_if_absent_for(path: &Path, os_user: &str) -> Result<bool, PrincipalsError> {
    if path.exists() {
        return Ok(false);
    }
    let file = PrincipalsFile {
        policy: PrincipalsPolicy { rbac: RbacMode::SingleUser, default_principal: Some("local".to_string()) },
        principals: vec![PrincipalEntry {
            id: "local".to_string(),
            display_name: None,
            kind: PrincipalKind::User,
            os_users: vec![os_user.to_string()],
            roles: vec!["admin".to_string()],
        }],
    };
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|source| PrincipalsError::Io { path: path.to_path_buf(), source })?;
    }
    let body =
        serde_yaml::to_string(&file).map_err(|source| PrincipalsError::Parse { path: path.to_path_buf(), source })?;
    // Atomic create-new: if a parallel bootstrap raced ahead of us
    // between the `path.exists()` check and now, `create_new` returns
    // `AlreadyExists`, which we treat as "another process wrote it
    // first" — preserves the no-overwrite guarantee (codex round-6 P2).
    use std::io::Write as _;
    let mut handle = match std::fs::OpenOptions::new().write(true).create_new(true).open(path) {
        Ok(handle) => handle,
        Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => return Ok(false),
        Err(source) => return Err(PrincipalsError::Io { path: path.to_path_buf(), source }),
    };
    handle.write_all(body.as_bytes()).map_err(|source| PrincipalsError::Io { path: path.to_path_buf(), source })?;
    Ok(true)
}

/// Resolve the current OS username. Uses `USER` env var first, falls
/// back to `LOGNAME`. Returns `None` when neither is set.
#[must_use]
pub fn current_os_username() -> Option<String> {
    std::env::var("USER")
        .ok()
        .filter(|v| !v.is_empty())
        .or_else(|| std::env::var("LOGNAME").ok().filter(|v| !v.is_empty()))
}

/// Resolve a peer OS user (from `getpeereid`/`SO_PEERCRED`) into a
/// declared principal entry. Returns `None` when the file is missing or
/// no entry lists the user.
#[must_use]
pub fn resolve_principal_for_os_user<'a>(file: &'a PrincipalsFile, os_user: &str) -> Option<&'a PrincipalEntry> {
    file.principals.iter().find(|p| p.os_users.iter().any(|u| u == os_user))
}

/// Resolve a declared principal entry by id. Used by `--as` lookups.
#[must_use]
pub fn resolve_principal_by_id<'a>(file: &'a PrincipalsFile, id: &str) -> Option<&'a PrincipalEntry> {
    file.principals.iter().find(|p| p.id == id)
}

/// Hardcoded role→permission table for v0.5.8.
///
/// Returns `true` when at least one of `roles` permits `method`.
/// `admin` matches everything; `viewer` matches a fixed read-only set.
/// Unknown roles match nothing.
///
/// Future v0.6 parses roles from `principals.yaml` `roles:` blocks; this
/// minimal table keeps the small core honest.
#[must_use]
pub fn role_allows_method(roles: &[String], method: &str) -> bool {
    for role in roles {
        if role == "admin" {
            return true;
        }
        if role == "viewer" && viewer_allows(method) {
            return true;
        }
    }
    false
}

fn viewer_allows(method: &str) -> bool {
    matches!(
        method,
        "workflow/list"
            | "workflow/get"
            | "workflow/events"
            | "subject/list"
            | "subject/get"
            | "subject/next"
            | "subject/watch"
            | "queue/list"
            | "queue/stats"
            | "daemon/status"
            | "daemon/health"
            | "daemon/agents"
            | "daemon/events"
            | "daemon/logs"
            | "daemon/metrics"
            | "agent/status"
            | "plugin/list"
            | "plugin/info"
            | "plugin/ping"
            | "plugin/search"
            | "plugin/browse"
            | "project/status"
    )
}

/// Result of a permission check at chokepoint #1.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PermissionDecision {
    /// The principal is allowed to invoke the method.
    Allow,
    /// The principal is forbidden. Includes a human-readable reason.
    Deny(String),
}

impl PermissionDecision {
    /// `true` if the decision is `Allow`.
    #[must_use]
    pub fn is_allowed(&self) -> bool {
        matches!(self, Self::Allow)
    }
}

/// Chokepoint #1: per-method permission check on the control dispatch.
///
/// Under [`RbacMode::SingleUser`] this is unconditionally `Allow` so
/// existing single-user installs see no behavior change. Under
/// [`RbacMode::Enforce`] the principal's declared roles are checked
/// against the hardcoded role→permission table.
///
/// `Principal::Daemon` is always allowed — the daemon is trusted (per
/// the design doc §Risks #2).
#[must_use]
pub fn check_principal_can(
    mode: RbacMode,
    principal: &Principal,
    method: &str,
    file: Option<&PrincipalsFile>,
) -> PermissionDecision {
    if mode.is_single_user() {
        return PermissionDecision::Allow;
    }
    if matches!(principal, Principal::Daemon) {
        return PermissionDecision::Allow;
    }
    let principal_id = principal.id();
    let Some(file) = file else {
        return PermissionDecision::Deny(format!(
            "principal {principal_id:?} cannot be authorized: principals.yaml not loaded under rbac=enforce"
        ));
    };
    let Some(entry) = resolve_principal_by_id(file, principal_id) else {
        return PermissionDecision::Deny(format!("principal {principal_id:?} is not declared in principals.yaml"));
    };
    if role_allows_method(&entry.roles, method) {
        PermissionDecision::Allow
    } else {
        PermissionDecision::Deny(format!(
            "principal {principal_id:?} with roles {:?} is not permitted to call {method:?}",
            entry.roles
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn principal_id_and_kind_labels_are_stable() {
        let user = Principal::User { os_user: "alice".to_string(), principal_id: "alice".to_string() };
        assert_eq!(user.id(), "alice");
        assert_eq!(user.kind(), "user");

        let daemon = Principal::Daemon;
        assert_eq!(daemon.id(), "daemon");
        assert_eq!(daemon.kind(), "daemon");

        let svc = Principal::ServiceAccount { id: "ci".to_string() };
        assert_eq!(svc.id(), "ci");
        assert_eq!(svc.kind(), "service_account");
    }

    #[test]
    fn principal_serializes_with_kind_tag() {
        let user = Principal::User { os_user: "alice".to_string(), principal_id: "alice".to_string() };
        let json = serde_json::to_string(&user).unwrap();
        assert!(json.contains("\"kind\":\"user\""), "got: {json}");
        assert!(json.contains("\"os_user\":\"alice\""));
    }

    #[test]
    fn single_user_mode_allows_everything() {
        let alice = Principal::User { os_user: "alice".to_string(), principal_id: "alice".to_string() };
        assert!(check_principal_can(RbacMode::SingleUser, &alice, "plugin/install", None).is_allowed());
        assert!(check_principal_can(RbacMode::SingleUser, &alice, "daemon/stop", None).is_allowed());
        assert!(check_principal_can(RbacMode::SingleUser, &alice, "anything/at/all", None).is_allowed());
    }

    #[test]
    fn enforce_mode_denies_unknown_principal() {
        let alice = Principal::User { os_user: "alice".to_string(), principal_id: "alice".to_string() };
        let empty = PrincipalsFile::default();
        let decision = check_principal_can(RbacMode::Enforce, &alice, "workflow/list", Some(&empty));
        assert!(!decision.is_allowed());
        match decision {
            PermissionDecision::Deny(msg) => assert!(msg.contains("alice"), "msg={msg}"),
            PermissionDecision::Allow => unreachable!(),
        }
    }

    #[test]
    fn enforce_mode_admin_role_allows_everything() {
        let alice = Principal::User { os_user: "alice".to_string(), principal_id: "alice".to_string() };
        let file = PrincipalsFile {
            policy: PrincipalsPolicy { rbac: RbacMode::Enforce, default_principal: None },
            principals: vec![PrincipalEntry {
                id: "alice".to_string(),
                display_name: None,
                kind: PrincipalKind::User,
                os_users: vec!["alice".to_string()],
                roles: vec!["admin".to_string()],
            }],
        };
        assert!(check_principal_can(RbacMode::Enforce, &alice, "plugin/install", Some(&file)).is_allowed());
        assert!(check_principal_can(RbacMode::Enforce, &alice, "daemon/stop", Some(&file)).is_allowed());
    }

    #[test]
    fn enforce_mode_viewer_role_allows_only_reads() {
        let bob = Principal::User { os_user: "bob".to_string(), principal_id: "bob".to_string() };
        let file = PrincipalsFile {
            policy: PrincipalsPolicy { rbac: RbacMode::Enforce, default_principal: None },
            principals: vec![PrincipalEntry {
                id: "bob".to_string(),
                display_name: None,
                kind: PrincipalKind::User,
                os_users: vec!["bob".to_string()],
                roles: vec!["viewer".to_string()],
            }],
        };
        assert!(check_principal_can(RbacMode::Enforce, &bob, "workflow/list", Some(&file)).is_allowed());
        assert!(check_principal_can(RbacMode::Enforce, &bob, "daemon/health", Some(&file)).is_allowed());
        assert!(!check_principal_can(RbacMode::Enforce, &bob, "plugin/install", Some(&file)).is_allowed());
        assert!(!check_principal_can(RbacMode::Enforce, &bob, "workflow/run", Some(&file)).is_allowed());
    }

    #[test]
    fn enforce_mode_daemon_always_allowed() {
        let file = PrincipalsFile::default();
        assert!(check_principal_can(RbacMode::Enforce, &Principal::Daemon, "anything", Some(&file)).is_allowed());
    }

    #[test]
    fn bootstrap_writes_default_file_when_absent() {
        // Hermetic: pass the OS user explicitly so we never mutate the
        // process-wide USER env var (which would race against any test
        // reading `protocol::scoped_state_root`).
        let dir = tempdir().unwrap();
        let path = dir.path().join("principals.yaml");
        let wrote = bootstrap_principals_file_if_absent_for(&path, "tester").unwrap();
        assert!(wrote);
        assert!(path.exists());
        let loaded = load_principals_file(&path).unwrap().unwrap();
        assert!(matches!(loaded.policy.rbac, RbacMode::SingleUser));
        assert_eq!(loaded.policy.default_principal.as_deref(), Some("local"));
        assert_eq!(loaded.principals.len(), 1);
        let entry = &loaded.principals[0];
        assert_eq!(entry.id, "local");
        assert_eq!(entry.roles, vec!["admin".to_string()]);
        assert!(entry.os_users.contains(&"tester".to_string()));
    }

    #[test]
    fn bootstrap_refuses_to_overwrite_existing_file() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("principals.yaml");
        fs::write(&path, "policy: {rbac: enforce}\nprincipals: []\n").unwrap();
        let wrote = bootstrap_principals_file_if_absent_for(&path, "tester").unwrap();
        assert!(!wrote, "bootstrap must never overwrite an existing principals.yaml");
        let body = fs::read_to_string(&path).unwrap();
        assert!(body.contains("rbac: enforce"));
    }

    #[test]
    fn resolve_by_os_user_walks_os_users_list() {
        let file = PrincipalsFile {
            policy: PrincipalsPolicy::default(),
            principals: vec![PrincipalEntry {
                id: "alice".to_string(),
                display_name: None,
                kind: PrincipalKind::User,
                os_users: vec!["alice".to_string(), "achen".to_string()],
                roles: vec!["operator".to_string()],
            }],
        };
        assert!(resolve_principal_for_os_user(&file, "achen").is_some());
        assert!(resolve_principal_for_os_user(&file, "unknown").is_none());
    }

    #[test]
    fn rbac_mode_serializes_kebab_case() {
        let json = serde_json::to_string(&RbacMode::SingleUser).unwrap();
        assert_eq!(json, "\"single-user\"");
        let json = serde_json::to_string(&RbacMode::Enforce).unwrap();
        assert_eq!(json, "\"enforce\"");
    }
}
