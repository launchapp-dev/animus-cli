//! `animus-hook` — harness hook spine for provider CLI sessions.
//!
//! Wired into provider harness hook configs (e.g. claude `--settings` hook
//! entries injected via the runtime-contract launch args) so every hook
//! event flows through Animus. Per-provider activation wiring lives
//! out-of-tree at the plugin surface; this binary is the provider-agnostic
//! kernel side: it records every event to the scoped runtime state and, for
//! the gate events (`PreToolUse` / `PermissionRequest`) when `--policy` is
//! given, synchronously evaluates the compiled hook policy
//! (`protocol::hook_policy`) against `{tool_name, tool_input}` and prints
//! the harness hook decision JSON on stdout.
//!
//! Contract notes (verified against claude CLI v2.1.x hook docs):
//! - PreToolUse decisions: `hookSpecificOutput.permissionDecision`
//!   (`allow` / `deny` / `ask`) + `permissionDecisionReason`. A policy
//!   `defer` verdict means abstain — print nothing and let the harness's
//!   normal permission flow decide (it does NOT suspend the call).
//! - PermissionRequest decisions: `hookSpecificOutput.decision.behavior`
//!   (`allow` requires `updatedInput`; `deny` carries `message`). `ask`
//!   maps to abstain here — no output lets the normal dialog proceed.
//! - The binary always exits 0; decisions travel exclusively via stdout
//!   JSON. Exit code 2 would hard-block with stderr as the reason, which is
//!   never what the spine wants for observability events.
//! - Fail-closed on policy errors: when `--policy` was explicitly passed
//!   but the file cannot be loaded, gate events get a `deny` with a
//!   diagnostic reason. A guardrail that silently disarms is worse than a
//!   visible, recoverable denial.
//!
//! Ships in the `orchestrator-cli` package (alongside `animus` and
//! `animus-mcp-proxy`) so the standard build + release path emits it next
//! to `animus`.

use std::io::Read;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use clap::{Parser, Subcommand};
use protocol::hook_policy::{HookPolicy, PolicyDecision, PolicyVerdict};
use serde_json::{json, Value};

/// Upper bound on the hook payload read from stdin (1 MiB beyond claude's
/// own payload sizes; protects the spine from a runaway writer).
const MAX_STDIN_BYTES: u64 = 10 * 1024 * 1024;

#[derive(Debug, Parser)]
#[command(
    name = "animus-hook",
    about = "Animus harness hook spine: record hook events and evaluate guardrail policy",
    version
)]
struct Args {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Record a harness hook event (payload on stdin) and, for gate events
    /// with `--policy`, print the hook decision JSON on stdout.
    Emit {
        /// Hook event name (e.g. PreToolUse, PostToolUse, PermissionRequest,
        /// Stop, SessionStart, SessionEnd). Unknown event names are accepted
        /// and recorded as-is so a provider wiring a newer hook event never
        /// hard-fails the session — only PreToolUse / PermissionRequest gate.
        #[arg(long)]
        event: String,
        /// Animus session / agent identifier this harness session belongs to.
        #[arg(long)]
        session: String,
        /// Project root used to resolve the scoped runtime state.
        #[arg(long)]
        project_root: PathBuf,
        /// Compiled hook policy file (`hook-policy.v1.json`). Only consulted
        /// for gate events (PreToolUse / PermissionRequest).
        #[arg(long)]
        policy: Option<PathBuf>,
    },
}

const PRE_TOOL_USE: &str = "PreToolUse";
const PERMISSION_REQUEST: &str = "PermissionRequest";

/// Events that gate a tool call and accept a permission decision. Every
/// other event name — known or unknown — is record-only.
fn is_gate(event: &str) -> bool {
    event == PRE_TOOL_USE || event == PERMISSION_REQUEST
}

fn main() {
    let Args { command } = Args::parse();
    let Command::Emit { event, session, project_root, policy } = command;

    // The spine must never break the harness session: every failure path
    // degrades to "log to stderr, keep going" and the process exits 0.
    let payload = read_stdin_payload();

    let decision = if is_gate(&event) {
        policy.as_deref().map(|policy_path| gate_decision(&event, policy_path, &payload))
    } else {
        None
    };

    record_event(&project_root, &event, &session, &payload, decision.as_ref());

    if let Some(decision) = decision {
        if let Some(output) = decision_output(&event, &decision, &payload) {
            let mut stdout = std::io::stdout().lock();
            let _ = writeln!(stdout, "{output}");
        }
    }
}

/// Read the hook payload from stdin. Non-JSON input is preserved as a raw
/// string so the event log never drops data.
fn read_stdin_payload() -> Value {
    let mut raw = String::new();
    if let Err(err) = std::io::stdin().lock().take(MAX_STDIN_BYTES).read_to_string(&mut raw) {
        eprintln!("animus-hook: failed to read stdin payload: {err}");
        return Value::Null;
    }
    if raw.trim().is_empty() {
        return Value::Null;
    }
    serde_json::from_str(&raw).unwrap_or(Value::String(raw))
}

