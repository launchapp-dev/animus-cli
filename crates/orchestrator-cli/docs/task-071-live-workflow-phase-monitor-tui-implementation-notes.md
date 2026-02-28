# TASK-071 Implementation Notes

## Overview

This document details the implementation approach for adding a live workflow phase monitor with agent output streaming to the AO CLI TUI.

## Architecture

### Module Structure

```
crates/orchestrator-cli/src/services/tui/workflow_monitor/
├── mod.rs       - Public interface and entry point
├── state.rs      - WorkflowMonitorState struct
├── render.rs     - Split-pane rendering logic  
└── events.rs     - Event handling
```

### Entry Point

Add new command handler in `handle_tui.rs`:

```rust
pub(crate) async fn handle_workflow_monitor(
    hub: Arc<FileServiceHub>,
    terminal: Terminal<Backend>,
) -> Result<()>
```

## State Management

### WorkflowMonitorState

```rust
use std::collections::{HashMap, VecDeque};
use chrono::{DateTime, Utc};

pub(crate) struct OutputLine {
    pub text: String,
    pub stream_type: OutputStreamType,  // stdout, stderr, system
    pub timestamp: DateTime<Utc>,
    pub is_json: bool,
}

#[derive(Clone, Copy)]
pub(crate) enum OutputStreamType {
    Stdout,
    Stderr,
    System,
}

pub(crate) struct WorkflowMonitorState {
    pub workflows: Vec<OrchestratorWorkflow>,
    pub selected_workflow_idx: Option<usize>,
    pub selected_phase_idx: Option<usize>,
    pub output_buffer: VecDeque<OutputLine>,
    pub scroll_lock: bool,
    pub last_refresh: DateTime<Utc>,
    pub filter_text: String,
    pub active_agent_run_id: Option<String>,
}
```

## Workflow Data Flow

### Fetching Workflows

1. Use existing `hub.workflows().list()` API
2. Filter for active workflows (status != Completed/Cancelled/Failed)
3. Cache in state with timestamp
4. Refresh on interval (2 seconds)

### Phase Tree Structure

```rust
pub(crate) struct PhaseNode {
    pub workflow_id: String,
    pub workflow_title: String,
    pub phase_index: usize,
    pub phase_id: String,
    pub status: WorkflowPhaseStatus,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
    pub attempt: u32,
}
```

Build tree by iterating:
1. For each workflow in `hub.workflows().list()`
2. For each phase in `workflow.phases`
3. Create PhaseNode with status info

## Rendering Implementation

### Layout

Use ratatui's `Layout::split()` with:
- Direction::Horizontal
- Constraints: [Percentage(30), Percentage(70)]
- Minimum constraint: [Min(20), Min(20)]

### Left Pane: Phase Tree

Use `List` widget with custom item rendering:
- Indent based on depth (workflow = 0, phase = 1)
- Status icons: ○ pending, ◐ running, ● completed, ✗ failed, ■ blocked
- Selected item: highlighted with reverse style
- Show attempt count for reworked phases

### Right Pane: Output

Use `Paragraph` widget with:
- Custom text styling for JSON highlighting
- Color mapping:
  - stdout: White
  - stderr: Red
  - system: Yellow
  - JSON keys: Cyan
  - JSON strings: Green
  - JSON numbers: Magenta

### JSON Syntax Highlighting

```rust
fn highlight_json(text: &str) -> Vec<Span<'static>> {
    // Simple JSON highlighting using regex
    // Keys: cyan, strings: green, numbers: magenta, booleans: yellow
}
```

## Event Handling

### Input Events

- `KeyEvent::Up` / `j`: Move selection up in tree
- `KeyEvent::Down` / `k`: Move selection down in tree
- `KeyEvent::Enter`: Select focused workflow/phase, start output stream
- `KeyEvent::Char('l')` with Ctrl: Clear output buffer
- `KeyEvent::Char('r')`: Manual refresh
- `KeyEvent::Char('/')`: Enter filter mode
- `KeyEvent::Char('q')`: Quit monitor
- `KeyEvent::Char('s')`: Toggle scroll lock

### Timer Events

- Poll workflows every 2 seconds using `tokio::time::interval`
- Update UI on each poll

## Agent Output Streaming

### Connection

When user selects a running phase:
1. Find active agent for selected workflow/phase
2. Connect to agent stdout/stderr (similar to run_agent.rs)
3. Stream to output buffer

### Buffer Management

- Maximum buffer: 500 lines
- When full: remove oldest lines (FIFO)
- Clear on workflow/phase selection change

## CLI Integration

### New Subcommand

Add to `cli_types.rs`:

```rust
#[derive(Debug, Clone, Parser)]
#[command(name = "workflow-monitor", about = "Live workflow phase monitor with agent output streaming")]
pub struct WorkflowMonitorArgs {
    #[arg(long, default_value = "2")]
    pub refresh_interval: u64,
    
    #[arg(long, default_value = "500")]
    pub buffer_lines: usize,
    
    #[arg(long)]
    pub workflow_id: Option<String>,
}
```

### Main.rs Integration

Wire up command in main.rs dispatch:

```rust
"workflow-monitor" => {
    handle_workflow_monitor(hub, args).await?
}
```

## Error Handling

### Workflow Fetch Failure

- Log error
- Show "Unable to fetch workflows" in UI
- Retry on next interval
- Don't clear existing data

### Agent Stream Failure

- Show "Agent disconnected" message
- Attempt reconnect every 5 seconds
- Allow manual retry with 'r' key

## Testing Approach

1. Unit tests for state management
2. Integration tests for workflow fetching
3. Manual testing with running daemon

## Related Files

- `crates/orchestrator-cli/src/services/tui/mod.rs` - Add module
- `crates/orchestrator-cli/src/services/tui/app_state.rs` - Reference patterns
- `crates/orchestrator-cli/src/services/tui/render.rs` - Rendering patterns
- `crates/orchestrator-cli/src/services/tui/run_agent.rs` - Output streaming patterns
- `crates/orchestrator-core/src/services/workflow_impl.rs` - Workflow service
- `crates/orchestrator-core/src/types.rs` - Workflow types
