//! Command-eval runner. Spawns the configured program in the phase's working
//! directory, waits up to `timeout_secs`, and reports pass/fail based on
//! whether the exit code matches `expected_exit`.
//!
//! The runner never spawns through a shell — args are taken verbatim from
//! the eval definition so callers can audit them at config-load time. Env
//! is inherited from the parent process; per-check env injection is
//! deferred to a later cut.

use std::path::PathBuf;
use std::process::Stdio;
use std::time::{Duration, Instant};

use orchestrator_config::agent_runtime_config::EvalCheck;
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::Command;
use tokio::time::timeout;

use super::{excerpt, EvalCheckResult, EvalContext};

const DEFAULT_TIMEOUT_SECS: u64 = 300;

/// Hard ceiling on per-stream buffered bytes for a single check. Anything
/// past this is discarded on the fly; the persisted excerpt is built from
/// what we kept. 256 KiB per stream is two orders of magnitude beyond the
/// 2 KiB default excerpt cap, leaves comfortable head + tail context for
/// diagnostics, and bounds the runner's memory regardless of how chatty the
/// child is.
const MAX_PER_STREAM_BYTES: usize = 256 * 1024;

fn resolve_working_dir(ctx: &EvalContext, check: &EvalCheck) -> PathBuf {
    // `${REPO_ROOT}` would collide with the workflow-YAML env interpolation
    // layer (substituted at load time, so it never reaches the runner). We
    // therefore only advertise / honour the bare-sigil `$REPO_ROOT` form.
    match check.working_dir.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        Some("$REPO_ROOT") | None => ctx.default_working_dir.clone(),
        Some(path) => {
            // Per codex round-3 P2: relative overrides MUST anchor on the
            // default working dir, not the runner process cwd, otherwise
            // the same YAML behaves differently depending on where the
            // daemon was started.
            let candidate = PathBuf::from(path);
            if candidate.is_absolute() {
                candidate
            } else {
                ctx.default_working_dir.join(candidate)
            }
        }
    }
}

