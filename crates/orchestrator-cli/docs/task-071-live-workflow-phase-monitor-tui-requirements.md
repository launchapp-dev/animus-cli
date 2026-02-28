# TASK-071: Live Workflow Phase Monitor with Agent Output Streaming to TUI

## Overview

Add a live workflow phase monitor with agent output streaming to the AO CLI TUI. Display active workflow phases as a tree pane with real-time progress, and stream agent output in adjacent pane with JSON syntax highlighting. Split view: phase tree left, raw output right.

## Problem Statement

Currently, the AO CLI TUI (`crates/orchestrator-cli/src/services/tui/`) only supports running single agent sessions with a model selection interface. Operators need visibility into:
1. Active workflow phases across all workflows
2. Real-time progress of each phase
3. Live agent output streaming with JSON syntax highlighting

This is essential for observability while agents run in the daemon.

## Requirements

### Functional Requirements

1. **Workflow Phase Tree Display (Left Pane)**
   - Display all active workflows as a tree structure using `hub.workflows().list()`
   - Show phase hierarchy with status indicators (pending, running, completed, failed, blocked)
   - Display current phase progress with visual indicators
   - Auto-refresh at configurable interval (default: 2 seconds)
   - Support selection and focus on specific workflow/phase

2. **Agent Output Streaming (Right Pane)**
   - Stream agent stdout/stderr in real-time
   - JSON syntax highlighting for structured output
   - Color-coded output: stdout (white), stderr (red), system (yellow)
   - Auto-scroll to bottom with manual scroll lock
   - Buffer last N lines (configurable, default: 500)

3. **Split View Layout**
   - Resizable split: phase tree (default 30%), output (default 70%)
   - Minimum pane widths: 20% each
   - Keyboard shortcuts for pane navigation

4. **Integration Points**
   - Use `hub.workflows()` for workflow/phase data
   - Use existing event streaming from `daemon_events.rs`
   - Connect to agent run output via `ao agent run` stdout/stderr

### User Interactions

- `j/k` or `Up/Down`: Navigate phase tree
- `Enter`: Select workflow/phase to view its output
- `Ctrl+L`: Clear output pane
- `q`: Quit monitor mode
- `r`: Refresh workflows
- `/`: Filter workflows by name

### Data Handling

- Workflow data: poll `hub.workflows().list()` every 2 seconds
- Output streaming: async read from agent stdout/stderr pipes
- State: maintain in-memory with bounded history

### Edge Cases

- No active workflows: display "No active workflows" message
- Workflow completes while viewing: mark as completed, retain in tree
- Agent crashes: display error message with reason
- Terminal resize: recalculate pane dimensions

## Technical Implementation Notes

### Architecture

1. **New Module**: `crates/orchestrator-cli/src/services/tui/workflow_monitor/`
   - `mod.rs`: Public interface
   - `state.rs`: WorkflowMonitorState struct
   - `render.rs`: Split-pane rendering
   - `events.rs`: Event handling

2. **State Structure**:
```rust
pub(crate) struct WorkflowMonitorState {
    pub workflows: Vec<OrchestratorWorkflow>,
    pub selected_workflow_idx: Option<usize>,
    pub output_buffer: VecDeque<OutputLine>,
    pub scroll_lock: bool,
    pub last_refresh: DateTime<Utc>,
}
```

3. **Rendering**: Extend existing TUI using ratatui with:
   - `Split` layout for panes
   - `Tree` widget for phase hierarchy
   - Custom styling for JSON highlighting

### Dependencies

- No new crate dependencies required
- Uses existing: ratatui, tokio, orchestrator-core

### File Changes

- `crates/orchestrator-cli/src/services/tui/mod.rs`: Add workflow_monitor module
- `crates/orchestrator-cli/src/services/tui/workflow_monitor/`: New directory with module files
- `crates/orchestrator-cli/src/cli_types.rs`: Add `workflow-monitor` subcommand
- `crates/orchestrator-cli/src/main.rs`: Wire up new command

## Acceptance Criteria

1. **Display Active Workflows**: Shows all workflows from `hub.workflows().list()` in tree format
2. **Phase Status Indicators**: Visual indicators for each phase status (pending, running, completed, failed)
3. **Real-time Updates**: Workflow tree refreshes every 2 seconds showing current state
4. **Output Streaming**: Agent stdout/stderr appears in right pane within 500ms of emission
5. **JSON Highlighting**: Structured JSON output is syntax-highlighted
6. **Keyboard Navigation**: All specified keyboard shortcuts functional
7. **Responsive Layout**: Panes resize correctly on terminal resize
8. **Empty State**: Shows appropriate message when no active workflows
9. **Error Handling**: Gracefully handles workflow fetch failures with retry
