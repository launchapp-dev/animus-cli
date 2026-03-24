use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use chrono::Utc;
use serde::{Deserialize, Serialize};

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
        let scope_root = match protocol_scope_root(project_root) {
            Some(p) => p,
            None => project_root.join(".ao"),
        };
        Self::open(&scope_root.join("logs"), "events.jsonl", Level::Info)
    }

    pub fn for_run(project_root: &Path, run_id: &str) -> Self {
        let scope_root = match protocol_scope_root(project_root) {
            Some(p) => p,
            None => project_root.join(".ao"),
        };
        Self::open(
            &scope_root.join("logs").join("runs"),
            &format!("{run_id}.jsonl"),
            Level::Debug,
        )
    }

    pub fn logs_dir(project_root: &Path) -> PathBuf {
        match protocol_scope_root(project_root) {
            Some(p) => p.join("logs"),
            None => project_root.join(".ao").join("logs"),
        }
    }

    fn should_log(&self, level: Level) -> bool {
        (level as u8) >= (self.min_level as u8)
    }

    fn write_entry(&self, entry: &LogEntry) {
        if !self.should_log(entry.level) {
            return;
        }
        let line = match serde_json::to_string(entry) {
            Ok(l) => l,
            Err(_) => return,
        };
        let mut guard = match self.file.lock() {
            Ok(g) => g,
            Err(_) => return,
        };
        if let Some(file) = guard.as_mut() {
            let _ = writeln!(file, "{}", line);
            let _ = file.flush();
        }
        drop(guard);
        self.rotate_if_needed();
    }

    fn rotate_if_needed(&self) {
        let size = fs::metadata(&self.path).map(|m| m.len()).unwrap_or(0);
        if size < MAX_LOG_SIZE {
            return;
        }
        let rotated = self.path.with_extension(format!(
            "{}{}",
            self.path.extension().map(|e| e.to_string_lossy().to_string()).unwrap_or_default(),
            ROTATED_SUFFIX
        ));
        let _ = fs::rename(&self.path, &rotated);
        let mut guard = match self.file.lock() {
            Ok(g) => g,
            Err(_) => return,
        };
        *guard = OpenOptions::new().create(true).append(true).open(&self.path).ok();
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

    pub fn read_entries(
        &self,
        limit: usize,
        category: Option<&str>,
        level: Option<Level>,
    ) -> Vec<LogEntry> {
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
        let rotated = self.path.with_extension(format!(
            "{}{}",
            self.path.extension().map(|e| e.to_string_lossy().to_string()).unwrap_or_default(),
            ROTATED_SUFFIX
        ));
        if rotated.exists() {
            if let Ok(file) = File::open(&rotated) {
                for line in BufReader::new(file).lines().flatten() {
                    all_lines.push(line);
                }
            }
        }

        // Then current file (newer entries)
        if self.path.exists() {
            if let Ok(file) = File::open(&self.path) {
                for line in BufReader::new(file).lines().flatten() {
                    all_lines.push(line);
                }
            }
        }

        let mut entries: Vec<LogEntry> = all_lines
            .iter()
            .rev()
            .filter_map(|line| serde_json::from_str::<LogEntry>(line).ok())
            .filter(|e| category.map_or(true, |c| e.cat == c || e.cat.starts_with(c)))
            .filter(|e| level.map_or(true, |l| (e.level as u8) >= (l as u8)))
            .filter(|e| since.map_or(true, |s| e.ts.as_str() >= s))
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

fn protocol_scope_root(project_root: &Path) -> Option<PathBuf> {
    let home = std::env::var("HOME").ok()?;
    let repo_scope = project_root.file_name()?.to_str()?;
    let ao_dir = Path::new(&home).join(".ao");
    for entry in fs::read_dir(&ao_dir).ok()? {
        let entry = entry.ok()?;
        let name = entry.file_name();
        let name_str = name.to_str()?;
        if name_str.starts_with(repo_scope) && entry.path().is_dir() {
            let project_root_file = entry.path().join(".project-root");
            if project_root_file.exists() {
                if let Ok(content) = fs::read_to_string(&project_root_file) {
                    if content.trim() == project_root.to_string_lossy().trim() {
                        return Some(entry.path());
                    }
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn logger_writes_structured_json_lines() {
        let dir = tempfile::tempdir().unwrap();
        let logger = Logger::open(dir.path(), "test.jsonl", Level::Debug);

        logger.info("schedule", "fired work-planner")
            .schedule("work-planner")
            .emit();

        logger.error("workflow", "runner exited with error")
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

        logger.info("llm.start", "starting agent session")
            .run("run-abc")
            .session("sess-123")
            .model_tool("kimi-code/kimi-for-coding", "claude")
            .provider("kimi-code")
            .workflow("wf-001")
            .phase("implementation")
            .emit();

        logger.debug("llm.turn", "turn completed")
            .run("run-abc")
            .turn(3)
            .tokens(1500, 800)
            .tool_calls(2)
            .emit();

        logger.info("llm.complete", "agent session finished")
            .run("run-abc")
            .session("sess-123")
            .tokens(12000, 5000)
            .tool_calls(15)
            .duration(45000)
            .exit(0)
            .emit();

        logger.error("llm.error", "API request failed")
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
    fn min_level_suppresses_lower_entries() {
        let dir = tempfile::tempdir().unwrap();
        let logger = Logger::open(dir.path(), "test.jsonl", Level::Warn);

        logger.debug("test", "should skip").emit();
        logger.info("test", "should skip").emit();
        logger.warn("test", "should write").emit();

        let entries = logger.read_entries(10, None, None);
        assert_eq!(entries.len(), 1);
    }
}
