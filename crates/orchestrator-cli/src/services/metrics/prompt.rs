use std::io::{self, BufRead, IsTerminal, Write};
use std::path::Path;

use anyhow::Result;
use protocol::{metrics_env_disabled, Config, MetricsConfig};
use uuid::Uuid;

/// Source of the recorded consent choice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ConsentSource {
    /// Block already populated — nothing to do.
    AlreadyAnswered,
    /// `ANIMUS_METRICS_DISABLE=1` short-circuited the prompt.
    EnvDisabled,
    /// No TTY available; default to opt-out per the privacy contract.
    NoTtyDefault,
    /// User answered the interactive prompt.
    UserAnswered { opted_in: bool },
}

/// Shows the first-run prompt when the user has never been asked.
///
/// Consent is persisted into the **user-global** config
/// (`~/.animus/config.json`), not the project-local one. The
/// `_project_root` parameter is kept on the API for call-site symmetry
/// and to leave room for future per-project overrides.
///
/// Idempotent: subsequent calls are no-ops once the global `metrics`
/// block is populated.
pub(crate) fn maybe_prompt_first_run(_project_root: &Path) -> Result<ConsentSource> {
    if metrics_env_disabled() {
        return Ok(ConsentSource::EnvDisabled);
    }
    // Distinguish "config does not exist yet" from "config exists but
    // is unreadable/corrupt". In the corrupt-file case we must NOT
    // write a fresh default — that would silently erase the user's
    // existing profiles / MCP servers / token. Treat it as
    // `AlreadyAnswered` so the prompt stays silent until they fix
    // the file by hand.
    let global_path = Config::global_config_dir().join("config.json");
    let mut config = if global_path.exists() {
        match Config::load_global_if_exists() {
            Some(cfg) => cfg,
            None => return Ok(ConsentSource::AlreadyAnswered),
        }
    } else {
        Config {
            agent_runner_token: None,
            mcp_servers: Default::default(),
            claude_profiles: Default::default(),
            default_subject_kind: None,
            auto_update: None,
            metrics: None,
        }
    };
    if config.metrics.is_some() {
        return Ok(ConsentSource::AlreadyAnswered);
    }
    if !io::stdin().is_terminal() || !io::stderr().is_terminal() {
        config.metrics = Some(MetricsConfig { enabled: Some(false), ..MetricsConfig::default() });
        config.save_global()?;
        return Ok(ConsentSource::NoTtyDefault);
    }

    let opted_in = ask(&mut io::stderr(), &mut io::stdin().lock())?;
    let install_id = if opted_in { Some(Uuid::new_v4().to_string()) } else { None };
    config.metrics = Some(MetricsConfig { enabled: Some(opted_in), install_id, ..MetricsConfig::default() });
    config.save_global()?;
    Ok(ConsentSource::UserAnswered { opted_in })
}

const PROMPT_TEXT: &str = "Help improve Animus with anonymous usage data?\n\n\
Sends event counters only (workflows started, plugins installed, errors hit).\n\
No code, no file paths, no repo names, no prompts, no credentials.\n\
Aggregate counts only, batched daily.\n\n\
Opt in? [y/N] ";

fn ask<W: Write, R: BufRead>(out: &mut W, input: &mut R) -> Result<bool> {
    write!(out, "{PROMPT_TEXT}")?;
    out.flush()?;
    let mut line = String::new();
    input.read_line(&mut line)?;
    Ok(parse_answer(&line))
}

/// Returns true only on an explicit affirmative response (`y` / `yes`).
/// An empty Enter, whitespace, `n`, or anything else defaults to **opt-
/// out**: telemetry must require deliberate consent, so the safe path
/// is silence-equals-no.
fn parse_answer(raw: &str) -> bool {
    let trimmed = raw.trim();
    matches!(trimmed.chars().next().map(|c| c.to_ascii_lowercase()), Some('y'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_answer_defaults_to_opt_out() {
        assert!(!parse_answer(""));
        assert!(!parse_answer("\n"));
        assert!(!parse_answer("   "));
    }

    #[test]
    fn yes_answers_opt_in() {
        assert!(parse_answer("y"));
        assert!(parse_answer("Y"));
        assert!(parse_answer("yes"));
        assert!(parse_answer("YES\n"));
    }

    #[test]
    fn no_and_other_answers_opt_out() {
        assert!(!parse_answer("n"));
        assert!(!parse_answer("N"));
        assert!(!parse_answer("no"));
        assert!(!parse_answer("No\n"));
        assert!(!parse_answer("maybe"));
        assert!(!parse_answer("x"));
    }

    #[test]
    fn ask_records_opt_out_on_empty_line() {
        let mut out: Vec<u8> = Vec::new();
        let mut input = std::io::Cursor::new(b"\n".to_vec());
        let answer = ask(&mut out, &mut input).expect("ask succeeds");
        assert!(!answer, "empty Enter must default to opt-out");
        let rendered = String::from_utf8(out).unwrap();
        assert!(rendered.contains("Opt in?"));
        assert!(rendered.contains("[y/N]"));
    }

    #[test]
    fn ask_records_opt_in_on_y() {
        let mut out: Vec<u8> = Vec::new();
        let mut input = std::io::Cursor::new(b"y\n".to_vec());
        let answer = ask(&mut out, &mut input).expect("ask succeeds");
        assert!(answer);
    }
}
