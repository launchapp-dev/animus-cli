//! Offline, template-driven scaffold for trigger backend plugins.
//!
//! Unlike [`super::new`] (which clones `launchapp-dev/animus-plugin-template`
//! over the network), this module emits a self-contained starter Cargo
//! project from string templates baked into the binary. Useful as the
//! recommended starting point for v0.5.5 trigger authors and as a
//! deterministic fixture for the scaffold test suite.
//!
//! The emitted project:
//!
//! - Names the binary `animus-trigger-<name>` (kebab → snake conversion is
//!   handled for `src/main.rs` identifiers).
//! - Depends on `animus-plugin-protocol` + `animus-plugin-runtime` from
//!   `launchapp-dev/animus-protocol` at the requested tag.
//! - Implements the `trigger_backend` plugin contract end-to-end:
//!   `initialize`, `$/ping`, `health/check`, `trigger/watch` (with
//!   acknowledgement + a background notification loop), and `trigger/ack`.
//! - Ships a `plugin.toml` mirror of the runtime manifest so static tooling
//!   (`animus plugin info` against an uninstalled tree) can discover it.
//! - Comes with a README walking through build, install, workflow YAML
//!   wiring, and debugging.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Serialize;

use crate::{invalid_input_error, print_value, PluginScaffoldTriggerArgs};

#[cfg(test)]
#[path = "scaffold_tests.rs"]
mod scaffold_tests;

/// Files (relative to the scaffold output root) `handle_plugin_scaffold_trigger`
/// always writes.
const SCAFFOLD_FILES: &[&str] = &["Cargo.toml", "plugin.toml", "src/main.rs", "README.md", ".gitignore"];

#[derive(Debug, Serialize)]
pub(super) struct PluginScaffoldOutput {
    pub(super) name: String,
    pub(super) full_name: String,
    pub(super) out_dir: String,
    pub(super) owner: String,
    pub(super) license: String,
    pub(super) protocol_tag: String,
    pub(super) files_written: Vec<String>,
    pub(super) next_steps: Vec<String>,
}

pub(super) fn handle_plugin_scaffold_trigger(args: PluginScaffoldTriggerArgs) -> Result<()> {
    let json = args.json;
    let name = args.name.trim().to_string();
    if !is_valid_kebab_name(&name) {
        return Err(invalid_input_error(format!(
            "trigger name '{}' must be kebab-case (lowercase letters, digits, hyphens; start with a letter, end alphanumerically)",
            name
        )));
    }

    let owner = resolve_owner(args.owner.as_deref());
    let license = args.license.trim().to_string();
    if license.is_empty() {
        return Err(invalid_input_error("--license must not be empty"));
    }
    reject_quotes_or_newlines("--license", &license)?;
    reject_quotes_or_newlines("--owner", &owner)?;

    let full_name = format!("animus-trigger-{}", name);
    let description =
        args.description.clone().unwrap_or_else(|| format!("Custom Animus trigger backend plugin ({})", name));
    reject_quotes_or_newlines("--description", &description)?;
    reject_quotes_or_newlines("--protocol-tag", &args.protocol_tag)?;

    let out_dir = args.out_dir.clone().unwrap_or_else(|| PathBuf::from(format!("./{}", full_name)));
    prepare_out_dir(&out_dir, args.force)?;

    let vars = build_vars(&name, &full_name, &owner, &license, &description, &args.protocol_tag);

    write_template(&out_dir.join("Cargo.toml"), CARGO_TOML_TEMPLATE, &vars)?;
    write_template(&out_dir.join("plugin.toml"), PLUGIN_TOML_TEMPLATE, &vars)?;
    std::fs::create_dir_all(out_dir.join("src"))
        .with_context(|| format!("failed to create src/ inside {}", out_dir.display()))?;
    write_template(&out_dir.join("src/main.rs"), MAIN_RS_TEMPLATE, &vars)?;
    write_template(&out_dir.join("README.md"), README_TEMPLATE, &vars)?;
    write_template(&out_dir.join(".gitignore"), GITIGNORE_TEMPLATE, &vars)?;

    let files_written: Vec<String> = SCAFFOLD_FILES.iter().map(|rel| out_dir.join(rel).display().to_string()).collect();

    let next_steps = vec![
        format!("cd {}", out_dir.display()),
        "cargo build --release".to_string(),
        format!("animus plugin install --path target/release/{}", full_name),
        "animus daemon preflight".to_string(),
    ];

    let output = PluginScaffoldOutput {
        name,
        full_name,
        out_dir: out_dir.display().to_string(),
        owner,
        license,
        protocol_tag: args.protocol_tag.clone(),
        files_written,
        next_steps: next_steps.clone(),
    };

    if !json {
        eprintln!("Scaffolded trigger plugin at {}", output.out_dir);
        eprintln!("Files:");
        for file in &output.files_written {
            eprintln!("  - {}", file);
        }
        eprintln!("Next steps:");
        for step in &next_steps {
            eprintln!("  - {}", step);
        }
    }

    print_value(output, json)
}

