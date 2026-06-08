use anyhow::Result;
use orchestrator_core::principal::{
    bootstrap_principals_file_if_absent, current_os_username, default_principals_path, load_principals_file,
    resolve_principal_by_id, resolve_principal_for_os_user, PrincipalKind, PrincipalsFile, RbacMode,
};
use serde::Serialize;

use crate::{print_value, AuthCommand};

/// v0.5.8 `animus auth ...` dispatch.
///
/// `as_principal` carries the `--as <id>` global flag so `whoami`
/// reports the effective principal a downstream daemon RPC would see.
pub(crate) async fn handle_auth(command: AuthCommand, as_principal: Option<String>, json: bool) -> Result<()> {
    match command {
        AuthCommand::Whoami => handle_whoami(as_principal, json),
    }
}

#[derive(Debug, Serialize)]
struct WhoamiOutput {
    /// `single-user` or `enforce` — whichever the principals file
    /// declares (or `single-user` when the file is absent).
    rbac_mode: &'static str,
    /// Effective principal id (after `--as` override resolution).
    principal_id: String,
    /// Effective principal kind (`user` / `daemon` / `service_account`).
    principal_kind: &'static str,
    /// OS user the CLI is running as.
    os_user: Option<String>,
    /// `true` when `--as` was used. Mirrors the loud-warn behavior of
    /// the daemon-side hook so scripts can detect impersonation.
    impersonated: bool,
    /// Roles declared for the effective principal in `principals.yaml`,
    /// when the file exists. Empty under single-user with no file.
    roles: Vec<String>,
    /// Resolved path to `principals.yaml` (debug surface).
    principals_path: String,
    /// `true` when the bootstrap path materialized a default file on
    /// this invocation.
    bootstrapped: bool,
}

fn handle_whoami(as_principal: Option<String>, json: bool) -> Result<()> {
    let path = default_principals_path();
    let bootstrapped = bootstrap_principals_file_if_absent(&path).unwrap_or(false);
    // Surface YAML parse / IO errors instead of silently degrading to
    // the single-user default — the daemon-side policy loader treats
    // the same parse failure as fail-closed (refuses to start the
    // control server), so `whoami` must not lie about the policy
    // state. (codex round-3 P2)
    let file = match load_principals_file(&path) {
        Ok(Some(file)) => file,
        Ok(None) => PrincipalsFile::default(),
        Err(err) => {
            return Err(anyhow::anyhow!(
                "principals.yaml at {} could not be loaded: {err} — fix the file (or remove it) before retrying",
                path.display()
            ));
        }
    };

    let os_user = current_os_username();
    let rbac_mode = file.policy.rbac;

    if as_principal.is_some() {
        eprintln!(
            "warning: --as is honor-system on the local Unix socket; the daemon rejects mismatches under policy.rbac=enforce"
        );
    }

    // Under enforce, reject `--as` overrides that the daemon's
    // `$/setPrincipal` hook would also reject. (codex round-4 P2 +
    // round-5 P2.) That means:
    //
    // - Unknown principal id => undeclared, deny.
    // - Self-claim (target.os_users contains the peer OS user) is
    //   always fine.
    // - Otherwise, the peer must have the `admin` role for
    //   impersonation to be honored.
    if matches!(rbac_mode, RbacMode::Enforce) {
        if let Some(ref id) = as_principal {
            let target = resolve_principal_by_id(&file, id).ok_or_else(|| {
                anyhow::anyhow!("--as {id:?} refers to an undeclared principal under policy.rbac=enforce")
            })?;
            let is_self_claim = os_user.as_deref().is_some_and(|u| target.os_users.iter().any(|tu| tu == u));
            let peer_is_admin = os_user
                .as_deref()
                .and_then(|u| resolve_principal_for_os_user(&file, u))
                .map(|e| e.roles.iter().any(|r| r == "admin"))
                .unwrap_or(false);
            if !is_self_claim && !peer_is_admin {
                return Err(anyhow::anyhow!(
                    "--as {id:?} refused: peer OS user {os_user:?} is not admin and not self-claim",
                ));
            }
        }
    }

    // Under enforce, an OS user with no `os_users` mapping is what the
    // daemon treats as `:unresolved-peer`; reflect that here so
    // `whoami` doesn't silently report a fake identity for the same
    // config error every RPC will hit. (codex round-9 P2.)
    if matches!(rbac_mode, RbacMode::Enforce) && as_principal.is_none() {
        let mapped = os_user.as_deref().and_then(|u| resolve_principal_for_os_user(&file, u));
        if mapped.is_none() {
            return Err(anyhow::anyhow!(
                "peer OS user {os_user:?} is not declared in principals.yaml; every daemon RPC will fail under policy.rbac=enforce"
            ));
        }
    }

    let (principal_id, principal_kind, roles) = match as_principal.as_deref() {
        Some(id) => resolved_for_id(&file, id, os_user.as_deref()),
        None => resolved_for_peer(&file, os_user.as_deref()),
    };

    let output = WhoamiOutput {
        rbac_mode: rbac_mode_label(rbac_mode),
        principal_id,
        principal_kind,
        os_user,
        impersonated: as_principal.is_some(),
        roles,
        principals_path: path.display().to_string(),
        bootstrapped,
    };
    print_value(output, json)
}

fn rbac_mode_label(mode: RbacMode) -> &'static str {
    match mode {
        RbacMode::SingleUser => "single-user",
        RbacMode::Enforce => "enforce",
    }
}

fn kind_label(kind: PrincipalKind) -> &'static str {
    match kind {
        PrincipalKind::User => "user",
        PrincipalKind::Service => "service_account",
    }
}

fn resolved_for_id(
    file: &PrincipalsFile,
    id: &str,
    fallback_os_user: Option<&str>,
) -> (String, &'static str, Vec<String>) {
    match resolve_principal_by_id(file, id) {
        Some(entry) => (entry.id.clone(), kind_label(entry.kind), entry.roles.clone()),
        None => (id.to_string(), "user", fallback_roles(file, fallback_os_user)),
    }
}

fn resolved_for_peer(file: &PrincipalsFile, os_user: Option<&str>) -> (String, &'static str, Vec<String>) {
    let Some(user) = os_user else {
        return ("unknown".to_string(), "user", Vec::new());
    };
    match resolve_principal_for_os_user(file, user) {
        Some(entry) => (entry.id.clone(), kind_label(entry.kind), entry.roles.clone()),
        None => {
            // No declaration — under single-user the OS user is the
            // principal; under enforce the daemon will reject.
            (user.to_string(), "user", Vec::new())
        }
    }
}

fn fallback_roles(file: &PrincipalsFile, os_user: Option<&str>) -> Vec<String> {
    os_user.and_then(|u| resolve_principal_for_os_user(file, u)).map(|e| e.roles.clone()).unwrap_or_default()
}
