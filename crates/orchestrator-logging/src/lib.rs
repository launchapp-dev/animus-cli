use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use chrono::Utc;
use fs2::FileExt;
use serde::{Deserialize, Serialize};

mod tracing_init;
pub use tracing_init::{init_agent_tracing, init_daemon_tracing, init_tracing, init_workflow_tracing};

pub mod log_redact;
pub use log_redact::{redact_log_entry, redact_string, REDACTED_PLACEHOLDER, REDACT_PATTERNS_ENV};

const MAX_LOG_SIZE: u64 = 50 * 1024 * 1024; // 50MB — full LLM content, no truncation
const ROTATED_SUFFIX: &str = ".1";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    Debug,
    Info,
    Warn,
    Error,
}

impl std::fmt::Display for Level {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Level::Debug => write!(f, "debug"),
            Level::Info => write!(f, "info"),
            Level::Warn => write!(f, "warn"),
            Level::Error => write!(f, "error"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogEntry {
    pub ts: String,
    pub level: Level,
    pub cat: String,
    pub msg: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workflow_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub schedule_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub phase_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub turn: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subject_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from_status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub to_status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pr_number: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mcp_tool: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mcp_server: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fallback_from: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fallback_to: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub meta: Option<serde_json::Value>,
}

impl LogEntry {
    fn new(level: Level, cat: impl Into<String>, msg: impl Into<String>) -> Self {
        Self {
            ts: Utc::now().to_rfc3339(),
            level,
            cat: cat.into(),
            msg: msg.into(),
            workflow_id: None,
            task_id: None,
            schedule_id: None,
            phase_id: None,
            model: None,
            tool: None,
            provider: None,
            run_id: None,
            session_id: None,
            turn: None,
            input_tokens: None,
            output_tokens: None,
            total_tokens: None,
            tool_calls: None,
            role: None,
            content: None,
            subject_id: None,
            status: None,
            from_status: None,
            to_status: None,
            branch: None,
            pr_number: None,
            mcp_tool: None,
            mcp_server: None,
            fallback_from: None,
            fallback_to: None,
            cost: None,
            exit_code: None,
            duration_ms: None,
            error: None,
            meta: None,
        }
    }
}

pub struct Logger {
    file: Mutex<Option<File>>,
    path: PathBuf,
    min_level: Level,
}

impl Logger {
    pub fn open(log_dir: &Path, filename: &str, min_level: Level) -> Self {
        let path = log_dir.join(filename);
        let _ = fs::create_dir_all(log_dir);
        let file = OpenOptions::new().create(true).append(true).open(&path).ok();
        Self { file: Mutex::new(file), path, min_level }
    }

    pub fn for_project(project_root: &Path) -> Self {
        let logs_dir = Self::logs_dir(project_root);
        Self::open(&logs_dir, "events.jsonl", Level::Info)
    }

    pub fn for_run(project_root: &Path, run_id: &str) -> Self {
        let runs_dir = Self::logs_dir(project_root).join("runs");
        Self::open(&runs_dir, &format!("{run_id}.jsonl"), Level::Debug)
    }

    /// Resolve the canonical log directory for a project. Delegates to
    /// `protocol::scoped_state_root` so the logging crate inherits the
    /// G1 same-origin-collision hardening; falls back to project-local
    /// `<project>/.animus/logs` when the scoped state root cannot be
    /// created or written (e.g. an unwritable `$HOME` on a service
    /// account). The fallback probe is best-effort: we try to create
    /// the logs subdirectory under the scoped root and, on failure,
    /// surface the project-local path instead so log writes don't
    /// silently disappear.
    pub fn logs_dir(project_root: &Path) -> PathBuf {
        let project_local_fallback = || project_root.join(".animus").join("logs");
        let Some(scope) = protocol::scoped_state_root(project_root) else {
            return project_local_fallback();
        };
        let logs = scope.join("logs");
        if fs::create_dir_all(&logs).is_ok() {
            logs
        } else {
            project_local_fallback()
        }
    }

    fn should_log(&self, level: Level) -> bool {
        (level as u8) >= (self.min_level as u8)
    }

    fn write_entry(&self, entry: &LogEntry) {
        if !self.should_log(entry.level) {
            return;
        }
        // v0.4.10: scrub secret-shaped values before they hit disk so every
        // emit site picks up redaction automatically. The clone is cheap
        // relative to JSON serialization + disk I/O on this path, and lets
        // the public `&LogEntry` API stay immutable.
        let mut owned = entry.clone();
        log_redact::redact_log_entry(&mut owned);
        let line = match serde_json::to_string(&owned) {
            Ok(l) => l,
            Err(_) => return,
        };
        let mut guard = match self.file.lock() {
            Ok(g) => g,
            Err(_) => return,
        };
        self.reopen_if_stale(&mut guard);
        if let Some(file) = guard.as_mut() {
            let _ = writeln!(file, "{}", line);
            let _ = file.flush();
        }
        self.rotate_if_needed(&mut guard);
    }

    // After another process rotates the log, this process's handle keeps
    // pointing at the renamed `.1` inode; detect that and reopen so new
    // entries land in the current file.
    #[cfg(unix)]
    fn reopen_if_stale(&self, guard: &mut Option<File>) {
        use std::os::unix::fs::MetadataExt;
        let Some(file) = guard.as_ref() else { return };
        let stale = match (file.metadata(), fs::metadata(&self.path)) {
            (Ok(handle), Ok(path)) => handle.ino() != path.ino() || handle.dev() != path.dev(),
            _ => true,
        };
        if stale {
            *guard = OpenOptions::new().create(true).append(true).open(&self.path).ok();
        }
    }

    #[cfg(not(unix))]
    fn reopen_if_stale(&self, _guard: &mut Option<File>) {}

    fn rotated_path(&self) -> PathBuf {
        self.path.with_extension(format!(
            "{}{}",
            self.path.extension().map(|e| e.to_string_lossy().to_string()).unwrap_or_default(),
            ROTATED_SUFFIX
        ))
    }

    // Called with the `file` mutex held so the size check, rename, and
    // reopen are atomic against in-process writers. A sidecar `.lock`
    // file (fs2 exclusive lock) extends the same guarantee across
    // processes (daemon + CLI share events.jsonl): the size is
    // re-checked after acquiring it so a second process that lost the
    // race cannot rename a near-empty fresh file over the rotated one.
    fn rotate_if_needed(&self, guard: &mut Option<File>) {
        if fs::metadata(&self.path).map(|m| m.len()).unwrap_or(0) < MAX_LOG_SIZE {
            return;
        }
        let lock_path = self.path.with_extension(format!(
            "{}{}",
            self.path.extension().map(|e| e.to_string_lossy().to_string()).unwrap_or_default(),
            ".lock"
        ));
        let lock_file = OpenOptions::new().create(true).truncate(false).write(true).open(&lock_path).ok();
        if let Some(lock) = &lock_file {
            let _ = lock.lock_exclusive();
        }
        if fs::metadata(&self.path).map(|m| m.len()).unwrap_or(0) >= MAX_LOG_SIZE {
            *guard = None;
            let _ = fs::rename(&self.path, self.rotated_path());
        }
        *guard = OpenOptions::new().create(true).append(true).open(&self.path).ok();
        if let Some(lock) = &lock_file {
            let _ = FileExt::unlock(lock);
        }
    }

    pub fn info(&self, cat: impl Into<String>, msg: impl Into<String>) -> EntryBuilder<'_> {
        EntryBuilder { logger: self, entry: LogEntry::new(Level::Info, cat, msg) }
    }

    pub fn warn(&self, cat: impl Into<String>, msg: impl Into<String>) -> EntryBuilder<'_> {
        EntryBuilder { logger: self, entry: LogEntry::new(Level::Warn, cat, msg) }
    }