fn resolve_owner(explicit: Option<&str>) -> String {
    if let Some(value) = explicit {
        let trimmed = value.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }
    std::env::var("USER").ok().filter(|value| !value.trim().is_empty()).unwrap_or_else(|| "launchapp-dev".to_string())
}

fn prepare_out_dir(out_dir: &Path, force: bool) -> Result<()> {
    if out_dir.exists() {
        let is_empty = std::fs::read_dir(out_dir).map(|mut entries| entries.next().is_none()).unwrap_or(false);
        if is_empty {
            return Ok(());
        }
        if !force {
            return Err(invalid_input_error(format!(
                "output directory already exists: {} (pass --force to overwrite)",
                out_dir.display()
            )));
        }
        if !looks_like_prior_scaffold(out_dir) {
            return Err(invalid_input_error(format!(
                "refusing to overwrite {} with --force: directory is non-empty and does not look like a previously scaffolded \
                 trigger plugin (expected at minimum a Cargo.toml whose [package].name starts with \"animus-trigger-\"). \
                 Move it aside or pass --out-dir to an empty/new path.",
                out_dir.display()
            )));
        }
        for rel in SCAFFOLD_FILES {
            let candidate = out_dir.join(rel);
            if !candidate.exists() {
                continue;
            }
            if candidate.is_dir() {
                std::fs::remove_dir_all(&candidate)
                    .with_context(|| format!("failed to overwrite scaffold dir {}", candidate.display()))?;
            } else {
                std::fs::remove_file(&candidate)
                    .with_context(|| format!("failed to overwrite scaffold file {}", candidate.display()))?;
            }
        }
        return Ok(());
    }
    std::fs::create_dir_all(out_dir)
        .with_context(|| format!("failed to create output directory {}", out_dir.display()))?;
    Ok(())
}

/// Returns `true` only when `dir` looks like a previously-emitted trigger
/// scaffold: it contains a `Cargo.toml` whose `[package].name` starts with
/// `animus-trigger-`. Used to gate `--force` so the scaffolder cannot
/// recursively delete an unrelated project tree.
fn looks_like_prior_scaffold(dir: &Path) -> bool {
    let Ok(cargo) = std::fs::read_to_string(dir.join("Cargo.toml")) else {
        return false;
    };
    for line in cargo.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("name") {
            let rest = rest.trim_start();
            if let Some(rest) = rest.strip_prefix('=') {
                let value = rest.trim().trim_matches('"');
                if value.starts_with("animus-trigger-") {
                    return true;
                }
            }
        }
    }
    false
}

fn build_vars(
    name: &str,
    full_name: &str,
    owner: &str,
    license: &str,
    description: &str,
    protocol_tag: &str,
) -> BTreeMap<String, String> {
    let mut vars = BTreeMap::new();
    vars.insert("name".to_string(), name.to_string());
    vars.insert("full_name".to_string(), full_name.to_string());
    vars.insert("name_snake".to_string(), to_snake(name));
    vars.insert("NAME_PASCAL".to_string(), to_pascal(name));
    vars.insert("NAME_UPPER".to_string(), to_upper_snake(name));
    vars.insert("owner".to_string(), owner.to_string());
    vars.insert("license".to_string(), license.to_string());
    vars.insert("description".to_string(), description.to_string());
    vars.insert("protocol_tag".to_string(), protocol_tag.to_string());
    vars
}

