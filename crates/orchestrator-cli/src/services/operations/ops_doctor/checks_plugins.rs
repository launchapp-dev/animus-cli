//! Plugin installation, binary presence, and per-binary executability checks.
//!
//! These checks run against the same discovery surface used by
//! `animus plugin info` so they reflect what the daemon would actually see at
//! startup.

use std::path::{Path, PathBuf};
use std::process::Command;

use orchestrator_core::plugin_preflight::{summarize_discovered_plugins, PluginPreflightSpec, RequiredRole};
use orchestrator_plugin_host::discover_plugins;

use super::check_kit::{CheckContext, CheckFix, CheckStatus, DiagnosticCheck};

const CATEGORY: &str = "plugins";

pub(crate) fn run(ctx: &CheckContext) -> Vec<DiagnosticCheck> {
    let mut out = Vec::new();

    let (discovered, discovery_err) = match discover_plugins(ctx.project_root.clone()) {
        Ok(list) => (list, None),
        Err(error) => (Vec::new(), Some(error.to_string())),
    };

    if let Some(err) = discovery_err {
        out.push(
            DiagnosticCheck::new("plugin_discovery", CATEGORY, CheckStatus::Fail, "Plugin discovery")
                .details(format!("plugin discovery failed: {err}"))
                .fix(CheckFix::command(
                    "reinstall_defaults",
                    "Re-run the default plugin install to recover from a corrupt registry.",
                    "animus plugin install-defaults",
                )),
        );
        return out;
    }

    let summaries = summarize_discovered_plugins(&discovered);

    // Required-role coverage check (mirrors `daemon preflight` posture so doctor
    // surfaces the same gating logic the daemon uses).
    let spec = PluginPreflightSpec::daemon_default();
    let mut missing_roles = Vec::new();
    for role in &spec.required_roles {
        let label = role.label();
        let satisfied = match role {
            RequiredRole::AtLeastOneProvider => summaries.iter().any(|s| s.is_provider()),
            RequiredRole::SubjectKind(kind) => summaries.iter().any(|s| s.covers_subject_kind(kind)),
            RequiredRole::TransportEnabled => true,
            RequiredRole::WorkflowRunner => summaries.iter().any(|s| s.is_workflow_runner()),
            RequiredRole::Queue => summaries.iter().any(|s| s.is_queue()),
        };
        if !satisfied {
            missing_roles.push(label);
        }
    }
    if missing_roles.is_empty() {
        out.push(
            DiagnosticCheck::new(
                "plugin_required_roles",
                CATEGORY,
                CheckStatus::Pass,
                "Required plugin roles installed",
            )
            .details(format!("{} role(s) satisfied", spec.required_roles.len())),
        );
    } else {
        out.push(
            DiagnosticCheck::new(
                "plugin_required_roles",
                CATEGORY,
                CheckStatus::Fail,
                "Required plugin roles installed",
            )
            .current(format!("missing: {}", missing_roles.join(", ")))
            .expected(format!("{} role(s) all satisfied", spec.required_roles.len()))
            .fix(CheckFix::command(
                "install_defaults",
                "Install the curated default plugin set so every required role is covered.",
                "animus plugin install-defaults",
            )),
        );
    }

    // Per-plugin binary checks.
    for plugin in &discovered {
        let binary_path = plugin.path.clone();
        let id_path = sanitize_id(&plugin.name);

        if !binary_path.exists() {
            out.push(
                DiagnosticCheck::new(
                    format!("plugin_binary_present.{id_path}"),
                    CATEGORY,
                    CheckStatus::Fail,
                    format!("Plugin binary present: {}", plugin.name),
                )
                .current(format!("missing at {}", binary_path.display()))
                .expected("executable file on disk".to_string())
                .fix(CheckFix::command(
                    "reinstall_plugin",
                    &format!("Reinstall {} to restore the missing binary.", plugin.name),
                    &format!("animus plugin install {}@latest", plugin.name),
                )),
            );
            continue;
        }

        if !is_executable(&binary_path) {
            out.push(
                DiagnosticCheck::new(
                    format!("plugin_binary_executable.{id_path}"),
                    CATEGORY,
                    CheckStatus::Fail,
                    format!("Plugin binary executable: {}", plugin.name),
                )
                .current(format!("not executable at {}", binary_path.display()))
                .expected("executable bit set (0o111)".to_string())
                .fix(CheckFix::auto(
                    "chmod_plugin_binary",
                    &format!("Set the executable bit on {}.", binary_path.display()),
                    &format!("chmod +x {}", binary_path.display()),
                )),
            );
            continue;
        }

        out.push(
            DiagnosticCheck::new(
                format!("plugin_binary_present.{id_path}"),
                CATEGORY,
                CheckStatus::Pass,
                format!("Plugin binary present: {}", plugin.name),
            )
            .details(format!(
                "{} v{} ({}) at {}",
                plugin.manifest.name,
                plugin.manifest.version,
                plugin.manifest.plugin_kind,
                binary_path.display()
            )),
        );

        if !ctx.skip_subprocess {
            let probe = probe_plugin_manifest(&binary_path);
            let mut check = DiagnosticCheck::new(
                format!("plugin_manifest_probe.{id_path}"),
                CATEGORY,
                if probe.is_ok() { CheckStatus::Pass } else { CheckStatus::Warn },
                format!("Plugin --manifest probe: {}", plugin.name),
            );
            match probe {
                Ok(()) => check = check.details("plugin responded to --manifest within 5s".to_string()),
                Err(reason) => {
                    check = check.current(reason).expected("--manifest returns valid JSON within 5s".to_string()).fix(
                        CheckFix::command(
                            "reinstall_plugin",
                            &format!("Reinstall {} — the binary failed its manifest probe.", plugin.name),
                            &format!("animus plugin install {}@latest", plugin.name),
                        ),
                    );
                }
            }
            out.push(check);
        }
    }

    if discovered.is_empty() && missing_roles.is_empty() {
        // No plugins, but also no roles required (custom spec) — still surface
        // a hint so operators don't think the doctor is broken.
        out.push(
            DiagnosticCheck::new("plugin_inventory", CATEGORY, CheckStatus::Warn, "Plugin inventory")
                .details("no plugins discovered; daemon will refuse to start without provider + subject backends")
                .fix(CheckFix::command(
                    "install_defaults",
                    "Install the curated default plugin set.",
                    "animus plugin install-defaults",
                )),
        );
    }

    out
}

fn sanitize_id(name: &str) -> String {
    name.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).collect()
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;

    std::fs::metadata(path).map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0).unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable(path: &Path) -> bool {
    path.is_file()
}

fn probe_plugin_manifest(path: &PathBuf) -> Result<(), String> {
    use std::time::{Duration, Instant};

    let start = Instant::now();
    let child = Command::new(path)
        .arg("--manifest")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn();
    let mut child = match child {
        Ok(c) => c,
        Err(e) => return Err(format!("failed to spawn: {e}")),
    };
    // Bounded wait (5s) without bringing in a runtime: poll try_wait().
    let deadline = start + Duration::from_secs(5);
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => return Ok(()),
            Ok(Some(status)) => return Err(format!("--manifest exited with {status}")),
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    return Err("--manifest exceeded 5s timeout".to_string());
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => return Err(format!("wait failed: {e}")),
        }
    }
}
