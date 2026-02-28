use std::collections::VecDeque;

use chrono::{DateTime, Utc};
use orchestrator_core::{OrchestratorWorkflow, WorkflowPhaseStatus, WorkflowStatus};

#[derive(Debug, Clone, Copy)]
#[allow(dead_code)]
pub(super) enum OutputStreamType {
    Stdout,
    Stderr,
    System,
}

pub(super) struct OutputLine {
    pub text: String,
    pub stream_type: OutputStreamType,
    pub is_json: bool,
}

pub(super) struct WorkflowMonitorState {
    pub workflows: Vec<OrchestratorWorkflow>,
    pub selected_idx: usize,
    pub output_buffer: VecDeque<OutputLine>,
    pub scroll_lock: bool,
    pub scroll_offset: usize,
    pub last_refresh: DateTime<Utc>,
    pub status_line: String,
    pub filter: String,
    pub filter_mode: bool,
    pub buffer_limit: usize,
}

impl WorkflowMonitorState {
    pub fn new(buffer_limit: usize) -> Self {
        Self {
            workflows: Vec::new(),
            selected_idx: 0,
            output_buffer: VecDeque::new(),
            scroll_lock: true,
            scroll_offset: 0,
            last_refresh: Utc::now(),
            status_line: "Loading workflows...".to_string(),
            filter: String::new(),
            filter_mode: false,
            buffer_limit,
        }
    }

    pub fn push_output(&mut self, text: String, stream_type: OutputStreamType) {
        if self.output_buffer.len() >= self.buffer_limit {
            self.output_buffer.pop_front();
        }
        let is_json = serde_json::from_str::<serde_json::Value>(&text).is_ok();
        self.output_buffer.push_back(OutputLine {
            text,
            stream_type,
            is_json,
        });
    }

    pub fn clear_output(&mut self) {
        self.output_buffer.clear();
        self.scroll_offset = 0;
    }

    pub fn filtered_workflows(&self) -> Vec<&OrchestratorWorkflow> {
        if self.filter.is_empty() {
            self.workflows.iter().collect()
        } else {
            let filter_lower = self.filter.to_ascii_lowercase();
            self.workflows
                .iter()
                .filter(|w| {
                    w.id.to_ascii_lowercase().contains(&filter_lower)
                        || w.task_id.to_ascii_lowercase().contains(&filter_lower)
                })
                .collect()
        }
    }

    pub fn selected_workflow(&self) -> Option<&OrchestratorWorkflow> {
        self.filtered_workflows().get(self.selected_idx).copied()
    }

    pub fn move_up(&mut self) {
        if self.selected_idx > 0 {
            self.selected_idx -= 1;
        }
    }

    pub fn move_down(&mut self) {
        let max = self.filtered_workflows().len().saturating_sub(1);
        if self.selected_idx < max {
            self.selected_idx += 1;
        }
    }

    pub fn clamp_selection(&mut self) {
        let len = self.filtered_workflows().len();
        if len == 0 {
            self.selected_idx = 0;
        } else if self.selected_idx >= len {
            self.selected_idx = len - 1;
        }
    }

    pub fn scroll_up(&mut self) {
        if self.scroll_offset > 0 {
            self.scroll_offset -= 1;
            self.scroll_lock = false;
        }
    }

    pub fn scroll_down(&mut self, max_lines: usize) {
        if self.scroll_offset + 1 < max_lines {
            self.scroll_offset += 1;
        }
    }
}

pub(super) fn workflow_status_icon(status: WorkflowStatus) -> &'static str {
    match status {
        WorkflowStatus::Pending => "○",
        WorkflowStatus::Running => "◐",
        WorkflowStatus::Paused => "⏸",
        WorkflowStatus::Completed => "●",
        WorkflowStatus::Failed => "✗",
        WorkflowStatus::Cancelled => "⊘",
    }
}