/// Evaluate the compiled policy for a gate event. Policy load failures are
/// fail-closed: the guardrail was explicitly requested, so a missing or
/// corrupt policy file produces a visible deny instead of silently
/// disarming.
fn gate_decision(event: &str, policy_path: &Path, payload: &Value) -> PolicyVerdict {
    let policy = match HookPolicy::load(policy_path) {
        Ok(policy) => policy,
        Err(err) => {
            eprintln!("animus-hook: {err}");
            return PolicyVerdict {
                decision: PolicyDecision::Deny,
                reason: Some(format!("animus hook policy could not be loaded ({err}); denying as a safety default")),
                rule_id: None,
            };
        }
    };
    let tool_name = payload.get("tool_name").and_then(Value::as_str).unwrap_or_default();
    let tool_input = payload.get("tool_input").cloned().unwrap_or(Value::Null);
    policy.evaluate(event, tool_name, &tool_input)
}

/// Map a policy verdict onto the harness hook decision JSON for the given
/// gate event. `None` means abstain (print nothing; the harness falls
/// through to its normal permission flow).
fn decision_output(event: &str, verdict: &PolicyVerdict, payload: &Value) -> Option<String> {
    let output = match event {
        PRE_TOOL_USE => {
            // `defer` means abstain. Claude only accepts an explicit defer
            // in non-interactive mode, while printing nothing abstains in
            // every mode — so abstention is expressed by silence.
            if verdict.decision == PolicyDecision::Defer {
                return None;
            }
            json!({
                "hookSpecificOutput": {
                    "hookEventName": "PreToolUse",
                    "permissionDecision": verdict.decision.as_str(),
                    "permissionDecisionReason": verdict.reason.clone().unwrap_or_default(),
                }
            })
        }
        PERMISSION_REQUEST => {
            // PermissionRequest only supports allow / deny; `ask` and
            // `defer` both abstain so the normal permission dialog runs.
            let decision = match verdict.decision {
                PolicyDecision::Deny => json!({
                    "behavior": "deny",
                    "message": verdict.reason.clone().unwrap_or_default(),
                }),
                PolicyDecision::Allow => json!({
                    "behavior": "allow",
                    "updatedInput": payload.get("tool_input").cloned().unwrap_or(Value::Null),
                }),
                PolicyDecision::Ask | PolicyDecision::Defer => return None,
            };
            json!({
                "hookSpecificOutput": {
                    "hookEventName": "PermissionRequest",
                    "decision": decision,
                }
            })
        }
        _ => return None,
    };
    Some(output.to_string())
}

/// Append the event (and any decision) to the scoped hook event log:
/// `~/.animus/<repo-scope>/hooks/events.jsonl`. Best-effort — a logging
/// failure must never affect the harness session.
fn record_event(project_root: &Path, event: &str, session: &str, payload: &Value, decision: Option<&PolicyVerdict>) {
    let Some(scope_root) = protocol::repository_scope::scoped_state_root(project_root) else {
        eprintln!("animus-hook: could not resolve scoped state root; skipping event record");
        return;
    };
    let hooks_dir = scope_root.join("hooks");
    if let Err(err) = std::fs::create_dir_all(&hooks_dir) {
        eprintln!("animus-hook: failed to create {}: {err}", hooks_dir.display());
        return;
    }
    let record = event_record(event, session, project_root, payload, decision);
    let path = hooks_dir.join("events.jsonl");
    let result = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .and_then(|mut file| writeln!(file, "{record}"));
    if let Err(err) = result {
        eprintln!("animus-hook: failed to append to {}: {err}", path.display());
    }
}

fn event_record(
    event: &str,
    session: &str,
    project_root: &Path,
    payload: &Value,
    decision: Option<&PolicyVerdict>,
) -> Value {
    let mut record = json!({
        "ts": chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        "event": event,
        "session": session,
        "project_root": project_root.display().to_string(),
        "payload": payload,
    });
    if let (Some(decision), Some(map)) = (decision, record.as_object_mut()) {
        map.insert("decision".to_string(), serde_json::to_value(decision).unwrap_or(Value::Null));
    }
    record
}

#[cfg(test)]
mod tests {
    use super::*;

    fn verdict(decision: PolicyDecision, reason: Option<&str>) -> PolicyVerdict {
        PolicyVerdict { decision, reason: reason.map(str::to_string), rule_id: None }
    }

    #[test]
    fn pre_tool_use_deny_shape() {
        let out =
            decision_output(PRE_TOOL_USE, &verdict(PolicyDecision::Deny, Some("blocked")), &serde_json::json!({}))
                .expect("deny emits a decision");
        let parsed: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(parsed["hookSpecificOutput"]["hookEventName"], "PreToolUse");
        assert_eq!(parsed["hookSpecificOutput"]["permissionDecision"], "deny");
        assert_eq!(parsed["hookSpecificOutput"]["permissionDecisionReason"], "blocked");
    }

