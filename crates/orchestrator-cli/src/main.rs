use std::sync::Arc;

use anyhow::Result;
use clap::Parser;
use orchestrator_core::config::resolve_project_root;
use orchestrator_core::{FileServiceHub, RuntimeConfig};
use serde::Serialize;

mod cli_types;
mod services;
mod shared;
pub(crate) use cli_types::*;
pub(crate) use shared::*;

#[tokio::main]
async fn main() {
    // Pre-scan argv for `--json` so that clap argparse failures (unknown
    // subcommands, bad flag values) still emit the `animus.cli.v1` error
    // envelope when the caller asked for machine-readable output. `Cli::parse`
    // exits the process directly on parse error, bypassing every downstream
    // `emit_cli_error` call site. We need the flag *before* clap sees it so
    // the failure path can branch on its presence.
    //
    // We scan `args_os()` (not `args()`) so a non-UTF-8 argument such as a
    // path with invalid UTF-8 bytes in `--project-root <bad-path>` doesn't
    // panic before clap can render its own error. The `--json` token itself
    // is pure ASCII, so we only need OsStr → str conversion for the
    // comparison; non-UTF-8 args are silently treated as not being `--json`,
    // which is the correct behavior.
    //
    // The scan stops at `--` so a literal `--json` token passed to a
    // subcommand argument list doesn't accidentally trip the JSON-mode error
    // envelope.
    let argv_requested_json = scan_argv_for_json_flag(std::env::args_os());

    match Cli::try_parse() {
        Ok(cli) => {
            let json = cli.json;
            let startup_check = spawn_startup_update_check(&cli);
            let project_root_override = cli.project_root.clone();
            let run_result = run(cli).await;
            let exit_code = match run_result {
                Ok(()) => 0,
                Err(error) => {
                    emit_cli_error(&error, json);
                    classify_exit_code(&error)
                }
            };
            if let Some(check) = startup_check {
                // Non-blocking modes (Notify / Off) get a tight 50ms grace
                // window so a slow network probe never adds dead-air to the
                // end of the command the user actually asked for.
                //
                // Trade-off documented up-front: `run_startup_check` persists
                // `last_checked = now` BEFORE the GitHub fetch (so a flaky
                // network can't cause us to retry on every CLI invocation).
                // That means a fetch cut off by this 50ms grace still consumes
                // the throttle window, so the user may miss the notification
                // for one `check_interval` period (24h by default). They will
                // still see the update notice on the next interval boundary,
                // and any long-running command (`daemon start`, `daemon run`)
                // gives the check time to complete and surface immediately.
                //
                // Blocking modes (Prompt / Auto) explicitly opted in to wait,
                // so they get no cap.
                let grace = if check.is_blocking { None } else { Some(std::time::Duration::from_millis(50)) };
                match grace {
                    Some(timeout) => {
                        let _ = tokio::time::timeout(timeout, check.handle).await;
                    }
                    None => {
                        let _ = check.handle.await;
                    }
                }
            }
            let runtime_config = RuntimeConfig { project_root: project_root_override, ..RuntimeConfig::default() };
            let (project_root, _) = resolve_project_root(&runtime_config);
            let project_root_path = std::path::PathBuf::from(project_root);
            // 200ms cap on end-of-run metrics flush. The flush is best-effort
            // — if the network is slow the next CLI invocation will retry,
            // so blocking the user's terminal for up to 2s of dead-air at the
            // tail of every command is not worth the marginal delivery
            // improvement.
            let _ = tokio::time::timeout(
                std::time::Duration::from_millis(200),
                services::metrics::maybe_flush_if_due(&project_root_path),
            )
            .await;
            std::process::exit(exit_code);
        }
        Err(parse_err) => {
            // `--help` / `--version` are *successful* clap displays, not
            // parse failures. Keep clap's standard exit-0 behavior even when
            // `--json` is set; converting them into an `invalid_input`
            // envelope would lie about what happened and break operators
            // running `animus --json --help` to discover the surface.
            //
            // The `DisplayHelpOnMissingArgumentOrSubcommand` kind IS a parse
            // failure (clap prints help with exit 2 because no command was
            // chosen), so we still want the JSON envelope path for it.
            if matches!(parse_err.kind(), clap::error::ErrorKind::DisplayHelp | clap::error::ErrorKind::DisplayVersion)
            {
                parse_err.exit();
            }
            if argv_requested_json {
                emit_argparse_error_envelope(&parse_err);
                std::process::exit(2);
            }
            // Non-JSON callers keep clap's pretty-printed help/error output
            // (including ANSI colors and usage hints), matching the pre-fix
            // experience.
            parse_err.exit();
        }
    }
}

