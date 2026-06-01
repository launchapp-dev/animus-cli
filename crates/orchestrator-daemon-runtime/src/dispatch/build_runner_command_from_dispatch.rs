use std::path::PathBuf;

use protocol::{McpRuntimeConfig, PhaseRoutingConfig, SubjectDispatch};
use tracing::warn;

pub fn build_runner_command_from_dispatch(dispatch: &SubjectDispatch, project_root: &str) -> std::process::Command {
    build_runner_command(dispatch, project_root, None, None)
}

pub fn build_runner_command(
    dispatch: &SubjectDispatch,
    project_root: &str,
    phase_routing: Option<&PhaseRoutingConfig>,
    mcp_config: Option<&McpRuntimeConfig>,
) -> std::process::Command {
    let mut cmd = std::process::Command::new(resolve_workflow_runner_binary_for(Some(project_root)));
    cmd.arg("execute");

    match dispatch.subject.to_workflow_subject() {
        protocol::orchestrator::WorkflowSubject::Task { id } => {
            cmd.arg("--task-id").arg(id);
        }
        protocol::orchestrator::WorkflowSubject::Requirement { id } => {
            cmd.arg("--requirement-id").arg(id);
        }
        protocol::orchestrator::WorkflowSubject::Custom { title, description } => {
            cmd.arg("--title").arg(title);
            cmd.arg("--description").arg(description);
        }
    }

    if let Some(input) = &dispatch.input {
        cmd.arg("--input-json").arg(input.to_string());
    }

    cmd.arg("--workflow-ref").arg(&dispatch.workflow_ref).arg("--project-root").arg(project_root);

    if let Some(routing) = phase_routing {
        if let Ok(json) = serde_json::to_string(routing) {
            cmd.arg("--phase-routing-json").arg(json);
        }
    }
    if let Some(mcp) = mcp_config {
        if let Ok(json) = serde_json::to_string(mcp) {
            cmd.arg("--mcp-config-json").arg(json);
        }
    }
    cmd
}

#[cfg(test)]
fn resolve_workflow_runner_binary() -> PathBuf {
    resolve_workflow_runner_binary_for(None)
}

fn resolve_workflow_runner_binary_for(project_root: Option<&str>) -> PathBuf {
    if let Ok(path) = std::env::var("ANIMUS_WORKFLOW_RUNNER_BIN") {
        let trimmed = path.trim();
        if !trimmed.is_empty() {
            return PathBuf::from(trimmed);
        }
    }

    // v0.5.1 round-4 fold-in: the in-tree `animus-workflow-runner` binary
    // was removed. The replacement is the plugin-installed binary
    // `animus-workflow-runner-default` from
    // `launchapp-dev/animus-workflow-runner-default` v0.3.0+. Resolution
    // order per candidate name:
    //   1. sibling of `current_exe` (and `current_exe/../` so cargo-test
    //      `target/debug/deps/` runners can find a release-mode sibling),
    //   2. project-local `.animus/plugins/` (when a project root is
    //      supplied — `animus plugin install --plugin-dir <project>/.animus/plugins`),
    //   3. `~/.animus/plugins.yaml` registry (the explicit-binary path
    //      recorded by `animus plugin install --plugin-dir <custom>`),
    //   4. `~/.animus/plugins/` (the canonical plugin-install dir; honors
    //      `$ANIMUS_CONFIG_DIR` and `$ANIMUS_PLUGIN_DIR`),
    //   5. `$PATH` lookup.
    // Candidate names, in precedence order:
    //   1. `animus-workflow-runner-default` (the plugin's binary name).
    //   2. `animus-workflow-runner` (the legacy in-tree binary name — kept
    //      as a back-compat fallback for installs that still ship it).
    //   3. `ao-workflow-runner` (the v0.4.x legacy name, retained for the
    //      same reason).
    for name in
        [plugin_workflow_runner_binary_name(), workflow_runner_binary_name(), legacy_workflow_runner_binary_name()]
    {
        if let Some(found) = find_workflow_runner_binary_with_project(name, project_root) {
            if name == legacy_workflow_runner_binary_name() {
                warn!(
                    "resolved legacy {name} binary; install the v0.5 workflow_runner plugin with `animus plugin install launchapp-dev/animus-workflow-runner-default` to upgrade"
                );
            } else if name == workflow_runner_binary_name() {
                warn!(
                    "resolved in-tree {name} binary; v0.5.1+ uses the plugin binary — install with `animus plugin install launchapp-dev/animus-workflow-runner-default`"
                );
            }
            return found;
        }
    }

    PathBuf::from(plugin_workflow_runner_binary_name())
}