pub async fn run_command_check(ctx: &EvalContext, check: &EvalCheck) -> EvalCheckResult {
    let start = Instant::now();
    let program = match check.command.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        Some(p) => p,
        None => {
            return EvalCheckResult::new(
                ctx,
                check,
                false,
                0,
                None,
                String::new(),
                Some("command field is empty — should have been caught by validation".to_string()),
            );
        }
    };
    let cwd = resolve_working_dir(ctx, check);
    let timeout_dur = Duration::from_secs(check.timeout_secs.unwrap_or(DEFAULT_TIMEOUT_SECS));
    let excerpt_cap = ctx.excerpt_cap();

    let mut cmd = Command::new(program);
    cmd.args(&check.args)
        .current_dir(&cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    // Codex round-3 P2: put the child (and any descendants it spawns) into
    // its own process group so a timeout can kill the entire tree, not just
    // the direct child. `process_group(0)` is a Unix-only tokio API; on
    // Windows we fall back to `kill_on_drop` which terminates the immediate
    // child (Windows job-object teardown is a larger fold-in).
    #[cfg(unix)]
    cmd.process_group(0);

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(err) => {
            return EvalCheckResult::new(
                ctx,
                check,
                false,
                start.elapsed().as_millis() as u64,
                None,
                String::new(),
                Some(format!("failed to spawn '{program}': {err}")),
            );
        }
    };
    let child_pid = child.id();

    // Drain stdout/stderr concurrently with the wait so verbose commands
    // (>64 KiB on either pipe — pipe-buffer-full territory) cannot deadlock
    // the child against our wait(). Per codex round-2 P2 we cap each stream
    // at `MAX_PER_STREAM_BYTES`: a runaway `yes`-style command cannot OOM the
    // runner — surplus bytes are discarded after we have a head + tail to
    // include in the excerpt. Codex round-11 P2: a successful command may
    // exit while a daemonized descendant inherits the pipes; we therefore
    // run the drains in concurrent tasks and detach them after the leader's
    // wait() resolves (with a short grace window so in-flight bytes still
    // land in the excerpt). A daemonized descendant that keeps the pipes
    // open after the leader exits no longer pins the check to the timeout.
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    const POST_EXIT_DRAIN_GRACE: Duration = Duration::from_millis(150);
    let stdout_buf = std::sync::Arc::new(std::sync::Mutex::new(Vec::<u8>::new()));
    let stderr_buf = std::sync::Arc::new(std::sync::Mutex::new(Vec::<u8>::new()));
    let mut stdout_handle =
        tokio::spawn(drain_bounded_into(stdout, MAX_PER_STREAM_BYTES, std::sync::Arc::clone(&stdout_buf)));
    let mut stderr_handle =
        tokio::spawn(drain_bounded_into(stderr, MAX_PER_STREAM_BYTES, std::sync::Arc::clone(&stderr_buf)));

    // Codex round-12 P2: only the wait() goes inside the outer command
    // timeout. The post-exit drain grace runs AFTER the wait completes so
    // a leader that exited just before the deadline cannot be misreported
    // as timed out by the 150ms drain window slipping over the cliff.
    let wait_outcome = timeout(timeout_dur, child.wait()).await;

    let status_result = match wait_outcome {
        Ok(status) => status,
        Err(_elapsed) => {
            // Codex round-3 P2: kill the whole process group on timeout
            // so any descendants get cleaned up instead of leaking.
            let mut killed_via_group = false;
            #[cfg(unix)]
            if let Some(pid) = child_pid {
                killed_via_group = kill_process_group_unix(pid);
            }
            #[cfg(not(unix))]
            let _ = child_pid;
            // Codex round-5/round-10 P2: kill the direct child if the
            // group kill did not land (Windows, sanitized sandboxes).
            if !killed_via_group {
                let _ = child.start_kill();
            }
            // Codex round-4 P2: reap so we do not leave a zombie.
            let _ = child.wait().await;
            stdout_handle.abort();
            stderr_handle.abort();
            let duration_ms = start.elapsed().as_millis() as u64;
            return EvalCheckResult::new(
                ctx,
                check,
                false,
                duration_ms,
                None,
                String::new(),
                Some(format!("timed out after {} secs", timeout_dur.as_secs())),
            );
        }
    };

    // Codex round-11 P2: give the drains a short post-exit window so
    // bytes still in flight from the leader land in the excerpt. This
    // grace runs OUTSIDE the outer command timeout per codex round-12 P2
    // — a leader that exited just before the deadline must not be
    // misreported as timed out simply because the drain slipped over.
    let _ = tokio::time::timeout(POST_EXIT_DRAIN_GRACE, &mut stdout_handle).await;
    let _ = tokio::time::timeout(POST_EXIT_DRAIN_GRACE, &mut stderr_handle).await;
    stdout_handle.abort();
    stderr_handle.abort();

    let duration_ms = start.elapsed().as_millis() as u64;

    match status_result {
        Ok(status) => {
            let exit_code = status.code();
            let passed = exit_code.map(|c| c == check.expected_exit).unwrap_or(false);
            let stdout_bytes = stdout_buf.lock().map(|g| g.clone()).unwrap_or_default();
            let stderr_bytes = stderr_buf.lock().map(|g| g.clone()).unwrap_or_default();
            let combined = build_combined_excerpt(&stdout_bytes, &stderr_bytes, excerpt_cap);
            EvalCheckResult::new(ctx, check, passed, duration_ms, exit_code, combined, None)
        }
        Err(err) => EvalCheckResult::new(
            ctx,
            check,
            false,
            duration_ms,
            None,
            String::new(),
            Some(format!("wait failed: {err}")),
        ),
    }
}