/// Detect `--json` (or `--json=...`) anywhere in argv, stopping at `--` so a
/// literal `--json` token passed to a subcommand argument list doesn't
/// accidentally trip the JSON-mode error envelope. Walks `OsString` args so a
/// non-UTF-8 argument (e.g. a path with invalid UTF-8 bytes) doesn't panic;
/// such args simply can't equal the ASCII `--json` literal and are skipped.
/// We don't reach for clap here because clap is the layer that just failed.
fn scan_argv_for_json_flag(argv: impl IntoIterator<Item = std::ffi::OsString>) -> bool {
    for arg in argv.into_iter().skip(1) {
        if arg == "--" {
            return false;
        }
        // OsStr → &str conversion: non-UTF-8 args can't match `--json` so
        // they're correctly treated as "not the JSON flag".
        if let Some(s) = arg.to_str() {
            if s == "--json" || s.starts_with("--json=") {
                return true;
            }
        }
    }
    false
}

/// Build and emit a `animus.cli.v1` error envelope for a clap argparse
/// failure. Clap's rendered output (multi-line, ANSI-colored) is collapsed
/// into a single-line message; the raw rendered text is preserved under
/// `error.details.raw` for callers that want it. `stage = "parse"` flags this
/// as pre-runtime so consumers can distinguish argparse failures from
/// downstream command errors. Exits via the caller; this function only
/// writes to stderr.
fn emit_argparse_error_envelope(err: &clap::Error) {
    let raw = err.render().to_string();
    let collapsed = raw.lines().map(str::trim).filter(|line| !line.is_empty()).collect::<Vec<_>>().join("; ");
    let message = if collapsed.is_empty() { "failed to parse command-line arguments".to_string() } else { collapsed };
    let details = serde_json::json!({
        "stage": "parse",
        "clap_kind": format!("{:?}", err.kind()),
        "raw": raw,
    });
    let parse_error = CliError::new(CliErrorKind::InvalidInput, message).with_details(details);
    let wrapped: anyhow::Error = parse_error.into();
    // `emit_cli_error` honors the kind→exit_code mapping (InvalidInput → 2),
    // which matches clap's historical exit code for argparse failures.
    emit_cli_error(&wrapped, true);
}