fn plugin_workflow_runner_binary_name() -> &'static str {
    #[cfg(target_os = "windows")]
    {
        "animus-workflow-runner-default.exe"
    }
    #[cfg(not(target_os = "windows"))]
    {
        "animus-workflow-runner-default"
    }
}

fn find_workflow_runner_binary_with_project(binary_name: &str, project_root: Option<&str>) -> Option<PathBuf> {
    if let Ok(exe_path) = std::env::current_exe() {
        if let Some(exe_dir) = exe_path.parent() {
            let sibling = exe_dir.join(binary_name);
            if sibling.exists() {
                return Some(sibling);
            }

            if exe_dir.file_name().is_some_and(|name| name == "deps") {
                if let Some(parent) = exe_dir.parent() {
                    let parent_sibling = parent.join(binary_name);
                    if parent_sibling.exists() {
                        return Some(parent_sibling);
                    }
                }
            }
        }
    }

    // Project-local plugin install dir — `animus plugin install
    // --plugin-dir <project>/.animus/plugins ...`. PluginDiscovery walks
    // this before the global dir, so the resolver must mirror that
    // precedence; otherwise the daemon advertises the project-local
    // workflow_runner via preflight then fails to spawn it. Codex P2
    // round-4.
    if let Some(project_root) = project_root {
        let project_plugins = std::path::Path::new(project_root).join(".animus").join("plugins").join(binary_name);
        if project_plugins.is_file() && is_executable(&project_plugins) {
            return Some(project_plugins);
        }
    }

    // Explicit-registry lookup. `animus plugin install` records the
    // installed binary path in `~/.animus/plugins.yaml` (or the
    // `$ANIMUS_CONFIG_DIR`-scoped variant). An operator who installs the
    // plugin into a non-default directory still ends up registered there,
    // so honoring this registry is the closest thing to "the same path the
    // host plugin-discovery uses" without taking a crate dep on
    // `orchestrator_plugin_host`. Codex P2 round-4.
    if let Some(registry_path) = find_registered_plugin_binary(binary_name) {
        if registry_path.is_file() && is_executable(&registry_path) {
            return Some(registry_path);
        }
    }

    // Plugin-install dir lookup — the canonical destination for
    // `animus plugin install launchapp-dev/animus-workflow-runner-default`.
    // Mirrors `orchestrator_plugin_host::discovery::plugin_install_dir`
    // resolution (honors `$ANIMUS_PLUGIN_DIR` and `$ANIMUS_CONFIG_DIR`)
    // without taking a crate dep on the host (this resolver runs deep in
    // the daemon-runtime crate; pulling in plugin-host would invert the
    // current build order).
    if let Some(install_dir) = plugin_install_dir_lookup() {
        let plugin_path = install_dir.join(binary_name);
        if plugin_path.is_file() && is_executable(&plugin_path) {
            return Some(plugin_path);
        }
    }

    // `$ANIMUS_PLUGIN_PATH` (colon-separated additional plugin directories
    // — PluginDiscovery scans this just after the global dir). Without
    // this branch a plugin supplied only via PLUGIN_PATH would pass
    // preflight but the scheduler would fail to spawn it. Codex P2
    // round-4.
    if let Ok(plugin_path) = std::env::var("ANIMUS_PLUGIN_PATH") {
        for dir in plugin_path.split(':') {
            let dir = dir.trim();
            if dir.is_empty() {
                continue;
            }
            let candidate = std::path::Path::new(dir).join(binary_name);
            if candidate.is_file() && is_executable(&candidate) {
                return Some(candidate);
            }
        }
    }

    // Fall back to PATH lookup. Handles the case where the daemon binary
    // was installed via `cargo install` / a different prefix than the
    // workflow runner (eg the daemon lives in `~/.cargo/bin/animus` but the
    // runner lives in `~/.local/bin/ao-workflow-runner` per a v0.4.x
    // installer run). Without this branch a PATH-only legacy install
    // disappears from the resolver as soon as the daemon binary moves.
    find_on_path(binary_name)
}

