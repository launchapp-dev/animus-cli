mod render;
mod state;

use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use orchestrator_core::services::ServiceHub;
use ratatui::{backend::CrosstermBackend, Terminal};

use crate::WorkflowMonitorArgs;
use state::{OutputStreamType, WorkflowMonitorState};

pub(crate) async fn handle_workflow_monitor(
    args: WorkflowMonitorArgs,
    hub: Arc<dyn ServiceHub>,
    json: bool,
) -> Result<()> {
    if json {
        return Err(anyhow!("`ao workflow-monitor` does not support --json output"));
    }

    let refresh_interval = Duration::from_secs(args.refresh_interval);
    let mut state = WorkflowMonitorState::new(args.buffer_lines);

    match hub.workflows().list().await {
        Ok(workflows) => {
            state.workflows = if let Some(ref id) = args.workflow_id {
                workflows.into_iter().filter(|w| &w.id == id).collect()
            } else {
                workflows
            };
            state.status_line = format!("{} workflow(s) loaded", state.workflows.len());
        }
        Err(e) => {
            state.status_line = format!("Failed to load workflows: {e}");
        }
    }

    enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let run_result = run_event_loop(
        &mut terminal,
        &mut state,
        &hub,
        refresh_interval,
        args.workflow_id.as_deref(),
    )
    .await;

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    run_result
}

async fn run_event_loop(
    terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>,
    state: &mut WorkflowMonitorState,
    hub: &Arc<dyn ServiceHub>,
    refresh_interval: Duration,
    workflow_id_filter: Option<&str>,
) -> Result<()> {
    let mut last_refresh = Instant::now();

    loop {
        terminal.draw(|frame| render::render(frame, state))?;

        if event::poll(Duration::from_millis(100))? {
            let Event::Key(key) = event::read()? else {
                continue;
            };
            if key.kind != KeyEventKind::Press {
                continue;
            }

            if state.filter_mode {
                match key.code {
                    KeyCode::Esc => {
                        state.filter_mode = false;
                    }
                    KeyCode::Enter => {
                        state.filter_mode = false;
                        state.clamp_selection();
                    }
                    KeyCode::Backspace => {
                        state.filter.pop();
                        state.clamp_selection();
                    }
                    KeyCode::Char(ch) => {
                        state.filter.push(ch);
                        state.clamp_selection();
                    }
                    _ => {}
                }
            } else {
                match key.code {
                    KeyCode::Char('q') => break,
                    KeyCode::Char('c')
                        if key.modifiers.contains(KeyModifiers::CONTROL) =>
                    {
                        break;
                    }
                    KeyCode::Up | KeyCode::Char('k') => {
                        state.move_up();
                        state.clear_output();
                        state.push_output(
                            "[ selection changed ]".to_string(),
                            OutputStreamType::System,
                        );
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        state.move_down();
                        state.clear_output();
                        state.push_output(
                            "[ selection changed ]".to_string(),
                            OutputStreamType::System,
                        );
                    }
                    KeyCode::Enter => {
                        let snapshot = state.selected_workflow().map(|wf| {
                            let id = wf.id.clone();
                            let status = wf.status;
                            let phase_count = wf.phases.len();
                            let current_phase = wf.current_phase.clone();
                            let phases: Vec<_> = wf
                                .phases
                                .iter()
                                .map(|p| {
                                    (
                                        p.phase_id.clone(),
                                        p.status,
                                        p.attempt,
                                        p.error_message.clone(),
                                    )
                                })
                                .collect();
                            (id, status, phase_count, current_phase, phases)
                        });
                        if let Some((id, status, phase_count, current_phase, phases)) = snapshot {
                            let msg = format!(
                                "[ Workflow {id} | status={status:?} | phases={phase_count} | current={} ]",
                                current_phase.as_deref().unwrap_or("none")
                            );
                            state.push_output(msg, OutputStreamType::System);
                            for (phase_id, phase_status, attempt, error_message) in phases {
                                let phase_msg = format!(
                                    "  phase={phase_id} status={phase_status:?} attempt={attempt}{}",
                                    error_message
                                        .map(|e| format!(" error={e}"))
                                        .unwrap_or_default(),
                                );
                                state.push_output(phase_msg, OutputStreamType::System);
                            }
                        }
                    }
                    KeyCode::Char('l')
                        if key.modifiers.contains(KeyModifiers::CONTROL) =>
                    {
                        state.clear_output();
                    }
                    KeyCode::Char('r') => {
                        refresh_workflows(state, hub, workflow_id_filter).await;
                        last_refresh = Instant::now();
                    }
                    KeyCode::Char('/') => {
                        state.filter_mode = true;
                    }
                    KeyCode::Char('s') => {
                        state.scroll_lock = !state.scroll_lock;
                    }
                    KeyCode::PageUp => {
                        for _ in 0..10 {
                            state.scroll_up();
                        }
                    }
                    KeyCode::PageDown => {
                        let max = state.output_buffer.len();
                        for _ in 0..10 {
                            state.scroll_down(max);
                        }
                    }
                    _ => {}
                }
            }
        }

        if last_refresh.elapsed() >= refresh_interval {
            refresh_workflows(state, hub, workflow_id_filter).await;
            last_refresh = Instant::now();
        }
    }

    Ok(())
}

async fn refresh_workflows(
    state: &mut WorkflowMonitorState,
    hub: &Arc<dyn ServiceHub>,
    workflow_id_filter: Option<&str>,
) {
    match hub.workflows().list().await {
        Ok(workflows) => {
            state.workflows = if let Some(id) = workflow_id_filter {
                workflows.into_iter().filter(|w| w.id == id).collect()
            } else {
                workflows
            };
            state.last_refresh = chrono::Utc::now();
            state.clamp_selection();
            state.status_line = format!("{} workflow(s)", state.workflows.len());
        }
        Err(e) => {
            state.status_line = format!("Refresh failed: {e}");
            state.push_output(
                format!("[ Workflow refresh failed: {e} ]"),
                OutputStreamType::System,
            );
        }
    }
}