async fn run(cli: Cli) -> Result<()> {
    services::operations::set_no_cache_flag(cli.no_cache);
    orchestrator_config::cache::set_no_cache_flag(cli.no_cache);
    orchestrator_core::set_daemon_health_cache_disabled(cli.no_cache);
    if matches!(cli.command, Command::Version) {
        let data = VersionInfo {
            name: env!("CARGO_PKG_NAME"),
            binary: env!("CARGO_BIN_NAME"),
            version: env!("CARGO_PKG_VERSION"),
        };
        return print_value(data, cli.json);
    }

    let runtime_config = RuntimeConfig { project_root: cli.project_root.clone(), ..RuntimeConfig::default() };
    let (project_root, _) = resolve_project_root(&runtime_config);
    // v0.5.8: propagate --as to ControlClient via env var. The CLI
    // warns once before dispatch; the daemon either accepts the
    // honor-system override (rbac=single-user) or rejects via peer-cred
    // (rbac=enforce). Both surface uniformly through the standard error
    // path.
    // Scope the carrier env var to the parsed --as flag so an
    // already-exported ANIMUS_AS_PRINCIPAL from a parent shell cannot
    // silently impersonate. (codex round-5 P2) If the current
    // invocation did not pass --as we unset the env var; if it did, we
    // overwrite with the parsed value.
    match cli.as_principal.as_deref() {
        Some(principal) => {
            std::env::set_var(orchestrator_daemon_runtime::control::ANIMUS_AS_PRINCIPAL_ENV, principal);
            eprintln!(
                "warning: --as is honor-system on the local Unix socket; logged loudly per v0.5.8 RBAC small core"
            );
        }
        None => std::env::remove_var(orchestrator_daemon_runtime::control::ANIMUS_AS_PRINCIPAL_ENV),
    }
    // Record a `cli_invoked` event before dispatch (no-op when telemetry
    // is disabled). The recorder constructor handles every guard
    // internally — kill switch, no consent block, opt-out — so this
    // line never blocks command execution.
    //
    // Skip the recorder for `animus state` commands: the recorder
    // materializes `~/.animus/<scope>/metrics/pending.jsonl` on first
    // call. That would (a) make a fresh target scope look non-empty to
    // `state import` (forcing operators to pass `--yes` for what
    // should be an empty restore) and (b) defeat the export path's
    // read-only "no scoped state — nothing to export" guard by
    // creating a metrics-only marker file. Telemetry for state ops is
    // out-of-scope for v0.5.8.
    if !matches!(cli.command, Command::State { .. }) {
        services::metrics::record_event(
            std::path::Path::new(&project_root),
            services::metrics::EventTags::CliInvoked { command_group: cli_command_group(&cli.command) },
        );
    }

    // v0.5.8 secrets: install both the keychain-backed workflow YAML
    // resolver (so `${VAR}` falls back to keychain entries) AND the
    // plugin-host `SecretSnapshotProvider` (so direct CLI plugin spawns
    // such as `animus plugin ping/call/info`, `animus web`, and the
    // session-backend probe paths see the same secrets a daemon-spawned
    // plugin would). First-installer-wins; safe to call once per CLI
    // invocation. (codex round-2 P2.)
    //
    // The inner `install_*` slots already early-return when populated, but
    // the per-call setup constructs a `KeyringSecretStore` and calls
    // `scoped_state_root` (filesystem + potential `git remote get-url`
    // subprocess) every time. Gate the whole block behind a process-local
    // sentinel so repeat calls within a single CLI process pay nothing.
    static KEYCHAIN_INSTALLED: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    KEYCHAIN_INSTALLED.get_or_init(|| {
        let project_root_for_secrets = std::path::Path::new(&project_root);
        let _ = orchestrator_daemon_runtime::quotas::install_keychain_workflow_resolver_for(project_root_for_secrets);
        let _ = orchestrator_daemon_runtime::quotas::install_keychain_secret_provider_for(project_root_for_secrets);
    });
    match cli.command {
        Command::Init(args) => services::operations::handle_init(args, &project_root, cli.json).await,
        Command::Doctor(args) => services::operations::handle_doctor(&project_root, args, cli.json).await,
        Command::Pack { command } => services::operations::handle_pack(command, &project_root, cli.json).await,
        Command::Plugin { command } => services::operations::handle_plugin(command, &project_root, cli.json).await,
        Command::Status => services::operations::handle_status(&project_root, cli.json).await,
        Command::Daemon { command: DaemonCommand::Status } => {
            services::runtime::handle_daemon_status_command(&project_root, cli.json).await
        }
        Command::Daemon { command: DaemonCommand::Health } => {
            services::runtime::handle_daemon_health_command(&project_root, cli.json).await
        }
        // Telemetry subcommands are global controls (user-global config +
        // every repo scope under `~/.animus/`); dispatch them before the
        // FileServiceHub bootstrap so they never create or migrate project
        // state. Bare `daemon metrics` (display) falls through to the
        // generic daemon path below.
        Command::Daemon { command: DaemonCommand::Metrics(DaemonMetricsCommandArgs { command: Some(sub), .. }) } => {
            services::operations::handle_metrics(sub, &project_root, cli.json).await
        }
        Command::History { command } => services::operations::handle_history(command, &project_root, cli.json).await,
        Command::Approval { command } => services::operations::handle_approval(command, &project_root, cli.json),
        Command::Trigger { command } => services::operations::handle_trigger(command, &project_root, cli.json).await,
        Command::Logs { command } => services::operations::handle_logs(command, &project_root, cli.json).await,
        Command::Subject { command } => services::operations::handle_subject(command, &project_root, cli.json).await,
        Command::Flavor { command } => services::operations::handle_flavor(command, &project_root, cli.json).await,
        Command::SelfCmd { command } => services::operations::handle_self(command, &project_root, cli.json).await,
        Command::Update(args) => services::operations::handle_update(args, &project_root, cli.json).await,
        Command::Cost { command } => services::operations::handle_cost(command, &project_root, cli.json).await,
        Command::Chat { command } => services::runtime::handle_chat(command, &project_root, cli.json).await,
        Command::Auth { command } => {
            services::operations::handle_auth(command, cli.as_principal.clone(), cli.json).await
        }
        Command::Events { command } => services::operations::handle_events(command, &project_root, cli.json).await,
        Command::State { command } => services::operations::handle_state(command, &project_root, cli.json).await,
        Command::Secret { command } => {
            services::operations::handle_secret(command, &project_root, cli.as_principal.clone(), cli.json).await
        }
        command => {
            let hub = Arc::new(FileServiceHub::new(&project_root)?);
            match command {
                Command::Daemon { command } => {
                    services::runtime::handle_daemon(command, hub.clone(), &project_root, cli.json).await
                }
                Command::Agent { command } => {
                    services::runtime::handle_agent(command, hub.clone(), &project_root, cli.json).await
                }
                Command::Project { command } => services::runtime::handle_project(command, hub.clone(), cli.json).await,
                Command::Queue { command } => {
                    services::operations::handle_queue(command, hub.clone(), &project_root, cli.json).await
                }
                Command::Workflow { command } => {
                    services::operations::handle_workflow(command, hub.clone(), &project_root, cli.json).await
                }
                Command::History { .. } => unreachable!("command handled before hub creation"),
                Command::Git { command } => services::operations::handle_git(command, &project_root, cli.json).await,
                Command::Skill { command } => {
                    services::operations::handle_skill(command, &project_root, cli.json).await
                }
                Command::Pack { .. } => unreachable!("handled before hub creation"),
                Command::Plugin { .. } => unreachable!("handled before hub creation"),
                Command::Output { command } => {
                    services::operations::handle_output(command, &project_root, cli.json).await
                }
                Command::Mcp { command } => services::operations::handle_mcp(command, &project_root, cli.json).await,
                Command::Web { command } => {
                    services::operations::handle_web(command, hub.clone(), &project_root, cli.json).await
                }
                Command::Status | Command::Version => {
                    unreachable!("command handled before runtime initialization")
                }
                Command::Init(_)
                | Command::Approval { .. }
                | Command::Doctor(_)
                | Command::Trigger { .. }
                | Command::Logs { .. }
                | Command::Subject { .. }
                | Command::Flavor { .. }
                | Command::SelfCmd { .. }
                | Command::Update(_)
                | Command::Cost { .. }
                | Command::Chat { .. }
                | Command::Auth { .. }
                | Command::Events { .. }
                | Command::State { .. }
                | Command::Secret { .. } => {
                    unreachable!("command handled before hub initialization")
                }
            }
        }
    }
}

