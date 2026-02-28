# TASK-073 Requirements: Command Palette and Vim-Style Navigation for TUI

## Phase
- Workflow phase: `requirements`
- Workflow ID: `82a34b02-4db4-4d7b-b6d7-a1aa8bbde947`
- Task: `TASK-073`
- Snapshot date: `2026-02-28`

## Objective
Add power-user navigation features to the AO TUI:
- Ctrl+K command palette for quick actions
- Vim-style navigation (H/J/K/L, gg/G, /search, ?help)

## Current Baseline Audit

| Surface | Current location | Current behavior | Gap |
| --- | --- | --- | --- |
| TUI entry point | `services/tui/handle_tui.rs` | Basic event loop with keyboard handling | No command palette, limited vim keys |
| Keyboard handling | `handle_tui.rs:183-210` | q, Ctrl+C, j/k, Enter, Backspace, Esc, r, p, Ctrl+L | Missing: H/J/K/L pane nav, gg/G jump, /search, ?help |
| App state | `services/tui/app_state.rs` | Tracks selected model, prompt, tasks, history | No active pane tracking, no search mode |
| Render | `services/tui/render.rs` | Three-pane layout: Models, Agent Output, Tasks | No visual indicator of active pane, no command palette overlay |

## Scope
In scope for implementation:
- Add command palette (Ctrl+K) with actions: create task, run agent, view workflow, toggle daemon
- Add vim-style pane navigation: H (left), J (down), K (up), L (right)
- Add gg (jump to top), G (jump to bottom) in lists
- Add /pattern search in lists with Enter to execute, Esc to cancel
- Add ? help overlay showing all keyboard shortcuts
- Visual indicator for active pane (highlighted border or color)
- Command palette fuzzy search and action execution

Out of scope:
- Mouse/touch support
- Custom keybinding configuration
- Macros/recording
- Multiple command palette categories beyond quick actions

## Constraints
- Determinism: navigation state must be consistent across renders
- Safety: no data loss when switching panes or executing palette actions
- Compatibility: existing keyboard shortcuts remain functional
- Repository safety: implementation in Rust TUI crate only
- Performance: command palette search must complete in <50ms

## Response Contract

### Command Palette
- Trigger: Ctrl+K from any state
- Display: Modal overlay with search input and action list
- Actions available:
  - `Create Task` - opens task creation flow
  - `Run Agent` - runs agent with current prompt
  - `View Workflow` - shows workflow status
  - `Toggle Daemon` - starts/stops daemon
- Selection: arrow keys or j/k, Enter to execute, Esc to cancel
- Search: fuzzy matching on action names

### Vim Navigation Modes
| Key | Action |
| --- | --- |
| h | Move focus to left pane (Models) |
| l | Move focus to right pane (Tasks) |
| j | Move selection down in active pane |
| k | Move selection up in active pane |
| gg | Jump to first item in list |
| G | Jump to last item in list |
| / | Enter search mode ( Esc to exit) |
| ? | Show help overlay |

### Active Pane Indicator
- Border highlight color change (e.g., cyan vs white)
- Footer shows current pane name
- Pane context: Models, Output, or Tasks

## Functional Requirements

### FR-01: Command Palette
- Ctrl+K opens modal overlay from any TUI state
- Search input filters available actions
- Enter executes selected action
- Esc closes palette without action

### FR-02: Pane Navigation
- H/J/K/L keys switch active pane or navigate within pane
- Active pane persists until changed
- Selection state preserved per pane

### FR-03: List Navigation
- gg jumps to first item
- G jumps to last item
- j/k move selection with boundary wrapping

### FR-04: Search Mode
- / enters search mode in active pane
- Input field captures search pattern
- Enter applies filter, Esc cancels
- Search applies to visible list content

### FR-05: Help Overlay
- ? shows full keyboard shortcut overlay
- Esc closes help
- Help lists all available shortcuts

### FR-06: Visual Feedback
- Active pane has distinct border/background
- Status line shows current mode (normal/search/help/palette)

## Acceptance Criteria
- AC-01: Ctrl+K opens command palette with searchable actions
- AC-02: H/J/K/L navigates between panes and within lists
- AC-03: gg/G jumps to list top/bottom
- AC-04: / enters search mode, filters list, Esc cancels
- AC-05: ? shows help overlay with all shortcuts
- AC-06: Active pane has visible indicator
- AC-07: All existing shortcuts continue to work
- AC-08: Command palette actions execute correctly

## Testable Acceptance Checklist
- T-01: Verify Ctrl+K opens palette from all TUI states
- T-02: Verify H moves focus to Models pane
- T-03: Verify L moves focus to Tasks pane
- T-04: Verify j/k scrolls in active pane
- T-05: Verify gg jumps to first item
- T-06: Verify G jumps to last item
- T-07: Verify / enters search, Enter applies, Esc cancels
- T-08: Verify ? shows help, Esc closes
- T-09: Verify palette action execution works
- T-10: Verify visual pane indicator updates correctly

## Implementation Notes Input (Next Phase)
Primary target:
- `crates/orchestrator-cli/src/services/tui/handle_tui.rs` - keyboard handling
- `crates/orchestrator-cli/src/services/tui/app_state.rs` - pane state, search mode
- `crates/orchestrator-cli/src/services/tui/render.rs` - overlays, pane highlights

Likely touched areas:
- Add `active_pane`, `search_mode`, `command_palette_open`, `help_visible` to AppState
- Add keyboard handlers for new shortcuts in run_event_loop
- Add render logic for command palette overlay
- Add render logic for help overlay
- Add search filtering logic to pane data

## Deterministic Deliverables for Implementation Phase
- Command palette opens on Ctrl+K with searchable actions
- Vim navigation keys work correctly (H/J/K/L, gg/G)
- Search mode filters lists with /
- Help overlay shows all shortcuts on ?
- Active pane visually indicated
- All existing functionality preserved
