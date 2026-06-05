//! Recent-crash detection: scoped runtime `runs/*/phases/*.session.json`
//! files whose JSON status is `Blocked` with a recent `blocked_at` timestamp.
//!
//! Conservative: any unreadable file is silently skipped — doctor must never
//! grow a transitive parser bug on a corrupted session file.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde::Deserialize;

use super::check_kit::{CheckContext, CheckFix, CheckStatus, DiagnosticCheck};

const CATEGORY: &str = "recent_crashes";
const RECENT_WINDOW: Duration = Duration::from_hours(24);

#[derive(Debug, Deserialize)]
struct SessionFileShape {
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    workflow_id: Option<String>,
}

pub(crate) fn run(ctx: &CheckContext) -> Vec<DiagnosticCheck> {
    let runs_root = match protocol::scoped_state_root(&ctx.project_root) {
        Some(root) => root.join("runs"),
        None => return Vec::new(),
    };

    if !runs_root.is_dir() {
        return vec![DiagnosticCheck::new("recent_crashes", CATEGORY, CheckStatus::Skipped, "Recent phase crashes")
            .details(format!("no scoped runs dir at {}", runs_root.display()))];
    }

    let mut recent: Vec<(PathBuf, String)> = Vec::new();
    let now = SystemTime::now();
    collect_blocked_sessions(&runs_root, &now, &mut recent, 0);

    if recent.is_empty() {
        return vec![DiagnosticCheck::new("recent_crashes", CATEGORY, CheckStatus::Pass, "Recent phase crashes")
            .details("no blocked phase session in the last 24h".to_string())];
    }

    let count = recent.len();
    let preview = recent
        .iter()
        .take(3)
        .map(|(path, wf)| format!("{} (workflow {wf})", path.display()))
        .collect::<Vec<_>>()
        .join("; ");
    let resume_hint = recent
        .first()
        .map(|(_, wf)| format!("animus workflow resume {wf} --force"))
        .unwrap_or_else(|| "animus workflow resume <id> --force".to_string());

    vec![DiagnosticCheck::new("recent_crashes", CATEGORY, CheckStatus::Warn, "Recent phase crashes")
        .current(format!("{count} blocked phase session(s) in last 24h: {preview}"))
        .expected("no recent blocked sessions".to_string())
        .fix(CheckFix::command(
            "resume_blocked_workflow",
            "Resume the most recent blocked workflow (manual review recommended first).",
            &resume_hint,
        ))]
}

fn collect_blocked_sessions(dir: &Path, now: &SystemTime, out: &mut Vec<(PathBuf, String)>, depth: usize) {
    if depth > 5 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        let file_type = match entry.file_type() {
            Ok(t) => t,
            Err(_) => continue,
        };
        if file_type.is_dir() {
            collect_blocked_sessions(&path, now, out, depth + 1);
            continue;
        }
        if !path.file_name().and_then(|n| n.to_str()).map(|n| n.ends_with(".session.json")).unwrap_or(false) {
            continue;
        }
        // Cheap mtime filter first — avoids parsing every old session file.
        if let Ok(meta) = entry.metadata() {
            if let Ok(modified) = meta.modified() {
                if now.duration_since(modified).map(|d| d > RECENT_WINDOW).unwrap_or(false) {
                    continue;
                }
            }
        }
        let Ok(raw) = std::fs::read_to_string(&path) else { continue };
        let Ok(parsed) = serde_json::from_str::<SessionFileShape>(&raw) else { continue };
        let is_blocked = parsed.status.as_deref().map(|s| s.eq_ignore_ascii_case("blocked")).unwrap_or(false);
        if !is_blocked {
            continue;
        }
        let workflow_id = parsed.workflow_id.unwrap_or_else(|| "<unknown>".to_string());
        out.push((path, workflow_id));
    }
}