fn reject_quotes_or_newlines(flag: &str, value: &str) -> Result<()> {
    if value.chars().any(|c| matches!(c, '"' | '\\' | '\n' | '\r')) {
        return Err(invalid_input_error(format!(
            "{flag} value must not contain quotes, backslashes, or newlines (got: {:?})",
            value
        )));
    }
    Ok(())
}

fn is_valid_kebab_name(name: &str) -> bool {
    if name.len() < 2 {
        return false;
    }
    let bytes = name.as_bytes();
    let first_ok = bytes[0].is_ascii_lowercase();
    let last = bytes[bytes.len() - 1];
    let last_ok = last.is_ascii_lowercase() || last.is_ascii_digit();
    if !first_ok || !last_ok {
        return false;
    }
    name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

fn to_snake(name: &str) -> String {
    name.replace('-', "_").to_ascii_lowercase()
}

fn to_upper_snake(name: &str) -> String {
    name.replace('-', "_").to_ascii_uppercase()
}

fn to_pascal(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut upper_next = true;
    for ch in name.chars() {
        if ch == '-' || ch == '_' {
            upper_next = true;
            continue;
        }
        if upper_next {
            for upper in ch.to_uppercase() {
                out.push(upper);
            }
            upper_next = false;
        } else {
            out.push(ch);
        }
    }
    out
}

fn write_template(path: &Path, template: &str, vars: &BTreeMap<String, String>) -> Result<()> {
    let rendered = substitute(template, vars);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create parent directory {}", parent.display()))?;
    }
    std::fs::write(path, rendered).with_context(|| format!("failed to write {}", path.display()))?;
    Ok(())
}

