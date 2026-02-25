use std::collections::{HashSet, VecDeque};
use std::path::PathBuf;
use std::process::Command as ProcessCommand;

use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

use crate::services::tui::app_event::AppEvent;
use crate::services::tui::model_profile::ModelProfile;
use crate::services::tui::task_snapshot::TaskSnapshot;

const HISTORY_LIMIT: usize = 300;

pub(crate) struct AppState {
    pub(crate) mcp_endpoint: String,
    pub(crate) mcp_agent_id: String,
    pub(crate) model_filter: Option<String>,
    pub(crate) tool_filter: Option<String>,
    pub(crate) profiles: Vec<ModelProfile>,
    pub(crate) selected_profile_idx: usize,
    pub(crate) prompt: String,
    pub(crate) status_line: String,
    pub(crate) history: VecDeque<String>,
    pub(crate) run_in_flight: bool,
    pub(crate) print_mode: bool,
    pub(crate) tasks: Vec<TaskSnapshot>,
    pub(crate) event_tx: UnboundedSender<AppEvent>,
    pub(crate) event_rx: UnboundedReceiver<AppEvent>,
}

impl AppState {
    pub(crate) fn discover_profiles_for_filters(
        model_filter: Option<&str>,
        tool_filter: Option<&str>,
    ) -> Vec<ModelProfile> {
        discover_profiles(model_filter, tool_filter)
    }

    pub(crate) fn new(
        mcp_endpoint: String,
        mcp_agent_id: String,
        model_filter: Option<String>,
        tool_filter: Option<String>,
        tasks: Vec<TaskSnapshot>,
        event_tx: UnboundedSender<AppEvent>,
        event_rx: UnboundedReceiver<AppEvent>,
    ) -> Self {
        let mut state = Self {
            mcp_endpoint,
            mcp_agent_id,
            model_filter,
            tool_filter,
            profiles: Vec::new(),
            selected_profile_idx: 0,
            prompt: String::new(),
            status_line: "Press Enter to run, p to toggle print mode, q to quit".to_string(),
            history: VecDeque::new(),
            run_in_flight: false,
            print_mode: true,
            tasks,
            event_tx,
            event_rx,
        };
        state.refresh_profiles();
        state
    }

    pub(crate) fn selected_profile(&self) -> Option<&ModelProfile> {
        self.profiles.get(self.selected_profile_idx)
    }

    pub(crate) fn move_selection_up(&mut self) {
        if self.selected_profile_idx > 0 {
            self.selected_profile_idx -= 1;
        }
    }

    pub(crate) fn move_selection_down(&mut self) {
        if self.selected_profile_idx + 1 < self.profiles.len() {
            self.selected_profile_idx += 1;
        }
    }

    pub(crate) fn append_prompt_char(&mut self, ch: char) {
        if !ch.is_control() {
            self.prompt.push(ch);
        }
    }

    pub(crate) fn pop_prompt_char(&mut self) {
        let _ = self.prompt.pop();
    }

    pub(crate) fn clear_prompt(&mut self) {
        self.prompt.clear();
    }

    pub(crate) fn take_prompt(&mut self) -> String {
        std::mem::take(&mut self.prompt)
    }

    pub(crate) fn refresh_profiles(&mut self) {
        let selected = self
            .profiles
            .get(self.selected_profile_idx)
            .map(|profile| (profile.tool.clone(), profile.model_id.clone()));
        self.profiles =
            discover_profiles(self.model_filter.as_deref(), self.tool_filter.as_deref());
        self.selected_profile_idx = selected
            .and_then(|(tool, model)| {
                self.profiles
                    .iter()
                    .position(|profile| profile.tool == tool && profile.model_id == model)
            })
            .unwrap_or_else(|| {
                self.profiles
                    .iter()
                    .position(ModelProfile::is_available)
                    .unwrap_or(0)
            });
    }

    pub(crate) fn set_tasks(&mut self, tasks: Vec<TaskSnapshot>) {
        self.tasks = tasks;
    }

    pub(crate) fn clear_history(&mut self) {
        self.history.clear();
    }

    pub(crate) fn history_lines(&self, max_lines: usize) -> Vec<String> {
        self.history
            .iter()
            .rev()
            .take(max_lines)
            .cloned()
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect()
    }

    pub(crate) fn drain_events(&mut self) {
        loop {
            match self.event_rx.try_recv() {
                Ok(AppEvent::AgentOutput { line, is_error }) => {
                    self.push_history(if is_error {
                        format!("stderr: {line}")
                    } else {
                        line
                    });
                }
                Ok(AppEvent::AgentFinished { summary, success }) => {
                    self.run_in_flight = false;
                    self.status_line = if success {
                        summary.clone()
                    } else {
                        format!("run failed: {summary}")
                    };
                    self.push_history(summary);
                }
                Err(tokio::sync::mpsc::error::TryRecvError::Empty) => break,
                Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => {
                    self.run_in_flight = false;
                    self.status_line = "agent event stream disconnected".to_string();
                    break;
                }
            }
        }
    }