pub(super) fn phase_status_icon(status: WorkflowPhaseStatus) -> &'static str {
    match status {
        WorkflowPhaseStatus::Pending => "○",
        WorkflowPhaseStatus::Ready => "◌",
        WorkflowPhaseStatus::Running => "◐",
        WorkflowPhaseStatus::Success => "●",
        WorkflowPhaseStatus::Failed => "✗",
        WorkflowPhaseStatus::Skipped => "–",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use orchestrator_core::{
        OrchestratorWorkflow, WorkflowCheckpointMetadata, WorkflowMachineState,
        WorkflowPhaseExecution, WorkflowPhaseStatus, WorkflowStatus,
    };
    use std::collections::HashMap;

    fn make_workflow(id: &str, task_id: &str, status: WorkflowStatus) -> OrchestratorWorkflow {
        OrchestratorWorkflow {
            id: id.to_string(),
            task_id: task_id.to_string(),
            status,
            current_phase: None,
            phases: vec![],
            pipeline_id: None,
            current_phase_index: 0,
            machine_state: WorkflowMachineState::Idle,
            started_at: Utc::now(),
            completed_at: None,
            failure_reason: None,
            checkpoint_metadata: WorkflowCheckpointMetadata::default(),
            rework_counts: HashMap::new(),
            total_reworks: 0,
            decision_history: vec![],
        }
    }

    fn make_phase(phase_id: &str, status: WorkflowPhaseStatus) -> WorkflowPhaseExecution {
        WorkflowPhaseExecution {
            phase_id: phase_id.to_string(),
            status,
            started_at: None,
            completed_at: None,
            attempt: 1,
            error_message: None,
        }
    }

    #[test]
    fn state_initializes_with_defaults() {
        let state = WorkflowMonitorState::new(100);
        assert!(state.workflows.is_empty());
        assert_eq!(state.selected_idx, 0);
        assert!(state.output_buffer.is_empty());
        assert!(state.scroll_lock);
        assert_eq!(state.scroll_offset, 0);
        assert!(state.filter.is_empty());
        assert!(!state.filter_mode);
        assert_eq!(state.buffer_limit, 100);
    }

    #[test]
    fn push_output_respects_buffer_limit() {
        let mut state = WorkflowMonitorState::new(3);
        state.push_output("line1".to_string(), OutputStreamType::Stdout);
        state.push_output("line2".to_string(), OutputStreamType::Stdout);
        state.push_output("line3".to_string(), OutputStreamType::Stdout);
        assert_eq!(state.output_buffer.len(), 3);

        state.push_output("line4".to_string(), OutputStreamType::Stdout);
        assert_eq!(state.output_buffer.len(), 3);
        assert_eq!(state.output_buffer[0].text, "line2");
        assert_eq!(state.output_buffer[2].text, "line4");
    }

    #[test]
    fn push_output_detects_json() {
        let mut state = WorkflowMonitorState::new(10);
        state.push_output(r#"{"key": "value"}"#.to_string(), OutputStreamType::Stdout);
        state.push_output("plain text".to_string(), OutputStreamType::Stdout);

        assert!(state.output_buffer[0].is_json);
        assert!(!state.output_buffer[1].is_json);
    }

    #[test]
    fn clear_output_resets_buffer_and_scroll() {
        let mut state = WorkflowMonitorState::new(10);
        state.push_output("test".to_string(), OutputStreamType::Stdout);
        state.scroll_offset = 5;
        state.scroll_lock = false;

        state.clear_output();
        assert!(state.output_buffer.is_empty());
        assert_eq!(state.scroll_offset, 0);
    }

    #[test]
    fn filtered_workflows_matches_id_and_task_id() {
        let mut state = WorkflowMonitorState::new(10);
        state.workflows = vec![
            make_workflow("wf-abc", "TASK-001", WorkflowStatus::Running),
            make_workflow("wf-xyz", "TASK-002", WorkflowStatus::Completed),
        ];

        state.filter = "ABC".to_string();
        let filtered = state.filtered_workflows();
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].id, "wf-abc");

        state.filter = "task-002".to_string();
        let filtered = state.filtered_workflows();
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].task_id, "TASK-002");
    }

    #[test]
    fn move_up_down_clamps_to_bounds() {
        let mut state = WorkflowMonitorState::new(10);
        state.workflows = vec![
            make_workflow("wf-1", "TASK-001", WorkflowStatus::Running),
            make_workflow("wf-2", "TASK-002", WorkflowStatus::Running),
            make_workflow("wf-3", "TASK-003", WorkflowStatus::Running),
        ];

        assert_eq!(state.selected_idx, 0);
        state.move_up();
        assert_eq!(state.selected_idx, 0);

        state.move_down();
        state.move_down();
        assert_eq!(state.selected_idx, 2);

        state.move_down();
        assert_eq!(state.selected_idx, 2);

        state.move_up();
        assert_eq!(state.selected_idx, 1);
    }

    #[test]
    fn clamp_selection_adjusts_to_filter_change() {
        let mut state = WorkflowMonitorState::new(10);
        state.workflows = vec![
            make_workflow("wf-1", "TASK-001", WorkflowStatus::Running),
            make_workflow("wf-2", "TASK-002", WorkflowStatus::Running),
        ];
        state.selected_idx = 1;

        state.filter = "wf-1".to_string();
        state.clamp_selection();
        assert_eq!(state.selected_idx, 0);
    }

    #[test]
    fn scroll_up_disables_scroll_lock() {
        let mut state = WorkflowMonitorState::new(10);
        state.scroll_offset = 5;
        state.scroll_lock = true;

        state.scroll_up();
        assert_eq!(state.scroll_offset, 4);
        assert!(!state.scroll_lock);
    }

    #[test]
    fn scroll_down_clamps_to_max() {
        let mut state = WorkflowMonitorState::new(10);
        state.scroll_offset = 0;

        state.scroll_down(5);
        assert_eq!(state.scroll_offset, 1);

        state.scroll_offset = 4;
        state.scroll_down(5);
        assert_eq!(state.scroll_offset, 4);
    }

    #[test]
    fn selected_workflow_returns_filtered_item() {
        let mut state = WorkflowMonitorState::new(10);
        state.workflows = vec![
            make_workflow("wf-visible", "TASK-001", WorkflowStatus::Running),
            make_workflow("wf-hidden", "TASK-002", WorkflowStatus::Running),
        ];
        state.filter = "visible".to_string();
        state.selected_idx = 0;

        let selected = state.selected_workflow();
        assert!(selected.is_some());
        assert_eq!(selected.unwrap().id, "wf-visible");
    }

    #[test]
    fn workflow_status_icon_returns_correct_glyph() {
        assert_eq!(workflow_status_icon(WorkflowStatus::Pending), "○");
        assert_eq!(workflow_status_icon(WorkflowStatus::Running), "◐");
        assert_eq!(workflow_status_icon(WorkflowStatus::Paused), "⏸");
        assert_eq!(workflow_status_icon(WorkflowStatus::Completed), "●");
        assert_eq!(workflow_status_icon(WorkflowStatus::Failed), "✗");
        assert_eq!(workflow_status_icon(WorkflowStatus::Cancelled), "⊘");
    }

    #[test]
    fn phase_status_icon_returns_correct_glyph() {
        assert_eq!(phase_status_icon(WorkflowPhaseStatus::Pending), "○");
        assert_eq!(phase_status_icon(WorkflowPhaseStatus::Ready), "◌");
        assert_eq!(phase_status_icon(WorkflowPhaseStatus::Running), "◐");
        assert_eq!(phase_status_icon(WorkflowPhaseStatus::Success), "●");
        assert_eq!(phase_status_icon(WorkflowPhaseStatus::Failed), "✗");
        assert_eq!(phase_status_icon(WorkflowPhaseStatus::Skipped), "–");
    }

    #[test]
    fn workflows_with_phases_are_accessible() {
        let mut state = WorkflowMonitorState::new(10);
        let mut workflow = make_workflow("wf-with-phases", "TASK-001", WorkflowStatus::Running);
        workflow.phases = vec![
            make_phase("build", WorkflowPhaseStatus::Success),
            make_phase("test", WorkflowPhaseStatus::Running),
            make_phase("deploy", WorkflowPhaseStatus::Pending),
        ];
        workflow.current_phase = Some("test".to_string());
        workflow.current_phase_index = 1;
        state.workflows = vec![workflow];

        let selected = state.selected_workflow();
        assert!(selected.is_some());
        let wf = selected.unwrap();
        assert_eq!(wf.phases.len(), 3);
        assert_eq!(wf.phases[0].phase_id, "build");
        assert_eq!(wf.phases[1].status, WorkflowPhaseStatus::Running);
        assert_eq!(wf.current_phase.as_deref(), Some("test"));
    }
}