    pub fn error(&self, cat: impl Into<String>, msg: impl Into<String>) -> EntryBuilder<'_> {
        EntryBuilder { logger: self, entry: LogEntry::new(Level::Error, cat, msg) }
    }

    pub fn debug(&self, cat: impl Into<String>, msg: impl Into<String>) -> EntryBuilder<'_> {
        EntryBuilder { logger: self, entry: LogEntry::new(Level::Debug, cat, msg) }
    }

    pub fn read_entries(&self, limit: usize, category: Option<&str>, level: Option<Level>) -> Vec<LogEntry> {
        self.read_entries_since(limit, category, level, None)
    }

    pub fn read_entries_since(
        &self,
        limit: usize,
        category: Option<&str>,
        level: Option<Level>,
        since: Option<&str>,
    ) -> Vec<LogEntry> {
        let mut all_lines = Vec::new();

        // Read rotated file first (older entries)
        let rotated = self.rotated_path();
        if rotated.exists() {
            if let Ok(file) = File::open(&rotated) {
                for line in BufReader::new(file).lines().map_while(Result::ok) {
                    all_lines.push(line);
                }
            }
        }

        // Then current file (newer entries)
        if self.path.exists() {
            if let Ok(file) = File::open(&self.path) {
                for line in BufReader::new(file).lines().map_while(Result::ok) {
                    all_lines.push(line);
                }
            }
        }

        let mut entries: Vec<LogEntry> = all_lines
            .iter()
            .rev()
            .filter_map(|line| serde_json::from_str::<LogEntry>(line).ok())
            .filter(|e| category.is_none_or(|c| e.cat == c || e.cat.starts_with(c)))
            .filter(|e| level.is_none_or(|l| (e.level as u8) >= (l as u8)))
            .filter(|e| since.is_none_or(|s| e.ts.as_str() >= s))
            .take(limit)
            .collect();
        entries.reverse();
        entries
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

pub struct EntryBuilder<'a> {
    logger: &'a Logger,
    entry: LogEntry,
}