    pub(crate) fn push_history(&mut self, line: String) {
        if self.history.len() >= HISTORY_LIMIT {
            let _ = self.history.pop_front();
        }
        self.history.push_back(line);
    }
}

fn discover_profiles(model_filter: Option<&str>, tool_filter: Option<&str>) -> Vec<ModelProfile> {
    let normalized_model_filter = model_filter
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let normalized_tool_filter = tool_filter
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(protocol::normalize_tool_id)
        .filter(|value| !value.is_empty());

    let mut profiles: Vec<ModelProfile> = ordered_default_model_specs()
        .into_iter()
        .filter(|(model_id, tool)| {
            if let Some(filter) = normalized_model_filter {
                if model_id != filter {
                    return false;
                }
            }
            if let Some(filter) = normalized_tool_filter.as_deref() {
                if tool != filter {
                    return false;
                }
            }
            true
        })
        .map(|(model_id, tool)| build_profile(&model_id, &tool))
        .collect();

    profiles.sort_by(|left, right| profile_sort_rank(left).cmp(&profile_sort_rank(right)));
    profiles
}

fn ordered_default_model_specs() -> Vec<(String, String)> {
    let defaults = protocol::default_model_specs();
    let mut seen_pairs: HashSet<(String, String)> = HashSet::new();
    let mut seen_tools: HashSet<String> = HashSet::new();
    let mut ordered = Vec::new();

    for (_, tool) in &defaults {
        if !seen_tools.insert(tool.clone()) {
            continue;
        }
        if let Some(default_model) = protocol::default_model_for_tool(tool) {
            let pair = (default_model.to_string(), tool.clone());
            if seen_pairs.insert(pair.clone()) {
                ordered.push(pair);
            }
        }
    }

    for (model_id, tool) in defaults {
        let pair = (model_id, tool);
        if seen_pairs.insert(pair.clone()) {
            ordered.push(pair);
        }
    }

    ordered
}

fn build_profile(model_id: &str, tool: &str) -> ModelProfile {
    if lookup_binary_in_path(tool).is_none() {
        return ModelProfile {
            model_id: model_id.to_string(),
            tool: tool.to_string(),
            availability: "missing_cli".to_string(),
            details: Some(format!("{tool} binary not found in PATH")),
        };
    }

    ModelProfile {
        model_id: model_id.to_string(),
        tool: tool.to_string(),
        availability: "available".to_string(),
        details: None,
    }
}

fn lookup_binary_in_path(binary_name: &str) -> Option<PathBuf> {
    #[cfg(unix)]
    {
        let output = ProcessCommand::new("which")
            .arg(binary_name)
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if path.is_empty() {
            None
        } else {
            Some(PathBuf::from(path))
        }
    }

    #[cfg(windows)]
    {
        let output = ProcessCommand::new("where")
            .arg(binary_name)
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        let first = String::from_utf8_lossy(&output.stdout)
            .lines()
            .next()
            .map(str::trim)
            .unwrap_or_default()
            .to_string();
        if first.is_empty() {
            None
        } else {
            Some(PathBuf::from(first))
        }
    }

    #[cfg(not(any(unix, windows)))]
    {
        let _ = binary_name;
        None
    }
}

fn profile_sort_rank(profile: &ModelProfile) -> u8 {
    match profile.availability.as_str() {
        "available" => 0,
        "missing_cli" => 1,
        _ => 2,
    }
}

#[cfg(test)]
mod tests {
    use super::ordered_default_model_specs;

    #[test]
    fn ordered_defaults_start_with_tool_default_model() {
        let defaults = ordered_default_model_specs();
        for tool in ["claude", "codex", "gemini", "opencode"] {
            let expected =
                protocol::default_model_for_tool(tool).expect("tool should have default");
            let first_for_tool = defaults
                .iter()
                .find_map(|(model_id, tool_id)| (tool_id == tool).then_some(model_id.as_str()))
                .expect("tool should be present");
            assert_eq!(first_for_tool, expected);
        }
    }

    #[test]
    fn ordered_defaults_do_not_duplicate_model_pairs() {
        let defaults = ordered_default_model_specs();
        let unique: std::collections::HashSet<_> = defaults.iter().cloned().collect();
        assert_eq!(unique.len(), defaults.len());
    }
}