struct StartupCheck {
    handle: tokio::task::JoinHandle<()>,
    is_blocking: bool,
}

fn spawn_startup_update_check(cli: &Cli) -> Option<StartupCheck> {
    if cli.json {
        return None;
    }
    if matches!(cli.command, Command::SelfCmd { .. } | Command::Update(_) | Command::Version) {
        return None;
    }
    let runtime_config = RuntimeConfig { project_root: cli.project_root.clone(), ..RuntimeConfig::default() };
    let (project_root, _) = resolve_project_root(&runtime_config);
    let config_block = services::self_update::resolve_effective_config_block(&project_root);
    let mode = services::self_update::effective_mode(config_block.as_ref());
    if matches!(mode, protocol::AutoUpdateMode::Off) {
        return None;
    }
    let is_blocking = matches!(mode, protocol::AutoUpdateMode::Auto | protocol::AutoUpdateMode::Prompt);
    let state = services::self_update::AutoUpdateState::load();
    let current_version = env!("CARGO_PKG_VERSION").to_string();

    let handle = tokio::spawn(async move {
        match services::self_update::run_startup_check(config_block, current_version, state).await {
            Ok(Some(message)) => {
                eprintln!("{message}");
            }
            Ok(None) => {}
            Err(_) => {
                // Network errors during a fire-and-forget check are silently
                // suppressed — the subcommand the user actually asked for is
                // what matters. Operators can run `animus self update
                // --check-only` to surface failures.
            }
        }
    });
    Some(StartupCheck { handle, is_blocking })
}