impl<'a> EntryBuilder<'a> {
    pub fn workflow(mut self, id: impl Into<String>) -> Self {
        self.entry.workflow_id = Some(id.into());
        self
    }
    pub fn task(mut self, id: impl Into<String>) -> Self {
        self.entry.task_id = Some(id.into());
        self
    }
    pub fn schedule(mut self, id: impl Into<String>) -> Self {
        self.entry.schedule_id = Some(id.into());
        self
    }
    pub fn phase(mut self, id: impl Into<String>) -> Self {
        self.entry.phase_id = Some(id.into());
        self
    }
    pub fn model_tool(mut self, model: impl Into<String>, tool: impl Into<String>) -> Self {
        self.entry.model = Some(model.into());
        self.entry.tool = Some(tool.into());
        self
    }
    pub fn provider(mut self, provider: impl Into<String>) -> Self {
        self.entry.provider = Some(provider.into());
        self
    }
    pub fn run(mut self, id: impl Into<String>) -> Self {
        self.entry.run_id = Some(id.into());
        self
    }
    pub fn session(mut self, id: impl Into<String>) -> Self {
        self.entry.session_id = Some(id.into());
        self
    }
    pub fn turn(mut self, n: u32) -> Self {
        self.entry.turn = Some(n);
        self
    }
    pub fn tokens(mut self, input: u64, output: u64) -> Self {
        self.entry.input_tokens = Some(input);
        self.entry.output_tokens = Some(output);
        self.entry.total_tokens = Some(input + output);
        self
    }
    pub fn tool_calls(mut self, count: u32) -> Self {
        self.entry.tool_calls = Some(count);
        self
    }
    pub fn role(mut self, role: impl Into<String>) -> Self {
        self.entry.role = Some(role.into());
        self
    }
    pub fn content(mut self, text: impl Into<String>) -> Self {
        self.entry.content = Some(text.into());
        self
    }
    pub fn subject(mut self, id: impl Into<String>) -> Self {
        self.entry.subject_id = Some(id.into());
        self
    }
    pub fn status(mut self, s: impl Into<String>) -> Self {
        self.entry.status = Some(s.into());
        self
    }
    pub fn transition(mut self, from: impl Into<String>, to: impl Into<String>) -> Self {
        self.entry.from_status = Some(from.into());
        self.entry.to_status = Some(to.into());
        self
    }
    pub fn branch(mut self, b: impl Into<String>) -> Self {
        self.entry.branch = Some(b.into());
        self
    }
    pub fn pr(mut self, num: u32) -> Self {
        self.entry.pr_number = Some(num);
        self
    }
    pub fn mcp(mut self, tool: impl Into<String>, server: impl Into<String>) -> Self {
        self.entry.mcp_tool = Some(tool.into());
        self.entry.mcp_server = Some(server.into());
        self
    }
    pub fn fallback(mut self, from: impl Into<String>, to: impl Into<String>) -> Self {
        self.entry.fallback_from = Some(from.into());
        self.entry.fallback_to = Some(to.into());
        self
    }
    pub fn cost(mut self, c: f64) -> Self {
        self.entry.cost = Some(c);
        self
    }
    pub fn exit(mut self, code: i32) -> Self {
        self.entry.exit_code = Some(code);
        self
    }
    pub fn duration(mut self, ms: u64) -> Self {
        self.entry.duration_ms = Some(ms);
        self
    }
    pub fn err(mut self, error: impl Into<String>) -> Self {
        self.entry.error = Some(error.into());
        self
    }
    pub fn meta(mut self, value: serde_json::Value) -> Self {
        self.entry.meta = Some(value);
        self
    }
    pub fn emit(self) {
        self.logger.write_entry(&self.entry);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn logger_writes_structured_json_lines() {
        let dir = tempfile::tempdir().unwrap();
        let logger = Logger::open(dir.path(), "test.jsonl", Level::Debug);

        logger.info("schedule", "fired work-planner").schedule("work-planner").emit();

        logger
            .error("workflow", "runner exited with error")
            .workflow("wf-123")
            .task("TASK-456")
            .exit(1)
            .err("rate limit exceeded")
            .emit();

        let entries = logger.read_entries(10, None, None);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].cat, "schedule");
        assert_eq!(entries[0].level, Level::Info);
        assert_eq!(entries[0].schedule_id.as_deref(), Some("work-planner"));
        assert_eq!(entries[1].cat, "workflow");
        assert_eq!(entries[1].level, Level::Error);
        assert_eq!(entries[1].exit_code, Some(1));
    }

    #[test]
    fn llm_run_lifecycle_events() {
        let dir = tempfile::tempdir().unwrap();
        let logger = Logger::open(dir.path(), "test.jsonl", Level::Debug);

        logger
            .info("llm.start", "starting agent session")
            .run("run-abc")
            .session("sess-123")
            .model_tool("kimi-code/kimi-for-coding", "claude")
            .provider("kimi-code")
            .workflow("wf-001")
            .phase("implementation")
            .emit();

        logger.debug("llm.turn", "turn completed").run("run-abc").turn(3).tokens(1500, 800).tool_calls(2).emit();

        logger
            .info("llm.complete", "agent session finished")
            .run("run-abc")
            .session("sess-123")
            .tokens(12000, 5000)
            .tool_calls(15)
            .duration(45000)
            .exit(0)
            .emit();

        logger
            .error("llm.error", "API request failed")
            .run("run-xyz")
            .model_tool("minimax/MiniMax-M2.7", "claude")
            .provider("minimax")
            .turn(1)
            .err("429 rate limit exceeded")
            .emit();

        let entries = logger.read_entries(10, None, None);
        assert_eq!(entries.len(), 4);

        assert_eq!(entries[0].cat, "llm.start");
        assert_eq!(entries[0].provider.as_deref(), Some("kimi-code"));
        assert_eq!(entries[0].session_id.as_deref(), Some("sess-123"));

        assert_eq!(entries[1].cat, "llm.turn");
        assert_eq!(entries[1].turn, Some(3));
        assert_eq!(entries[1].input_tokens, Some(1500));
        assert_eq!(entries[1].tool_calls, Some(2));

        assert_eq!(entries[2].cat, "llm.complete");
        assert_eq!(entries[2].total_tokens, Some(17000));
        assert_eq!(entries[2].duration_ms, Some(45000));

        assert_eq!(entries[3].cat, "llm.error");
        assert_eq!(entries[3].error.as_deref(), Some("429 rate limit exceeded"));

        let llm_only = logger.read_entries(10, Some("llm.error"), None);
        assert_eq!(llm_only.len(), 1);
    }

    #[test]
    fn read_entries_filters_by_category() {
        let dir = tempfile::tempdir().unwrap();
        let logger = Logger::open(dir.path(), "test.jsonl", Level::Debug);

        logger.info("schedule", "dispatch").emit();
        logger.info("workflow", "started").emit();
        logger.info("schedule", "completed").emit();

        let entries = logger.read_entries(10, Some("schedule"), None);
        assert_eq!(entries.len(), 2);
    }

    #[test]
    fn read_entries_filters_by_level() {
        let dir = tempfile::tempdir().unwrap();
        let logger = Logger::open(dir.path(), "test.jsonl", Level::Debug);

        logger.debug("test", "noise").emit();
        logger.info("test", "info").emit();
        logger.error("test", "failure").emit();

        let entries = logger.read_entries(10, None, Some(Level::Warn));
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].level, Level::Error);
    }

    #[test]
    fn concurrent_writers_rotate_without_destroying_rotated_file() {
        let dir = tempfile::tempdir().unwrap();
        let logger = Logger::open(dir.path(), "rotate.jsonl", Level::Debug);

        let big = "a".repeat(1024 * 1024);
        std::thread::scope(|scope| {
            for _ in 0..7 {
                scope.spawn(|| {
                    for _ in 0..8 {
                        logger.info("rotation", "bulk").content(big.clone()).emit();
                    }
                });
            }
        });

        let rotated = logger.rotated_path();
        assert!(rotated.exists(), "writing past MAX_LOG_SIZE must rotate");
        let rotated_size = fs::metadata(&rotated).unwrap().len();
        assert!(
            rotated_size >= MAX_LOG_SIZE,
            "rotated file must hold the full pre-rotation history, got {} bytes",
            rotated_size
        );
        let current_size = fs::metadata(logger.path()).unwrap().len();
        assert!(current_size < MAX_LOG_SIZE, "current file should restart small, got {} bytes", current_size);

        let entries = logger.read_entries(1000, Some("rotation"), None);
        assert_eq!(entries.len(), 56, "no entries may be lost across rotation");
    }

    #[test]
    fn min_level_suppresses_lower_entries() {
        let dir = tempfile::tempdir().unwrap();
        let logger = Logger::open(dir.path(), "test.jsonl", Level::Warn);

        logger.debug("test", "should skip").emit();
        logger.info("test", "should skip").emit();
        logger.warn("test", "should write").emit();

        let entries = logger.read_entries(10, None, None);
        assert_eq!(entries.len(), 1);
    }

    #[cfg(unix)]
    mod scope_delegation {
        use super::*;
        use protocol::test_utils::EnvVarGuard;
        use std::os::unix::fs::PermissionsExt;
        use tempfile::tempdir;

        fn install_fake_git(bin: &Path, origin: &str) {
            let git_script = bin.join("git");
            std::fs::write(&git_script, format!("#!/bin/sh\necho '{origin}'\n")).expect("write fake git");
            let mut perms = std::fs::metadata(&git_script).expect("metadata").permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&git_script, perms).expect("set perms");
        }

        #[test]
        fn logs_dir_canonicalizes_noncanonical_project_root() {
            let temp = tempdir().expect("tempdir");
            let home = temp.path().join("home");
            let bin = temp.path().join("bin");
            let nested = temp.path().join("workspace").join("repo");
            std::fs::create_dir_all(home.join(".animus")).expect("ao root");
            std::fs::create_dir_all(&nested).expect("nested repo");
            std::fs::create_dir_all(&bin).expect("bin");
            install_fake_git(&bin, "git@github.com:example/canon.git");

            let _home_guard = EnvVarGuard::set("HOME", Some(home.to_string_lossy().as_ref()));
            let _path_guard = EnvVarGuard::set("PATH", Some(bin.to_string_lossy().as_ref()));

            let canonical = nested.canonicalize().expect("canonicalize nested");
            let noncanonical = temp.path().join("workspace").join("..").join("workspace").join("repo");

            let canon_logs = Logger::logs_dir(&canonical);
            let noncanon_logs = Logger::logs_dir(&noncanonical);

            assert_eq!(
                canon_logs, noncanon_logs,
                "noncanonical input must resolve to the same scope as canonical input"
            );
            assert!(
                canon_logs.starts_with(home.join(".animus")),
                "logs must land under ~/.animus, not project-local fallback: {:?}",
                canon_logs
            );
        }

        #[test]
        fn distinct_clones_of_same_origin_get_distinct_log_dirs() {
            let temp = tempdir().expect("tempdir");
            let home = temp.path().join("home");
            let bin = temp.path().join("bin");
            let clone_a = temp.path().join("clones").join("alpha");
            let clone_b = temp.path().join("clones").join("beta");
            std::fs::create_dir_all(home.join(".animus")).expect("ao root");
            std::fs::create_dir_all(&clone_a).expect("clone a");
            std::fs::create_dir_all(&clone_b).expect("clone b");
            std::fs::create_dir_all(&bin).expect("bin");
            install_fake_git(&bin, "git@github.com:example/shared.git");

            let _home_guard = EnvVarGuard::set("HOME", Some(home.to_string_lossy().as_ref()));
            let _path_guard = EnvVarGuard::set("PATH", Some(bin.to_string_lossy().as_ref()));

            let logs_a = Logger::logs_dir(&clone_a);
            let logs_b = Logger::logs_dir(&clone_b);

            assert_ne!(logs_a, logs_b, "G1 invariant: two clones of the same origin must not share a log directory");
            assert!(
                logs_a.starts_with(home.join(".animus")),
                "clone A logs must be scoped under ~/.animus: {:?}",
                logs_a
            );
            assert!(
                logs_b.starts_with(home.join(".animus")),
                "clone B logs must be scoped under ~/.animus: {:?}",
                logs_b
            );
        }

        #[test]
        fn for_project_writes_into_scoped_state_root() {
            let temp = tempdir().expect("tempdir");
            let home = temp.path().join("home");
            let bin = temp.path().join("bin");
            let repo = temp.path().join("repo");
            std::fs::create_dir_all(home.join(".animus")).expect("ao root");
            std::fs::create_dir_all(&repo).expect("repo");
            std::fs::create_dir_all(&bin).expect("bin");
            install_fake_git(&bin, "git@github.com:example/for-project.git");

            let _home_guard = EnvVarGuard::set("HOME", Some(home.to_string_lossy().as_ref()));
            let _path_guard = EnvVarGuard::set("PATH", Some(bin.to_string_lossy().as_ref()));

            let logger = Logger::for_project(&repo);
            logger.info("scope_test", "events go to the scoped path").emit();

            let expected = Logger::logs_dir(&repo).join("events.jsonl");
            assert_eq!(logger.path(), expected.as_path());
            assert!(expected.exists(), "logger should have written to scoped path: {:?}", expected);
            assert!(
                !repo.join(".animus").join("logs").join("events.jsonl").exists(),
                "logger must not fall back to project-local .animus/logs when scoped state is resolvable"
            );
        }

        #[test]
        fn for_run_writes_into_scoped_runs_dir() {
            let temp = tempdir().expect("tempdir");
            let home = temp.path().join("home");
            let bin = temp.path().join("bin");
            let repo = temp.path().join("repo");
            std::fs::create_dir_all(home.join(".animus")).expect("ao root");
            std::fs::create_dir_all(&repo).expect("repo");
            std::fs::create_dir_all(&bin).expect("bin");
            install_fake_git(&bin, "git@github.com:example/for-run.git");

            let _home_guard = EnvVarGuard::set("HOME", Some(home.to_string_lossy().as_ref()));
            let _path_guard = EnvVarGuard::set("PATH", Some(bin.to_string_lossy().as_ref()));

            let run_logger = Logger::for_run(&repo, "run-xyz");
            run_logger.debug("scope_test", "run-scoped log").run("run-xyz").emit();

            let expected = Logger::logs_dir(&repo).join("runs").join("run-xyz.jsonl");
            assert_eq!(run_logger.path(), expected.as_path());
            assert!(expected.exists(), "run logger should have written to scoped path: {:?}", expected);
        }
    }
}