    #[test]
    fn pre_tool_use_ask_and_allow_emit_decisions() {
        for decision in [PolicyDecision::Ask, PolicyDecision::Allow] {
            let out = decision_output(PRE_TOOL_USE, &verdict(decision, Some("r")), &serde_json::json!({}))
                .expect("non-defer emits a decision");
            let parsed: Value = serde_json::from_str(&out).unwrap();
            assert_eq!(parsed["hookSpecificOutput"]["permissionDecision"], decision.as_str());
        }
    }

    #[test]
    fn pre_tool_use_defer_abstains() {
        assert!(decision_output(PRE_TOOL_USE, &verdict(PolicyDecision::Defer, None), &Value::Null).is_none());
    }

    #[test]
    fn permission_request_deny_shape() {
        let out = decision_output(
            PERMISSION_REQUEST,
            &verdict(PolicyDecision::Deny, Some("no")),
            &serde_json::json!({"tool_input": {"command": "rm"}}),
        )
        .expect("deny emits a decision");
        let parsed: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(parsed["hookSpecificOutput"]["hookEventName"], "PermissionRequest");
        assert_eq!(parsed["hookSpecificOutput"]["decision"]["behavior"], "deny");
        assert_eq!(parsed["hookSpecificOutput"]["decision"]["message"], "no");
    }

    #[test]
    fn permission_request_allow_echoes_tool_input() {
        let payload = serde_json::json!({"tool_input": {"command": "ls"}});
        let out = decision_output(PERMISSION_REQUEST, &verdict(PolicyDecision::Allow, None), &payload)
            .expect("allow emits a decision");
        let parsed: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(parsed["hookSpecificOutput"]["decision"]["behavior"], "allow");
        assert_eq!(parsed["hookSpecificOutput"]["decision"]["updatedInput"]["command"], "ls");
    }

    #[test]
    fn permission_request_ask_and_defer_abstain() {
        for decision in [PolicyDecision::Ask, PolicyDecision::Defer] {
            assert!(decision_output(PERMISSION_REQUEST, &verdict(decision, None), &Value::Null).is_none());
        }
    }

    #[test]
    fn non_gate_events_never_emit_decisions() {
        for event in ["PostToolUse", "Stop", "SessionStart", "SessionEnd"] {
            assert!(!is_gate(event));
            assert!(decision_output(event, &verdict(PolicyDecision::Deny, Some("x")), &Value::Null).is_none());
        }
    }

    #[test]
    fn unreadable_policy_fails_closed() {
        let verdict = gate_decision(
            PRE_TOOL_USE,
            Path::new("/nonexistent/animus-hook-policy.json"),
            &serde_json::json!({"tool_name": "Bash", "tool_input": {"command": "ls"}}),
        );
        assert_eq!(verdict.decision, PolicyDecision::Deny);
        assert!(verdict.reason.as_deref().unwrap_or_default().contains("could not be loaded"));
    }

    #[test]
    fn gate_decision_evaluates_policy_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hook-policy.v1.json");
        std::fs::write(
            &path,
            serde_json::json!({
                "version": 1,
                "rules": [{
                    "id": "no-force-push",
                    "tools": ["Bash"],
                    "input_matchers": [{"field": "command", "regex": "git\\s+push\\b.*--force"}],
                    "decision": "deny",
                    "reason": "Force pushes are blocked by Animus policy."
                }]
            })
            .to_string(),
        )
        .unwrap();

        let denied = gate_decision(
            PRE_TOOL_USE,
            &path,
            &serde_json::json!({"tool_name": "Bash", "tool_input": {"command": "git push --force origin main"}}),
        );
        assert_eq!(denied.decision, PolicyDecision::Deny);
        assert_eq!(denied.rule_id.as_deref(), Some("no-force-push"));

        let deferred = gate_decision(
            PRE_TOOL_USE,
            &path,
            &serde_json::json!({"tool_name": "Bash", "tool_input": {"command": "ls"}}),
        );
        assert_eq!(deferred.decision, PolicyDecision::Defer);
    }

    #[test]
    fn event_record_includes_decision_when_present() {
        let record = event_record(
            PRE_TOOL_USE,
            "sess-1",
            Path::new("/tmp/project"),
            &serde_json::json!({"tool_name": "Bash"}),
            Some(&verdict(PolicyDecision::Deny, Some("blocked"))),
        );
        assert_eq!(record["event"], "PreToolUse");
        assert_eq!(record["session"], "sess-1");
        assert_eq!(record["decision"]["decision"], "deny");
        assert!(record["ts"].as_str().unwrap_or_default().ends_with('Z'));

        let plain = event_record("SessionEnd", "sess-1", Path::new("/tmp/project"), &Value::Null, None);
        assert!(plain.get("decision").is_none());
    }
}
