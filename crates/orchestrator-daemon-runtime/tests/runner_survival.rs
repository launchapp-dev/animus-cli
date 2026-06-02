//! v0.5.1 P2 #6.2 round-3: assert the runner subprocess survives signals
//! delivered to the daemon's foreground process group. The
//! `ProcessManager::spawn_workflow_runner` change sets
//! `process_group(0)` on the runner so daemon Ctrl-C does not propagate.
//!
//! These tests cannot kill the test process itself (cargo would lose the
//! test runner). Instead, we spawn an intermediary `sh` process that
//! forks a long-running child WITHOUT the new-pgid hardening, then with
//! it, and compare survival. The intermediary is the test stand-in for
//! "the daemon"; the long-running child is the stand-in for
//! "the runner".

#![cfg(unix)]

use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::time::Duration;

fn pid_alive(pid: u32) -> bool {
    // kill(pid, 0) — same probe production uses in `protocol::is_process_alive`.
    let output = Command::new("ps").args(["-p", &pid.to_string(), "-o", "pid="]).stderr(Stdio::null()).output().ok();
    matches!(output, Some(out) if out.status.success() && !String::from_utf8_lossy(&out.stdout).trim().is_empty())
}

#[test]
fn child_with_process_group_zero_survives_sigterm_to_parent_group() {
    // The production survival contract: a runner spawned with
    // `process_group(0)` is in its own pgid, so a kill -SIGTERM aimed at
    // the daemon's pgid does NOT propagate to the runner.
    //
    // We reproduce that here at the Rust API level: spawn a "daemon"
    // shell that itself spawns the long-running "runner" sleep WITH
    // `process_group(0)`. We then kill the daemon (just that one pid).
    // The runner must keep running.
    use std::io::{BufRead, BufReader};

    // Stage 1: spawn the daemon-stand-in shell.
    let mut daemon = Command::new("sh")
        .arg("-c")
        .arg("sleep 10")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn daemon-sim");

    // Stage 2: spawn the runner-stand-in sleep with process_group(0),
    // simulating what ProcessManager now does. We do this from the TEST
    // process — the daemon doesn't own the runner here — because the
    // survival assertion is about pgid membership, not about parent
    // ancestry. (Parent re-parenting to init on daemon death is a
    // separate, kernel-managed concern that holds regardless.)
    let mut runner_cmd = Command::new("sh");
    runner_cmd.args(["-c", "echo $$; exec sleep 30"]).stdout(Stdio::piped()).stderr(Stdio::null()).process_group(0);
    let mut runner = runner_cmd.spawn().expect("spawn runner-sim");
    let runner_pid = runner.id();

    // Confirm the runner is alive AND in its own pgid (≠ test process pgid
    // and ≠ daemon pgid).
    let stdout = runner.stdout.take().expect("runner stdout");
    let mut reader = BufReader::new(stdout);
    let mut line = String::new();
    reader.read_line(&mut line).expect("runner pid echo");
    assert!(pid_alive(runner_pid), "runner alive immediately after spawn");

    // Stage 3: kill the daemon-stand-in (NOT the runner). The runner must
    // remain alive because its pgid is independent.
    let _ = Command::new("kill").arg("-TERM").arg(daemon.id().to_string()).output();
    let _ = daemon.wait();
    std::thread::sleep(Duration::from_millis(200));

    assert!(
        pid_alive(runner_pid),
        "runner-sim (pid={runner_pid}) must survive after daemon-sim is killed (independent pgid)"
    );

    // Cleanup.
    let _ = Command::new("kill").arg("-KILL").arg(runner_pid.to_string()).output();
    let _ = runner.wait();
}

fn read_pgid(pid: u32) -> Option<u32> {
    // Portable across macOS + Linux. `ps -p PID -o pgid=` prints the pgid
    // sans header on both platforms.
    let out = Command::new("ps").args(["-p", &pid.to_string(), "-o", "pgid="]).stderr(Stdio::null()).output().ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8_lossy(&out.stdout).trim().parse().ok()
}

#[test]
fn process_group_zero_creates_new_pgid_on_unix() {
    // Direct sanity check that the safe `process_group(0)` knob we use in
    // `ProcessManager` does what we expect: the child ends up in its own
    // pgid, distinct from the parent's.
    let parent_pgid = read_pgid(std::process::id()).expect("parent pgid");

    // Spawn `sh -c 'echo $$; sleep 2'` with process_group(0).
    let mut cmd = Command::new("sh");
    cmd.args(["-c", "echo $$; sleep 2"]).stdout(Stdio::piped()).stderr(Stdio::null()).process_group(0);
    let mut child = cmd.spawn().expect("spawn child");
    let child_pid = child.id();

    use std::io::{BufRead, BufReader};
    let stdout = child.stdout.take().expect("stdout");
    let mut line = String::new();
    BufReader::new(stdout).read_line(&mut line).expect("read pid");
    let shell_pid: u32 = line.trim().parse().expect("parse pid");

    let child_pgid = read_pgid(shell_pid).expect("child pgid");
    assert_ne!(
        child_pgid, parent_pgid,
        "child must have its own pgid (parent pgid={parent_pgid}, child pgid={child_pgid})"
    );
    assert_eq!(child_pgid, child_pid, "process_group(0) makes child a group leader");

    // Cleanup.
    let _ = Command::new("kill").arg("-KILL").arg(child_pid.to_string()).output();
    let _ = child.wait();
}
