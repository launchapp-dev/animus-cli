use std::collections::{HashSet, VecDeque};

use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

use crate::services::tui::app_event::AppEvent;
use crate::services::tui::model_profile::ModelProfile;
use crate::services::tui::task_snapshot::TaskSnapshot;

const HISTORY_LIMIT: usize = 300;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FocusPane {
    Models,
    Tasks,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CreateTaskField {
    Title,
    Description,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ModalState {
    None,
    TaskDetail,
    StatusPicker { selected: usize },
    AssignInput { input: String },
    CreateTask { title_input: String, description_input: String, focused_field: CreateTaskField },
    DeleteTask { confirm: bool },
}

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
    pub(crate) focus: FocusPane,
    pub(crate) task_selected_idx: usize,
    pub(crate) modal: ModalState,
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
            status_line: "Tab=switch pane  Enter=detail/run  s=status  a=assign  c=create  d=delete  q=quit"
                .to_string(),
            history: VecDeque::new(),
            run_in_flight: false,
            print_mode: true,
            tasks,
            event_tx,
            event_rx,
            focus: FocusPane::Models,
            task_selected_idx: 0,
            modal: ModalState::None,
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

    pub(crate) fn cycle_focus(&mut self) {
        self.focus = match self.focus {
            FocusPane::Models => FocusPane::Tasks,
            FocusPane::Tasks => FocusPane::Models,
        };
    }

    pub(crate) fn task_move_up(&mut self) {
        if self.task_selected_idx > 0 {
            self.task_selected_idx -= 1;
        }
    }

    pub(crate) fn task_move_down(&mut self) {
        if !self.tasks.is_empty() && self.task_selected_idx + 1 < self.tasks.len() {
            self.task_selected_idx += 1;
        }
    }

    pub(crate) fn selected_task(&self) -> Option<&TaskSnapshot> {
        self.tasks.get(self.task_selected_idx)
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
        self.profiles = discover_profiles(self.model_filter.as_deref(), self.tool_filter.as_deref());
        self.selected_profile_idx = selected
            .and_then(|(tool, model)| {
                self.profiles.iter().position(|profile| profile.tool == tool && profile.model_id == model)
            })
            .unwrap_or_else(|| self.profiles.iter().position(ModelProfile::is_available).unwrap_or(0));
    }

    pub(crate) fn set_tasks(&mut self, tasks: Vec<TaskSnapshot>) {
        let len = tasks.len();
        self.tasks = tasks;
        if len == 0 {
            self.task_selected_idx = 0;
        } else if self.task_selected_idx >= len {
            self.task_selected_idx = len - 1;
        }
    }

    pub(crate) fn clear_history(&mut self) {
        self.history.clear();
    }

    pub(crate) fn history_lines(&self, max_lines: usize) -> Vec<String> {
        self.history.iter().rev().take(max_lines).cloned().collect::<Vec<_>>().into_iter().rev().collect()
    }

    pub(crate) fn drain_events(&mut self) {
        loop {
            match self.event_rx.try_recv() {
                Ok(AppEvent::AgentOutput { line, is_error }) => {
                    self.push_history(if is_error { format!("stderr: {line}") } else { line });
                }
                Ok(AppEvent::AgentFinished { summary, success }) => {
                    self.run_in_flight = false;
                    self.status_line = if success { summary.clone() } else { format!("run failed: {summary}") };
                    self.push_history(summary);
                }
                Ok(AppEvent::TasksRefreshed(tasks)) => {
                    let len = tasks.len();
                    self.tasks = tasks;
                    if len == 0 {
                        self.task_selected_idx = 0;
                    } else if self.task_selected_idx >= len {
                        self.task_selected_idx = len - 1;
                    }
                    self.status_line = format!("tasks refreshed ({len})");
                }
                Ok(AppEvent::TaskOpError(msg)) => {
                    self.status_line = format!("task op failed: {msg}");
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
    let normalized_model_filter = model_filter.map(str::trim).filter(|value| !value.is_empty());
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

    profiles.sort_by_key(profile_sort_rank);
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
    if cli_wrapper::lookup_binary_in_path(tool).is_none() {
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

fn profile_sort_rank(profile: &ModelProfile) -> u8 {
    match profile.availability.as_str() {
        "available" => 0,
        "missing_cli" => 1,
        _ => 2,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordered_defaults_start_with_tool_default_model() {
        let defaults = ordered_default_model_specs();
        for tool in ["claude", "codex", "gemini", "oai-runner"] {
            let expected = protocol::default_model_for_tool(tool).expect("tool should have default");
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

    #[test]
    fn focus_pane_cycle_toggles_between_models_and_tasks() {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let mut state = AppState::new("endpoint".to_string(), "agent".to_string(), None, None, vec![], tx, rx);
        assert_eq!(state.focus, FocusPane::Models);
        state.cycle_focus();
        assert_eq!(state.focus, FocusPane::Tasks);
        state.cycle_focus();
        assert_eq!(state.focus, FocusPane::Models);
    }

    #[test]
    fn task_navigation_moves_selection() {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let tasks = vec![
            TaskSnapshot {
                id: "TASK-001".to_string(),
                status: orchestrator_core::TaskStatus::Ready,
                title: "First".to_string(),
                description: String::new(),
                assignee_label: String::new(),
            },
            TaskSnapshot {
                id: "TASK-002".to_string(),
                status: orchestrator_core::TaskStatus::InProgress,
                title: "Second".to_string(),
                description: String::new(),
                assignee_label: String::new(),
            },
        ];
        let mut state = AppState::new("endpoint".to_string(), "agent".to_string(), None, None, tasks, tx, rx);
        assert_eq!(state.task_selected_idx, 0);
        state.task_move_down();
        assert_eq!(state.task_selected_idx, 1);
        state.task_move_down();
        assert_eq!(state.task_selected_idx, 1);
        state.task_move_up();
        assert_eq!(state.task_selected_idx, 0);
        state.task_move_up();
        assert_eq!(state.task_selected_idx, 0);
    }

    #[test]
    fn selected_task_returns_current_selection() {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let tasks = vec![
            TaskSnapshot {
                id: "TASK-001".to_string(),
                status: orchestrator_core::TaskStatus::Ready,
                title: "First".to_string(),
                description: String::new(),
                assignee_label: String::new(),
            },
            TaskSnapshot {
                id: "TASK-002".to_string(),
                status: orchestrator_core::TaskStatus::InProgress,
                title: "Second".to_string(),
                description: String::new(),
                assignee_label: String::new(),
            },
        ];
        let mut state = AppState::new("endpoint".to_string(), "agent".to_string(), None, None, tasks, tx, rx);
        assert_eq!(state.selected_task().map(|t| t.id.as_str()), Some("TASK-001"));
        state.task_move_down();
        assert_eq!(state.selected_task().map(|t| t.id.as_str()), Some("TASK-002"));
    }

    #[test]
    fn set_tasks_clamps_selection_to_valid_range() {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let mut state = AppState::new("endpoint".to_string(), "agent".to_string(), None, None, vec![], tx, rx);
        state.task_selected_idx = 5;
        state.set_tasks(vec![]);
        assert_eq!(state.task_selected_idx, 0);
        let tasks = vec![TaskSnapshot {
            id: "TASK-001".to_string(),
            status: orchestrator_core::TaskStatus::Ready,
            title: "Only".to_string(),
            description: String::new(),
            assignee_label: String::new(),
        }];
        state.set_tasks(tasks);
        assert_eq!(state.task_selected_idx, 0);
    }

    #[test]
    fn set_tasks_adjusts_selection_when_exceeds_length() {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let tasks = vec![
            TaskSnapshot {
                id: "TASK-001".to_string(),
                status: orchestrator_core::TaskStatus::Ready,
                title: "First".to_string(),
                description: String::new(),
                assignee_label: String::new(),
            },
            TaskSnapshot {
                id: "TASK-002".to_string(),
                status: orchestrator_core::TaskStatus::Ready,
                title: "Second".to_string(),
                description: String::new(),
                assignee_label: String::new(),
            },
        ];
        let mut state = AppState::new("endpoint".to_string(), "agent".to_string(), None, None, tasks, tx, rx);
        state.task_selected_idx = 1;
        state.set_tasks(vec![TaskSnapshot {
            id: "TASK-003".to_string(),
            status: orchestrator_core::TaskStatus::Ready,
            title: "Single".to_string(),
            description: String::new(),
            assignee_label: String::new(),
        }]);
        assert_eq!(state.task_selected_idx, 0);
    }

    #[test]
    fn modal_state_task_detail_displays_selected_task() {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let tasks = vec![TaskSnapshot {
            id: "TASK-001".to_string(),
            status: orchestrator_core::TaskStatus::Ready,
            title: "Test Task".to_string(),
            description: "A description".to_string(),
            assignee_label: "agent:dev".to_string(),
        }];
        let mut state = AppState::new("endpoint".to_string(), "agent".to_string(), None, None, tasks, tx, rx);
        state.modal = ModalState::TaskDetail;
        assert!(matches!(state.modal, ModalState::TaskDetail));
        assert_eq!(state.selected_task().unwrap().id, "TASK-001");
    }

    #[test]
    fn modal_state_status_picker_tracks_selection() {
        let modal = ModalState::StatusPicker { selected: 2 };
        if let ModalState::StatusPicker { selected } = modal {
            assert_eq!(selected, 2);
        } else {
            panic!("Expected StatusPicker modal");
        }
    }

    #[test]
    fn modal_state_assign_input_stores_input() {
        let modal = ModalState::AssignInput { input: "agent:reviewer".to_string() };
        if let ModalState::AssignInput { input } = modal {
            assert_eq!(input, "agent:reviewer");
        } else {
            panic!("Expected AssignInput modal");
        }
    }

    #[test]
    fn modal_state_create_task_has_focused_field() {
        let modal = ModalState::CreateTask {
            title_input: "New task".to_string(),
            description_input: "Description".to_string(),
            focused_field: CreateTaskField::Description,
        };
        if let ModalState::CreateTask { title_input, description_input, focused_field } = modal {
            assert_eq!(title_input, "New task");
            assert_eq!(description_input, "Description");
            assert_eq!(focused_field, CreateTaskField::Description);
        } else {
            panic!("Expected CreateTask modal");
        }
    }

    #[test]
    fn modal_state_delete_task_tracks_confirm() {
        let modal = ModalState::DeleteTask { confirm: true };
        if let ModalState::DeleteTask { confirm } = modal {
            assert!(confirm);
        } else {
            panic!("Expected DeleteTask modal");
        }
    }

    #[test]
    fn create_task_field_tab_order() {
        assert_eq!(CreateTaskField::Title, CreateTaskField::Title);
        assert_eq!(CreateTaskField::Description, CreateTaskField::Description);
        assert_ne!(CreateTaskField::Title, CreateTaskField::Description);
    }

    #[test]
    fn focus_pane_equality() {
        assert_eq!(FocusPane::Models, FocusPane::Models);
        assert_eq!(FocusPane::Tasks, FocusPane::Tasks);
        assert_ne!(FocusPane::Models, FocusPane::Tasks);
    }
}