/// Look up `binary_name` in the canonical `plugins.yaml` registry. Returns
/// the registered binary path when found. Best-effort parse — registry
/// shape variation (eg pre-v0.4.12 keys) is tolerated and falls through
/// to the directory-scan path.
fn find_registered_plugin_binary(binary_name: &str) -> Option<PathBuf> {
    use std::io::Read;

    let registry_path = plugin_registry_path()?;
    let mut file = std::fs::File::open(&registry_path).ok()?;
    let mut contents = String::new();
    file.read_to_string(&mut contents).ok()?;

    // The yaml shape is:
    //   plugins:
    //     <logical_name>:
    //       binary: <absolute path>
    //       name: <on-disk binary name, optional>
    // We do not want to take a `serde_yaml` dep just for this resolver
    // (the only daemon-runtime YAML dep today is in workflow config). A
    // line-oriented scan is sufficient: pair every `binary:` line with the
    // first subsequent `name:` line and accept a match either by basename
    // or by `name:` exact match.
    let mut pending_binary: Option<String> = None;
    for raw_line in contents.lines() {
        let line = raw_line.trim();
        if let Some(rest) = line.strip_prefix("binary:") {
            let value = rest.trim().trim_matches('"').trim_matches('\'').to_string();
            if !value.is_empty() {
                let path = PathBuf::from(expand_home(&value));
                if path.file_name().and_then(|n| n.to_str()).map(|n| n == binary_name).unwrap_or(false) {
                    return Some(path);
                }
                pending_binary = Some(value);
            }
        } else if let Some(rest) = line.strip_prefix("name:") {
            let value = rest.trim().trim_matches('"').trim_matches('\'');
            if value == binary_name {
                if let Some(binary) = pending_binary.take() {
                    return Some(PathBuf::from(expand_home(&binary)));
                }
            }
        }
    }
    None
}

fn plugin_registry_path() -> Option<PathBuf> {
    let home = if let Ok(value) = std::env::var("ANIMUS_CONFIG_DIR") {
        let trimmed = value.trim();
        if !trimmed.is_empty() {
            PathBuf::from(trimmed)
        } else {
            PathBuf::from(std::env::var_os("HOME")?).join(".animus")
        }
    } else {
        PathBuf::from(std::env::var_os("HOME")?).join(".animus")
    };
    Some(home.join("plugins.yaml"))
}

fn expand_home(path: &str) -> String {
    if let Some(rest) = path.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            let mut out = PathBuf::from(home);
            out.push(rest);
            return out.to_string_lossy().into_owned();
        }
    }
    path.to_string()
}

fn plugin_install_dir_lookup() -> Option<PathBuf> {
    if let Ok(value) = std::env::var("ANIMUS_PLUGIN_DIR") {
        let trimmed = value.trim();
        if !trimmed.is_empty() {
            return Some(PathBuf::from(trimmed));
        }
    }
    let home = if let Ok(value) = std::env::var("ANIMUS_CONFIG_DIR") {
        let trimmed = value.trim();
        if !trimmed.is_empty() {
            PathBuf::from(trimmed)
        } else {
            PathBuf::from(std::env::var_os("HOME")?).join(".animus")
        }
    } else {
        PathBuf::from(std::env::var_os("HOME")?).join(".animus")
    };
    Some(home.join("plugins"))
}

fn find_on_path(binary_name: &str) -> Option<PathBuf> {
    let path_var = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_var) {
        if dir.as_os_str().is_empty() {
            continue;
        }
        let candidate = dir.join(binary_name);
        if !candidate.is_file() {
            continue;
        }
        if !is_executable(&candidate) {
            continue;
        }
        return Some(candidate);
    }
    None
}