/// Read up to `max_bytes` from `reader` into the shared `sink`, discarding
/// anything past that ceiling but continuing to read (and drop) so the
/// child does not block on a full pipe. Designed to be spawned as its
/// own task so the caller can abort it without losing data already
/// written to `sink`.
async fn drain_bounded_into<R: AsyncRead + Unpin>(
    reader: Option<R>,
    max_bytes: usize,
    sink: std::sync::Arc<std::sync::Mutex<Vec<u8>>>,
) {
    let Some(mut reader) = reader else {
        return;
    };
    let mut tmp = [0u8; 8 * 1024];
    loop {
        match reader.read(&mut tmp).await {
            Ok(0) => return,
            Ok(n) => {
                if let Ok(mut buf) = sink.lock() {
                    if buf.len() < max_bytes {
                        let take = std::cmp::min(n, max_bytes - buf.len());
                        buf.extend_from_slice(&tmp[..take]);
                    }
                    // else: surplus bytes are discarded but we keep
                    // draining the pipe so the child can make progress.
                }
            }
            Err(_) => return,
        }
    }
}

#[cfg(unix)]
fn kill_process_group_unix(pid: u32) -> bool {
    use std::process::{Command as StdCommand, Stdio};
    // Best-effort: try SIGTERM first, then SIGKILL after a brief grace
    // window. `kill -PG -<pgid>` targets the entire group; the pgid equals
    // the leader's pid because we spawned with `process_group(0)`. We
    // address /bin/kill via its absolute path so a daemon launched without
    // /bin on PATH still resolves the helper. stderr is silenced because
    // `kill` is noisy on races (the leader may have already exited; in
    // tests the child may be re-parented before we get here). Returns
    // `true` when SIGKILL was successfully dispatched so the caller can
    // fall back to `child.start_kill()` if the helper itself failed to
    // spawn — e.g. in sanitized sandboxes where /bin/kill is unreachable.
    let pgid = format!("-{pid}");
    let term_ok = StdCommand::new("/bin/kill")
        .arg("-TERM")
        .arg(&pgid)
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if term_ok {
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
    StdCommand::new("/bin/kill")
        .arg("-KILL")
        .arg(&pgid)
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn build_combined_excerpt(stdout: &[u8], stderr: &[u8], cap: usize) -> String {
    let stdout_s = String::from_utf8_lossy(stdout);
    let stderr_s = String::from_utf8_lossy(stderr);
    if stderr_s.is_empty() {
        excerpt(&stdout_s, cap)
    } else if stdout_s.is_empty() {
        excerpt(&stderr_s, cap)
    } else {
        let half = cap.saturating_sub(16) / 2;
        let head = excerpt(&stdout_s, half);
        let tail = excerpt(&stderr_s, half);
        format!("[stdout]\n{head}\n[stderr]\n{tail}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use orchestrator_config::agent_runtime_config::EvalKind;
    use tempfile::TempDir;

    fn cmd_check(id: &str, program: &str, args: Vec<&str>, expected_exit: i32) -> EvalCheck {
        EvalCheck {
            id: id.into(),
            kind: EvalKind::Command,
            command: Some(program.into()),
            args: args.into_iter().map(String::from).collect(),
            working_dir: None,
            timeout_secs: Some(5),
            expected_exit,
            agent: None,
            prompt: None,
        }
    }

    fn ctx_for(tmp: &TempDir) -> EvalContext {
        EvalContext::new("implementation", tmp.path().to_path_buf())
    }

    #[tokio::test]
    async fn command_check_passes_on_expected_exit_zero() {
        let tmp = TempDir::new().expect("tmp");
        let ctx = ctx_for(&tmp);
        let check = cmd_check("ok", "true", Vec::new(), 0);
        let result = run_command_check(&ctx, &check).await;
        assert!(result.passed, "expected pass, got {result:?}");
        assert_eq!(result.exit_code, Some(0));
        assert_eq!(result.kind, EvalKind::Command);
    }

    #[tokio::test]
    async fn command_check_fails_on_non_matching_exit() {
        let tmp = TempDir::new().expect("tmp");
        let ctx = ctx_for(&tmp);
        let check = cmd_check("bad", "false", Vec::new(), 0);
        let result = run_command_check(&ctx, &check).await;
        assert!(!result.passed);
        assert_eq!(result.exit_code, Some(1));
    }

    #[tokio::test]
    async fn command_check_treats_nonzero_expected_exit_as_pass() {
        let tmp = TempDir::new().expect("tmp");
        let ctx = ctx_for(&tmp);
        let check = cmd_check("bad-but-expected", "false", Vec::new(), 1);
        let result = run_command_check(&ctx, &check).await;
        assert!(result.passed, "expected pass when exit code matches expected_exit=1, got {result:?}");
    }

    #[tokio::test]
    async fn command_check_records_excerpt_from_stdout() {
        let tmp = TempDir::new().expect("tmp");
        let ctx = ctx_for(&tmp);
        let check = cmd_check("echo", "/bin/echo", vec!["hello-from-eval"], 0);
        let result = run_command_check(&ctx, &check).await;
        assert!(result.passed);
        assert!(result.output_excerpt.contains("hello-from-eval"), "got {:?}", result.output_excerpt);
    }

    #[tokio::test]
    async fn command_check_times_out_when_program_hangs() {
        let tmp = TempDir::new().expect("tmp");
        let ctx = ctx_for(&tmp);
        let mut check = cmd_check("hang", "/bin/sleep", vec!["10"], 0);
        check.timeout_secs = Some(1);
        let result = run_command_check(&ctx, &check).await;
        assert!(!result.passed);
        assert!(
            result.error.as_deref().unwrap_or("").contains("timed out"),
            "expected timeout error, got {:?}",
            result.error
        );
    }

    #[tokio::test]
    async fn command_check_reports_spawn_failure() {
        let tmp = TempDir::new().expect("tmp");
        let ctx = ctx_for(&tmp);
        let check = cmd_check("missing", "/this/program/does/not/exist", Vec::new(), 0);
        let result = run_command_check(&ctx, &check).await;
        assert!(!result.passed);
        assert!(result.error.is_some(), "expected an error message, got {:?}", result.error);
    }

    #[tokio::test]
    async fn command_check_drains_large_stdout_without_deadlocking() {
        // Regression for codex round-1 P2: prior wait-then-read flow could
        // deadlock the child against a full stdout pipe (>64 KiB on Linux).
        // We exercise that path here by emitting ~256 KiB of zeros via `yes`
        // through `head -c`, which is well past any pipe-buffer threshold.
        let tmp = TempDir::new().expect("tmp");
        let ctx = ctx_for(&tmp);
        let check = EvalCheck {
            id: "noisy".into(),
            kind: EvalKind::Command,
            command: Some("/bin/sh".into()),
            args: vec!["-c".into(), "yes 0 | head -c 262144".into()],
            working_dir: None,
            timeout_secs: Some(10),
            expected_exit: 0,
            agent: None,
            prompt: None,
        };
        let result = run_command_check(&ctx, &check).await;
        assert!(result.passed, "expected pass on verbose output, got {result:?}");
        assert!(
            !result.error.as_deref().unwrap_or("").contains("timed out"),
            "verbose output must not race the timeout, got {:?}",
            result.error
        );
    }

    #[tokio::test]
    async fn command_check_resolves_relative_working_dir_against_default() {
        // Regression for codex round-3 P2: a relative `working_dir` MUST be
        // joined onto the context's default working dir, not the runner
        // process cwd. We seed a `nested` directory under the tempdir and
        // assert pwd inside it.
        let tmp = TempDir::new().expect("tmp");
        let nested = tmp.path().join("nested");
        std::fs::create_dir_all(&nested).expect("mkdir nested");
        let ctx = ctx_for(&tmp);
        let mut check = cmd_check("pwd-rel", "/bin/pwd", Vec::new(), 0);
        check.working_dir = Some("nested".to_string());
        let result = run_command_check(&ctx, &check).await;
        assert!(result.passed, "expected pass, got {result:?}");
        let nested_canonical = std::fs::canonicalize(&nested).expect("canonicalize");
        assert!(
            result.output_excerpt.contains(&*nested_canonical.to_string_lossy()),
            "relative working_dir should anchor on default; expected {:?}, got {:?}",
            nested_canonical,
            result.output_excerpt
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn command_check_does_not_timeout_when_leader_exits_with_open_pipe_in_descendant() {
        // Regression for codex round-11 P2: a successful command that
        // daemonizes a descendant inheriting stdout/stderr must not be
        // misreported as timed out. The leader exits zero in well under
        // the configured timeout; the post-exit drain grace handles the
        // descendant's inherited pipe.
        let tmp = TempDir::new().expect("tmp");
        let ctx = ctx_for(&tmp);
        let script = "echo hello; sleep 30 &";
        let mut check = cmd_check("daemonize", "/bin/sh", vec!["-c", script], 0);
        check.timeout_secs = Some(10);
        let start = std::time::Instant::now();
        let result = run_command_check(&ctx, &check).await;
        let elapsed = start.elapsed();
        assert!(result.passed, "leader-exit-zero must pass, got {result:?}");
        assert!(elapsed.as_secs() < 5, "must not block on inherited pipe (took {:.1}s)", elapsed.as_secs_f64());
        assert!(result.output_excerpt.contains("hello"), "leader output should still be captured");
        // Clean up the orphaned sleep so it doesn't leak past the test.
        let _ =
            std::process::Command::new("pkill").args(["-f", "sleep 30"]).stderr(std::process::Stdio::null()).status();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn command_check_kills_descendants_on_timeout() {
        // Regression for codex round-3 P2: a timed-out command must kill its
        // descendants, not just the direct child. We spawn a shell that
        // forks a long sleep into the background, writes its pid to a
        // file, then ALSO hangs the leader so the timeout path fires.
        // After the timeout we read the backgrounded sleep's pid and assert
        // it is gone (kill_process_group_unix should have signalled it).
        let tmp = TempDir::new().expect("tmp");
        let ctx = ctx_for(&tmp);
        let pidfile = tmp.path().join("child.pid");
        let pidfile_str = pidfile.to_string_lossy().into_owned();
        let script = format!("sleep 30 & echo $! > {pidfile_str}; sleep 30");
        let mut check = cmd_check("descendants", "/bin/sh", vec!["-c", &script], 0);
        check.timeout_secs = Some(1);
        let result = run_command_check(&ctx, &check).await;
        assert!(!result.passed, "timed-out command must report failure");
        // Give the kill -KILL a moment to land before we sample /proc-style.
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        let raw = std::fs::read_to_string(&pidfile).expect("pidfile readable");
        let pid: i32 = raw.trim().parse().expect("pid parses");
        // `kill -0` returns 0 when the pid is alive, non-zero when reaped.
        let alive = std::process::Command::new("kill")
            .args(["-0", &pid.to_string()])
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        assert!(!alive, "backgrounded descendant (pid {pid}) must be killed by timeout cleanup");
    }

    #[tokio::test]
    async fn command_check_honours_working_dir_override() {
        let tmp = TempDir::new().expect("tmp");
        let other = TempDir::new().expect("other");
        let ctx = ctx_for(&tmp);
        let mut check = cmd_check("pwd", "/bin/pwd", Vec::new(), 0);
        check.working_dir = Some(other.path().to_string_lossy().into_owned());
        let result = run_command_check(&ctx, &check).await;
        assert!(result.passed);
        let other_canonical = std::fs::canonicalize(other.path()).expect("canonicalize");
        assert!(
            result.output_excerpt.contains(&*other_canonical.to_string_lossy()),
            "expected excerpt to contain {:?}, got {:?}",
            other_canonical,
            result.output_excerpt
        );
    }
}