pub(super) fn substitute(template: &str, vars: &BTreeMap<String, String>) -> String {
    let mut out = String::with_capacity(template.len());
    let bytes = template.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if i + 1 < bytes.len() && bytes[i] == b'{' && bytes[i + 1] == b'{' {
            if let Some(close) = find_close(&bytes[i + 2..]) {
                let raw = &template[i + 2..i + 2 + close];
                let key = raw.trim();
                if let Some(value) = vars.get(key) {
                    out.push_str(value);
                    i = i + 2 + close + 2;
                    continue;
                }
            }
        }
        let ch = template[i..].chars().next().expect("non-empty");
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

fn find_close(slice: &[u8]) -> Option<usize> {
    let mut j = 0;
    while j + 1 < slice.len() {
        if slice[j] == b'}' && slice[j + 1] == b'}' {
            return Some(j);
        }
        j += 1;
    }
    None
}

// =====================================================================
// Templates
//
// `{{var}}` markers are substituted at scaffold time. Anything else is
// emitted verbatim — including `{{` sequences that aren't followed by a
// known key, which are left as-is. The Rust `format!`-style `{}` placeholders
// inside generated code are NOT touched.
// =====================================================================

const CARGO_TOML_TEMPLATE: &str = r#"[workspace]

[package]
name = "{{full_name}}"
version = "0.1.0"
edition = "2021"
license = "{{license}}"
description = "{{description}}"
repository = "https://github.com/{{owner}}/{{full_name}}"

[[bin]]
name = "{{full_name}}"
path = "src/main.rs"

[dependencies]
animus-plugin-protocol = { git = "https://github.com/launchapp-dev/animus-protocol", tag = "{{protocol_tag}}" }
animus-plugin-runtime  = { git = "https://github.com/launchapp-dev/animus-protocol", tag = "{{protocol_tag}}" }
anyhow = "1"
async-trait = "0.1"
chrono = { version = "0.4", features = ["serde"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
tokio = { version = "1", features = ["rt-multi-thread", "macros", "io-std", "io-util", "sync", "time"] }
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }
"#;

const PLUGIN_TOML_TEMPLATE: &str = r#"# Static plugin metadata for {{full_name}}.
#
# Animus's primary discovery surface is the executable's `--manifest`
# JSON output. This file mirrors that data so static tooling
# (`animus plugin info`, CI lint, the public plugin registry) can read
# the manifest without spawning the binary.

name        = "{{full_name}}"
version     = "0.1.0"
plugin_kind = "trigger_backend"
description = "{{description}}"
binary      = "{{full_name}}"
protocol    = "stdio"

# The plugin host clears the daemon's environment before spawn and only
# forwards a minimal shell allowlist (PATH, HOME, TMPDIR, LANG, LC_ALL,
# RUST_LOG, RUST_BACKTRACE, TZ) plus anything declared here. List the
# env vars the trigger needs to read at spawn time.
env_required = []
"#;

const MAIN_RS_TEMPLATE: &str = r##"//! {{full_name}} — Animus trigger backend plugin.
//!
//! Generated by `animus plugin scaffold trigger {{name}}`. Edit the
//! `WATCH_INTERVAL` constant and the body of [`spawn_event_loop`] to
//! emit real events; the supplied implementation emits a heartbeat every
//! few seconds so you can wire the plugin end-to-end before plumbing real
//! upstream state.

use std::sync::Arc;
use std::time::Duration;

use animus_plugin_protocol::{
    HealthCheckResult, HealthStatus, TriggerAckParams, TriggerActionHint, TriggerEvent,
    TriggerWatchParams, PLUGIN_KIND_TRIGGER_BACKEND, TRIGGER_METHOD_ACK, TRIGGER_METHOD_EVENT,
    TRIGGER_METHOD_WATCH,
};
use animus_plugin_runtime::{Notifier, Plugin};
use anyhow::Result;
use chrono::Utc;
use serde_json::json;
use tokio::sync::Mutex;

const PLUGIN_NAME: &str = "{{full_name}}";
const PLUGIN_VERSION: &str = env!("CARGO_PKG_VERSION");
const PLUGIN_DESCRIPTION: &str = "{{description}}";

const WATCH_INTERVAL: Duration = Duration::from_secs(15);

#[derive(Default)]
struct {{NAME_PASCAL}}State {
    delivered: Vec<String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .init();

    let state: Arc<Mutex<{{NAME_PASCAL}}State>> = Arc::new(Mutex::new({{NAME_PASCAL}}State::default()));
    let watch_state = state.clone();
    let ack_state = state.clone();

    Plugin::new(PLUGIN_NAME, PLUGIN_VERSION, PLUGIN_KIND_TRIGGER_BACKEND)
        .description(PLUGIN_DESCRIPTION)
        .methods([
            TRIGGER_METHOD_WATCH.to_string(),
            TRIGGER_METHOD_ACK.to_string(),
            "health/check".to_string(),
        ])
        .register_method::<TriggerWatchParams, serde_json::Value, _, _>(
            TRIGGER_METHOD_WATCH,
            move |params, ctx| {
                let state = watch_state.clone();
                async move {
                    ctx.keep_cancellation();
                    let notifier = ctx.notifier.clone();
                    let cancellation = ctx.cancellation.clone();
                    spawn_event_loop(state, notifier, cancellation, params);
                    Ok(json!({"watching": true}))
                }
            },
        )
        .register_notification(TRIGGER_METHOD_ACK, move |params, _notifier| {
            let state = ack_state.clone();
            async move {
                if let Ok(parsed) = serde_json::from_value::<TriggerAckParams>(params) {
                    let mut guard = state.lock().await;
                    guard.delivered.retain(|id| id != &parsed.event_id);
                }
            }
        })
        .register_method::<serde_json::Value, HealthCheckResult, _, _>(
            "health/check",
            |_, _| async move {
                Ok(HealthCheckResult {
                    status: HealthStatus::Healthy,
                    uptime_ms: None,
                    memory_usage_bytes: None,
                    last_error: None,
                })
            },
        )
        .run()
        .await
}

fn spawn_event_loop(
    state: Arc<Mutex<{{NAME_PASCAL}}State>>,
    notifier: Notifier,
    cancellation: animus_plugin_runtime::CancellationToken,
    params: TriggerWatchParams,
) {
    tokio::spawn(async move {
        let trigger_id = resolve_trigger_id(params.config.as_ref());

        let mut tick: u64 = 0;
        loop {
            tokio::select! {
                _ = cancellation.cancelled() => break,
                _ = tokio::time::sleep(WATCH_INTERVAL) => {}
            }
            tick += 1;
            let event = TriggerEvent {
                event_id: format!("{{name_snake}}-{}", tick),
                trigger_id: trigger_id.clone(),
                subject_id: None,
                subject_kind: None,
                action_hint: Some(TriggerActionHint::RunWorkflow),
                payload: json!({
                    "occurred_at": Utc::now().to_rfc3339(),
                    "tick": tick,
                }),
            };
            {
                let mut guard = state.lock().await;
                guard.delivered.push(event.event_id.clone());
            }
            notifier.notify_typed(TRIGGER_METHOD_EVENT, &event).await;
        }
    });
}

fn resolve_trigger_id(config: Option<&serde_json::Value>) -> Option<String> {
    let value = config?;
    if let Some(direct) = value.get("trigger_id").and_then(|v| v.as_str()) {
        return Some(direct.to_string());
    }
    if let Some(entries) = value.get("triggers").and_then(|v| v.as_array()) {
        for entry in entries {
            if let Some(id) = entry.get("id").and_then(|v| v.as_str()) {
                return Some(id.to_string());
            }
        }
    }
    None
}
"##;

const README_TEMPLATE: &str = r##"# {{full_name}}

{{description}}

This is an [Animus](https://github.com/launchapp-dev/animus-cli) trigger
backend plugin generated by `animus plugin scaffold trigger {{name}}`.
The daemon spawns the binary as a long-lived child process, completes the
JSON-RPC handshake, sends `trigger/watch`, and consumes the `trigger/event`
notifications the binary emits.

## Layout

- `src/main.rs` — the runtime entrypoint and event-loop. Replace the
  default heartbeat with your real upstream listener (Slack socket, HTTP
  endpoint, inotify watcher, cron tick, ...).
- `plugin.toml` — static metadata mirroring the `--manifest` payload.
- `Cargo.toml` — depends on `animus-plugin-protocol` and
  `animus-plugin-runtime` from `launchapp-dev/animus-protocol` at
  `{{protocol_tag}}`.

## Build + install

```bash
cargo build --release
animus plugin install --path target/release/{{full_name}}
```

Verify the daemon can discover it:

```bash
animus plugin list
animus plugin info --name {{full_name}}
animus plugin ping --name {{full_name}}
```

## Wire into a workflow

Add a `triggers:` entry of `type: plugin` to your `.animus/workflows.yaml`.
The daemon forwards every enabled `type: plugin` entry to the plugin
in `TriggerWatchParams.config.triggers[]`; this scaffold picks the
first entry's `id` and uses it as `trigger_id` on every emitted event:

```yaml
workflows:
  - id: respond-to-{{name}}
    phases: [{name: respond, tool: claude}]

triggers:
  - id: {{name}}-default
    type: plugin
    workflow_ref: respond-to-{{name}}
    config:
      trigger_id: {{name}}-default
```

Restart the daemon to pick up the workflow file change:

```bash
animus daemon stop
animus daemon start --autonomous
```

## Debugging

- Plugin stderr is forwarded to the daemon process log: `~/.animus/<repo-scope>/daemon/daemon.log`.
- Structured supervisor/runtime events are mirrored separately at `~/.animus/<repo-scope>/logs/events.jsonl`.
- Set `RUST_LOG=info` (or `debug`) in the daemon's environment to surface
  the plugin's tracing output.
- Temporarily disable trigger supervision with
  `ANIMUS_DAEMON_DISABLE_TRIGGERS=1 animus daemon start` to confirm the
  plugin is the only thing emitting events.
- Bypass the daemon while iterating:

  ```bash
  echo '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocol_version":"0.1","host_info":{"name":"manual","version":"0.0.0"},"capabilities":{}}}' | \
      ./target/release/{{full_name}}
  ```

## Protocol surface

`{{full_name}}` implements the [`trigger_backend`](https://github.com/launchapp-dev/animus-protocol/blob/{{protocol_tag}}/animus-plugin-protocol/src/lib.rs)
plugin kind:

| Method | Direction | Purpose |
|---|---|---|
| `initialize` | host → plugin | Handshake; declares capabilities |
| `trigger/watch` | host → plugin | Opens the event stream |
| `trigger/event` | plugin → host | Notification carrying a `TriggerEvent` |
| `trigger/ack` | host → plugin | Confirms a delivered event id |
| `health/check` | host → plugin | Periodic health poll |
| `shutdown` / `exit` | host → plugin | Clean teardown on daemon stop |

See `docs/guides/authoring-trigger-plugins.md` in the Animus repo for the
full walkthrough.
"##;

const GITIGNORE_TEMPLATE: &str = "target/\nCargo.lock\n";