#[cfg(unix)]
fn is_executable(path: &std::path::Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).map(|m| m.permissions().mode() & 0o111 != 0).unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable(_path: &std::path::Path) -> bool {
    // On Windows the PATH lookup uses extension-based resolution which the
    // OS handles at spawn time. Returning true preserves existing
    // behavior; callers fall through to `Command::spawn` and let the OS
    // surface any non-executability.
    true
}

fn workflow_runner_binary_name() -> &'static str {
    #[cfg(target_os = "windows")]
    {
        "animus-workflow-runner.exe"
    }

    #[cfg(not(target_os = "windows"))]
    {
        "animus-workflow-runner"
    }
}

fn legacy_workflow_runner_binary_name() -> &'static str {
    #[cfg(target_os = "windows")]
    {
        "ao-workflow-runner.exe"
    }

    #[cfg(not(target_os = "windows"))]
    {
        "ao-workflow-runner"
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    #[cfg(unix)]
    use std::sync::Mutex;

    use protocol::orchestrator::WorkflowSubject;
    use protocol::{SubjectDispatch, SubjectDispatchExt};
    use serde_json::json;

    use super::build_runner_command_from_dispatch;

    #[test]
    fn runner_command_uses_subject_workflow_ref_and_input_from_dispatch() {
        let dispatch = SubjectDispatch::for_custom(
            "schedule:nightly",
            "nightly dispatch",
            "ops",
            Some(json!({"nightly":true})),
            "schedule",
        );
        let command = build_runner_command_from_dispatch(&dispatch, "/tmp/project");
        let program = command.get_program().to_string_lossy().into_owned();
        let args = command.get_args().map(|arg| arg.to_string_lossy().into_owned()).collect::<Vec<_>>();

        // The resolved binary is one of:
        //   * `animus-workflow-runner-default` (the v0.5 plugin binary —
        //     preferred),
        //   * `animus-workflow-runner` (legacy in-tree, only when the
        //     plugin is not installed and the legacy binary is on PATH),
        //   * `ao-workflow-runner` (v0.4.x legacy fallback).
        // In CI / dev environments where none of the three exist, the
        // resolver returns a bare relative name (no parent path).
        let file_name = Path::new(&program).file_name().and_then(|name| name.to_str()).unwrap_or("");
        assert!(
            file_name == super::plugin_workflow_runner_binary_name()
                || file_name == super::workflow_runner_binary_name()
                || file_name == super::legacy_workflow_runner_binary_name(),
            "unexpected workflow runner binary: {file_name}"
        );
        assert_eq!(
            args,
            vec![
                "execute",
                "--title",
                "schedule:nightly",
                "--description",
                "nightly dispatch",
                "--input-json",
                "{\"nightly\":true}",
                "--workflow-ref",
                "ops",
                "--project-root",
                "/tmp/project",
            ]
        );
        assert_eq!(
            &dispatch.subject.to_workflow_subject(),
            &WorkflowSubject::Custom {
                title: "schedule:nightly".to_string(),
                description: "nightly dispatch".to_string(),
            }
        );
    }

    #[test]
    fn plugin_workflow_runner_binary_name_uses_default_suffix() {
        // As of v0.5.1 round-4 fold-in the resolver prefers the
        // out-of-tree plugin binary
        // `animus-workflow-runner-default`. The two legacy names are kept
        // as fallbacks for installs that have not migrated yet.
        let primary = super::plugin_workflow_runner_binary_name();
        assert!(
            primary.starts_with("animus-workflow-runner-default"),
            "primary name should be the plugin binary: {primary}"
        );

        let in_tree = super::workflow_runner_binary_name();
        assert!(
            in_tree.starts_with("animus-workflow-runner"),
            "in-tree fallback name should start with animus-: {in_tree}"
        );

        let legacy = super::legacy_workflow_runner_binary_name();
        assert!(legacy.starts_with("ao-workflow-runner"), "legacy name should retain the ao- prefix: {legacy}");
    }

    // Shared lock for tests that mutate `ANIMUS_WORKFLOW_RUNNER_BIN` or
    // `PATH`. Routed through the dispatch-wide shared lock so we also
    // serialize against `process_manager`'s env-mutating tests.
    #[cfg(unix)]
    fn env_lock() -> &'static Mutex<()> {
        crate::dispatch::test_env::lock()
    }

    #[cfg(unix)]
    struct EnvVarGuard {
        key: &'static str,
        original: Option<String>,
    }

    #[cfg(unix)]
    impl EnvVarGuard {
        fn set(key: &'static str, value: Option<&str>) -> Self {
            let original = std::env::var(key).ok();
            match value {
                Some(v) => std::env::set_var(key, v),
                None => std::env::remove_var(key),
            }
            Self { key, original }
        }
    }

    #[cfg(unix)]
    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            match self.original.as_deref() {
                Some(v) => std::env::set_var(self.key, v),
                None => std::env::remove_var(self.key),
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn resolve_picks_new_binary_when_both_exist_alongside_current_exe() {
        // Back-compat regression: when an operator upgrades the daemon
        // binary but still has the legacy `ao-workflow-runner` on the side
        // (because the installer also drops a back-compat symlink), the
        // resolver MUST select `animus-workflow-runner` first. Otherwise
        // we silently keep invoking the legacy name and the rename is a
        // no-op.
        use std::os::unix::fs::PermissionsExt;

        let _lock = env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let _clear = EnvVarGuard::set("ANIMUS_WORKFLOW_RUNNER_BIN", None);
        // Scope ANIMUS_PLUGIN_DIR to an empty tempdir so a real installed
        // `animus-workflow-runner-default` in the dev environment does not
        // pre-empt the precedence test we're running here.
        let empty_plugin_dir = tempfile::tempdir().expect("tempdir");
        let _plugin =
            EnvVarGuard::set("ANIMUS_PLUGIN_DIR", Some(empty_plugin_dir.path().to_str().expect("utf-8 tempdir")));

        // Resolve `current_exe`'s parent dir; that's where the resolver
        // looks for siblings. We can't move `current_exe`, but we CAN
        // create siblings next to it. Cargo test binaries live under
        // `target/debug/deps/` so the resolver also probes the parent
        // (`target/debug/`).
        let exe = std::env::current_exe().expect("current_exe");
        let exe_dir = exe.parent().expect("exe parent");
        // Pick the `deps` parent (target/debug) so the siblings don't
        // collide with the real release-built runner under
        // `target/debug/animus-workflow-runner` on developer machines.
        let bin_dir = if exe_dir.file_name().is_some_and(|n| n == "deps") {
            exe_dir.parent().expect("target/debug").to_path_buf()
        } else {
            exe_dir.to_path_buf()
        };

        // If a real runner already sits there (cargo build -p
        // workflow-runner-v2 was run), don't clobber it. Skip the test
        // instead — the assertion below would either be redundant or
        // wedge on a stale `ao-workflow-runner` artifact.
        let new_path = bin_dir.join(super::workflow_runner_binary_name());
        let legacy_path = bin_dir.join(super::legacy_workflow_runner_binary_name());
        if new_path.exists() || legacy_path.exists() {
            return;
        }

        let payload = b"#!/bin/sh\nexit 0\n";
        std::fs::write(&new_path, payload).expect("write new runner");
        std::fs::write(&legacy_path, payload).expect("write legacy runner");
        let mut perms = std::fs::metadata(&new_path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&new_path, perms.clone()).unwrap();
        std::fs::set_permissions(&legacy_path, perms).unwrap();

        let resolved = super::resolve_workflow_runner_binary();

        // Tidy up before asserting so a failed assertion still cleans the
        // sibling files we wrote into `target/debug/`.
        let _ = std::fs::remove_file(&new_path);
        let _ = std::fs::remove_file(&legacy_path);

        assert_eq!(
            resolved.file_name().and_then(|n| n.to_str()),
            Some(super::workflow_runner_binary_name()),
            "resolver must pick the new animus-workflow-runner when both names exist; got {}",
            resolved.display()
        );
    }

    #[cfg(unix)]
    #[test]
    fn resolve_falls_back_to_legacy_when_new_missing() {
        // Back-compat: a freshly upgraded daemon binary running against a
        // user environment that still only has `ao-workflow-runner` (no
        // re-run of the installer yet) must keep dispatching. The
        // resolver's job is to find the legacy binary and emit a warn
        // line via `tracing::warn!`. We can't assert the warn easily
        // here without wiring a tracing subscriber, but we can prove the
        // path is selected.
        use std::os::unix::fs::PermissionsExt;

        let _lock = env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let _clear = EnvVarGuard::set("ANIMUS_WORKFLOW_RUNNER_BIN", None);
        // Clear PATH so the resolver's PATH-fallback can't surface a real
        // `animus-workflow-runner` from a developer's environment and pre-empt
        // the legacy-sibling path we're trying to exercise here.
        let _path = EnvVarGuard::set("PATH", Some(""));
        let empty_plugin_dir = tempfile::tempdir().expect("tempdir");
        let _plugin =
            EnvVarGuard::set("ANIMUS_PLUGIN_DIR", Some(empty_plugin_dir.path().to_str().expect("utf-8 tempdir")));

        let exe = std::env::current_exe().expect("current_exe");
        let exe_dir = exe.parent().expect("exe parent");
        let bin_dir = if exe_dir.file_name().is_some_and(|n| n == "deps") {
            exe_dir.parent().expect("target/debug").to_path_buf()
        } else {
            exe_dir.to_path_buf()
        };

        let new_path = bin_dir.join(super::workflow_runner_binary_name());
        let legacy_path = bin_dir.join(super::legacy_workflow_runner_binary_name());
        // Bail out if the env already has a real runner — we don't want to
        // mutate cargo build artifacts.
        if new_path.exists() || legacy_path.exists() {
            return;
        }

        let payload = b"#!/bin/sh\nexit 0\n";
        std::fs::write(&legacy_path, payload).expect("write legacy runner");
        let mut perms = std::fs::metadata(&legacy_path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&legacy_path, perms).unwrap();

        let resolved = super::resolve_workflow_runner_binary();

        let _ = std::fs::remove_file(&legacy_path);

        assert_eq!(
            resolved.file_name().and_then(|n| n.to_str()),
            Some(super::legacy_workflow_runner_binary_name()),
            "resolver must fall back to ao-workflow-runner when animus-workflow-runner is missing; got {}",
            resolved.display()
        );
    }

    #[cfg(unix)]
    #[test]
    fn resolve_falls_back_to_legacy_on_path_when_no_sibling() {
        // PATH-only fallback: covers the case where the daemon binary lives
        // in a different prefix than the runner (eg `cargo install animus`
        // drops the daemon in `~/.cargo/bin/` while a prior v0.4.x installer
        // left `ao-workflow-runner` in `~/.local/bin/`). Without PATH
        // fallback the resolver would emit a literal "animus-workflow-runner"
        // path and spawn would fail. With it we surface the legacy binary
        // and the warn nudge, so the daemon keeps dispatching.
        use std::os::unix::fs::PermissionsExt;

        let _lock = env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let _clear = EnvVarGuard::set("ANIMUS_WORKFLOW_RUNNER_BIN", None);
        let empty_plugin_dir = tempfile::tempdir().expect("tempdir");
        let _plugin =
            EnvVarGuard::set("ANIMUS_PLUGIN_DIR", Some(empty_plugin_dir.path().to_str().expect("utf-8 tempdir")));

        // Make sure no sibling lookup succeeds: pick a scratch PATH dir
        // that's far from `current_exe`.
        let temp = tempfile::tempdir().expect("tempdir");
        let legacy_path = temp.path().join(super::legacy_workflow_runner_binary_name());
        std::fs::write(&legacy_path, b"#!/bin/sh\nexit 0\n").expect("write legacy on PATH");
        let mut perms = std::fs::metadata(&legacy_path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&legacy_path, perms).unwrap();

        // Scope PATH down to the scratch dir so we don't accidentally pick
        // up a real `animus-workflow-runner` from the developer's
        // environment.
        let _path = EnvVarGuard::set("PATH", Some(temp.path().to_str().expect("utf-8 tempdir")));

        // If the sibling probe still finds a real animus-workflow-runner
        // built into `target/debug/`, skip the test — that artifact predates
        // the test and we can't isolate around it without removing it.
        let exe = std::env::current_exe().expect("current_exe");
        let exe_dir = exe.parent().expect("exe parent");
        let bin_dir = if exe_dir.file_name().is_some_and(|n| n == "deps") {
            exe_dir.parent().expect("target/debug").to_path_buf()
        } else {
            exe_dir.to_path_buf()
        };
        // Skip if EITHER sibling exists in `target/debug/` — a stale
        // pre-rename build artifact (`ao-workflow-runner`) would also
        // pre-empt the PATH probe and wedge the assertion below.
        if bin_dir.join(super::workflow_runner_binary_name()).exists()
            || bin_dir.join(super::legacy_workflow_runner_binary_name()).exists()
        {
            return;
        }

        let resolved = super::resolve_workflow_runner_binary();
        assert_eq!(
            resolved.file_name().and_then(|n| n.to_str()),
            Some(super::legacy_workflow_runner_binary_name()),
            "resolver must fall back to ao-workflow-runner on PATH when no sibling exists; got {}",
            resolved.display()
        );
        // And the path must be the one we wrote, not just a name.
        assert_eq!(resolved, legacy_path, "resolved path must point at the PATH-discovered legacy binary");
    }

    #[cfg(unix)]
    #[test]
    fn resolve_prefers_plugin_binary_in_plugin_install_dir() {
        // v0.5.1 round-4 fold-in: when the plugin binary
        // `animus-workflow-runner-default` is installed under
        // `$ANIMUS_PLUGIN_DIR` it must win over any legacy in-tree
        // `animus-workflow-runner` discoverable via PATH. Otherwise the
        // daemon silently keeps invoking the deleted in-tree binary.
        use std::os::unix::fs::PermissionsExt;

        let _lock = env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let _clear = EnvVarGuard::set("ANIMUS_WORKFLOW_RUNNER_BIN", None);

        let plugin_dir = tempfile::tempdir().expect("tempdir");
        let plugin_path = plugin_dir.path().join(super::plugin_workflow_runner_binary_name());
        std::fs::write(&plugin_path, b"#!/bin/sh\nexit 0\n").expect("write plugin runner");
        let mut perms = std::fs::metadata(&plugin_path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&plugin_path, perms).unwrap();

        let _plugin = EnvVarGuard::set("ANIMUS_PLUGIN_DIR", Some(plugin_dir.path().to_str().expect("utf-8 tempdir")));
        // Also surface a PATH-discoverable legacy binary so the test
        // proves the plugin dir wins over PATH.
        let path_dir = tempfile::tempdir().expect("tempdir");
        let legacy_path = path_dir.path().join(super::workflow_runner_binary_name());
        std::fs::write(&legacy_path, b"#!/bin/sh\nexit 0\n").expect("write legacy on PATH");
        let mut perms = std::fs::metadata(&legacy_path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&legacy_path, perms).unwrap();
        let _path = EnvVarGuard::set("PATH", Some(path_dir.path().to_str().expect("utf-8 tempdir")));

        // Skip if a sibling binary by either legacy name pre-empts the
        // plugin-dir lookup (developer machines that built workflow-runner-v2
        // historically have leftover artifacts).
        let exe = std::env::current_exe().expect("current_exe");
        let exe_dir = exe.parent().expect("exe parent");
        let bin_dir = if exe_dir.file_name().is_some_and(|n| n == "deps") {
            exe_dir.parent().expect("target/debug").to_path_buf()
        } else {
            exe_dir.to_path_buf()
        };
        if bin_dir.join(super::plugin_workflow_runner_binary_name()).exists()
            || bin_dir.join(super::workflow_runner_binary_name()).exists()
            || bin_dir.join(super::legacy_workflow_runner_binary_name()).exists()
        {
            return;
        }

        let resolved = super::resolve_workflow_runner_binary();
        assert_eq!(resolved, plugin_path, "resolver must select the plugin binary from $ANIMUS_PLUGIN_DIR");
    }
}