#[derive(Debug, Serialize)]
struct VersionInfo {
    name: &'static str,
    binary: &'static str,
    version: &'static str,
}

fn cli_command_group(command: &Command) -> services::metrics::CommandGroup {
    use services::metrics::CommandGroup;
    match command {
        Command::Version => CommandGroup::Version,
        Command::Daemon { .. } => CommandGroup::Daemon,
        Command::Agent { .. } => CommandGroup::Agent,
        Command::Chat { .. } => CommandGroup::Chat,
        Command::Project { .. } => CommandGroup::Project,
        Command::Queue { .. } => CommandGroup::Queue,
        Command::Workflow { .. } => CommandGroup::Workflow,
        Command::History { .. } => CommandGroup::History,
        Command::Git { .. } => CommandGroup::Git,
        Command::Approval { .. } => CommandGroup::Approval,
        Command::Skill { .. } => CommandGroup::Skill,
        Command::Pack { .. } => CommandGroup::Pack,
        Command::Plugin { .. } => CommandGroup::Plugin,
        Command::Status => CommandGroup::Status,
        Command::Output { .. } => CommandGroup::Output,
        Command::Mcp { .. } => CommandGroup::Mcp,
        Command::Web { .. } => CommandGroup::Web,
        Command::Init(_) => CommandGroup::Init,
        Command::Doctor(_) => CommandGroup::Doctor,
        Command::Trigger { .. } => CommandGroup::Trigger,
        Command::Logs { .. } => CommandGroup::Logs,
        Command::Subject { .. } => CommandGroup::Subject,
        Command::Flavor { .. } => CommandGroup::Flavor,
        Command::SelfCmd { .. } => CommandGroup::SelfUpdate,
        Command::Update(_) => CommandGroup::SelfUpdate,
        Command::Cost { .. } => CommandGroup::Cost,
        Command::Auth { .. } => CommandGroup::Auth,
        Command::Events { .. } => CommandGroup::Events,
        Command::State { .. } => CommandGroup::State,
        Command::Secret { .. } => CommandGroup::Secret,
    }
}

#[cfg(test)]
mod argv_scan_tests {
    use super::scan_argv_for_json_flag;
    use std::ffi::OsString;

    fn os(args: &[&str]) -> Vec<OsString> {
        args.iter().map(|s| OsString::from(*s)).collect()
    }

    #[test]
    fn scan_finds_bare_json_flag() {
        assert!(scan_argv_for_json_flag(os(&["animus", "--json", "status"])));
    }

    #[test]
    fn scan_finds_json_with_value_form() {
        // Even though clap rejects `--json=true` (it's a bool flag), the
        // scanner must still detect the intent so the error envelope fires.
        assert!(scan_argv_for_json_flag(os(&["animus", "--json=true", "status"])));
    }

    #[test]
    fn scan_returns_false_when_no_json_flag_present() {
        assert!(!scan_argv_for_json_flag(os(&["animus", "status", "--verbose"])));
    }

    #[test]
    fn scan_stops_at_double_dash_separator() {
        // `--json` after `--` belongs to the subcommand's argv, not the
        // animus CLI. Don't trip the JSON-mode envelope on it.
        assert!(!scan_argv_for_json_flag(os(&["animus", "agent", "run", "--", "--json"])));
    }

    #[test]
    fn scan_treats_non_utf8_args_as_non_json() {
        // OsString with invalid UTF-8 bytes must not panic the scanner.
        // Construct a non-UTF-8 OsString without unsafe by going through
        // a path with invalid bytes on Unix.
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStringExt;
            let bad = OsString::from_vec(vec![0x66, 0x6f, 0x80, 0x6f]); // "fo\x80o"
            let argv = vec![OsString::from("animus"), bad, OsString::from("status")];
            assert!(!scan_argv_for_json_flag(argv), "non-UTF-8 arg must be skipped, not panic");
        }
        #[cfg(not(unix))]
        {
            // Other platforms construct OsString from u16 sequences; the
            // contract under test is the same: a non-matching OsString must
            // not trip the JSON detector.
            assert!(!scan_argv_for_json_flag(os(&["animus", "status"])));
        }
    }
}
