use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use chrono::Utc;
use serde::{Deserialize, Serialize};

const MAX_LOG_SIZE: u64 = 5 * 1024 * 1024; // 5MB
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
        Self::open(&scope_root.join("logs"), "daemon.jsonl", Level::Info)
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
        let content = match fs::read_to_string(&self.path) {
            Ok(c) => c,
            Err(_) => return Vec::new(),
        };
        let mut entries: Vec<LogEntry> = content
            .lines()
            .rev()
            .filter_map(|line| serde_json::from_str::<LogEntry>(line).ok())
            .filter(|e| category.map_or(true, |c| e.cat == c))
            .filter(|e| level.map_or(true, |l| (e.level as u8) >= (l as u8)))
            .take(limit)
            .collect();
        entries.reverse();
        entries
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
    let repo_scope = project_root
        .file_name()?
        .to_str()?;
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
