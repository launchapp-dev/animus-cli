use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use orchestrator_core::services::ServiceHub;
use ratatui::{backend::CrosstermBackend, Terminal};
use tokio::sync::mpsc::unbounded_channel;

use crate::services::tui::app_event::AppEvent;
use crate::services::tui::app_state::AppState;
use crate::services::tui::mcp_bridge::AoCliMcpBridge;
use crate::services::tui::render::render;
use crate::services::tui::run_agent::run_agent_session;
use crate::services::tui::task_snapshot::TaskSnapshot;
use crate::TuiArgs;

pub(crate) async fn handle_tui(
    args: TuiArgs,
    hub: Arc<dyn ServiceHub>,
    project_root: &str,
    json: bool,
) -> Result<()> {
    if json {
        return Err(anyhow!("`ao tui` does not support --json output"));
    }

    let model_filter = args.model;
    let tool_filter = args.tool.map(|value| value.to_ascii_lowercase());
    let headless = args.headless;
    let headless_prompt = args.prompt;

    let bridge = AoCliMcpBridge::start(project_root)
        .await
        .context("failed to start AO CLI MCP bridge")?;
    if headless {
        let result = run_headless_mode(
            project_root,
            bridge.endpoint(),
            model_filter.as_deref(),
            tool_filter.as_deref(),
            headless_prompt,
        )
        .await;
        bridge.stop().await;
        return result;
    }

    let (event_tx, event_rx) = unbounded_channel();
    let initial_tasks = load_task_snapshots(&hub).await?;
    let mut app = AppState::new(
        bridge.endpoint().to_string(),
        "ao".to_string(),
        model_filter,
        tool_filter,
        initial_tasks,
        event_tx,
        event_rx,
    );

    app.push_history(format!("MCP locked to AO CLI via {}", app.mcp_endpoint));

    let mut terminal = initialize_terminal()?;
    let run_result = run_event_loop(&mut terminal, &mut app, &hub, project_root).await;
    let restore_result = restore_terminal(&mut terminal);
    bridge.stop().await;

    run_result?;
    restore_result?;
    Ok(())
}

async fn run_headless_mode(
    project_root: &str,
    mcp_endpoint: &str,
    model_filter: Option<&str>,
    tool_filter: Option<&str>,
    prompt: Option<String>,
) -> Result<()> {
    let prompt = prompt
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow!("`ao tui --headless` requires `--prompt`"))?;
    let profiles = AppState::discover_profiles_for_filters(model_filter, tool_filter);
    if profiles.is_empty() {
        return Err(anyhow!("no model profiles matched the provided filters"));
    }

    let profile = profiles
        .iter()
        .position(|candidate| candidate.is_available())
        .and_then(|index| profiles.get(index).cloned())
        .or_else(|| profiles.first().cloned())
        .ok_or_else(|| anyhow!("no model profile could be selected"))?;
    if !profile.is_available() {
        return Err(anyhow!(
            "selected profile {} [{}] is {}",
            profile.tool,
            profile.model_id,
            profile.availability
        ));
    }

    let (event_tx, mut event_rx) = unbounded_channel();
    let project_root = project_root.to_string();
    let tool = profile.tool.clone();
    let model = profile.model_id.clone();
    let endpoint = mcp_endpoint.to_string();

    eprintln!(
        "headless run: tool={} model={} mcp_endpoint={}",
        tool, model, endpoint
    );

    tokio::spawn(async move {
        let result = run_agent_session(
            project_root,
            tool,
            model,
            prompt,
            endpoint,
            "ao".to_string(),
            true,
            false,
            event_tx.clone(),
        )
        .await;
        if let Err(error) = result {
            let _ = event_tx.send(AppEvent::AgentFinished {
                summary: error.to_string(),
                success: false,
            });
        }
    });

    while let Some(event) = event_rx.recv().await {
        match event {
            AppEvent::AgentOutput { line, is_error } => {
                if is_error {
                    eprintln!("{line}");
                } else {
                    println!("{line}");
                }
            }
            AppEvent::AgentFinished { summary, success } => {
                if success {
                    eprintln!("{summary}");
                    return Ok(());
                }
                return Err(anyhow!(summary));
            }
        }
    }

    Err(anyhow!("headless run ended unexpectedly"))
}

async fn run_event_loop(
    terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>,
    app: &mut AppState,
    hub: &Arc<dyn ServiceHub>,
    project_root: &str,
) -> Result<()> {
    loop {
        app.drain_events();
        terminal.draw(|frame| render(frame, app))?;

        if !event::poll(Duration::from_millis(100))? {
            continue;
        }

        let Event::Key(key) = event::read()? else {
            continue;
        };
        if key.kind != KeyEventKind::Press {
            continue;
        }

        match key.code {
            KeyCode::Char('q') => break,
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => break,
            KeyCode::Up | KeyCode::Char('k') => app.move_selection_up(),
            KeyCode::Down | KeyCode::Char('j') => app.move_selection_down(),
            KeyCode::Backspace => app.pop_prompt_char(),
            KeyCode::Esc => app.clear_prompt(),
            KeyCode::Enter => launch_selected_run(app, project_root),
            KeyCode::Char('r') => {
                app.refresh_profiles();
                app.set_tasks(load_task_snapshots(hub).await?);
                app.status_line = "refreshed models and tasks".to_string();
            }
            KeyCode::Char('p') => {
                app.print_mode = !app.print_mode;
                app.status_line = if app.print_mode {
                    "print mode enabled (raw agent stream)".to_string()
                } else {
                    "print mode disabled (summarized events)".to_string()
                };
            }
            KeyCode::Char('l') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                app.clear_history();
                app.status_line = "output cleared".to_string();
            }
            KeyCode::Char(ch) => app.append_prompt_char(ch),
            _ => {}
        }
    }

    Ok(())
}

fn launch_selected_run(app: &mut AppState, project_root: &str) {
    if app.run_in_flight {
        app.status_line = "an agent run is already active".to_string();
        return;
    }

    let Some(profile) = app.selected_profile().cloned() else {
        app.status_line = "no model profile is available".to_string();
        return;
    };

    if !profile.is_available() {
        app.status_line = format!("selected profile is {}", profile.availability);
        return;
    }

    if app.prompt.trim().is_empty() {
        app.status_line = "prompt is empty".to_string();
        return;
    }

    let prompt = app.take_prompt();
    let event_tx = app.event_tx.clone();
    let project_root = project_root.to_string();
    let tool = profile.tool.clone();
    let model = profile.model_id.clone();
    let mcp_endpoint = app.mcp_endpoint.clone();
    let mcp_agent_id = app.mcp_agent_id.clone();
    let print_mode = app.print_mode;

    app.run_in_flight = true;
    app.status_line = format!(
        "running {} [{}] with MCP lock ({})",
        tool,
        model,
        if print_mode {
            "print mode"
        } else {
            "summary mode"
        }
    );
    app.push_history(format!("run started for {tool} [{model}]"));

    tokio::spawn(async move {
        let result = run_agent_session(
            project_root,
            tool,
            model,
            prompt,
            mcp_endpoint,
            mcp_agent_id,
            print_mode,
            true,
            event_tx.clone(),
        )
        .await;
        if let Err(error) = result {
            let _ = event_tx.send(AppEvent::AgentFinished {
                summary: error.to_string(),
                success: false,
            });
        }
    });
}

async fn load_task_snapshots(hub: &Arc<dyn ServiceHub>) -> Result<Vec<TaskSnapshot>> {
    let tasks = hub
        .tasks()
        .list_prioritized()
        .await
        .context("failed to load prioritized tasks for TUI")?;
    Ok(tasks
        .into_iter()
        .take(24)
        .map(TaskSnapshot::from_task)
        .collect())
}

fn initialize_terminal() -> Result<Terminal<CrosstermBackend<std::io::Stdout>>> {
    enable_raw_mode().context("failed to enable terminal raw mode")?;
    let mut stdout = std::io::stdout();
    execute!(stdout, EnterAlternateScreen).context("failed to enter alternate screen")?;
    let backend = CrosstermBackend::new(stdout);
    Terminal::new(backend).context("failed to create terminal backend")
}

fn restore_terminal(terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>) -> Result<()> {
    disable_raw_mode().context("failed to disable terminal raw mode")?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)
        .context("failed to leave alternate screen")?;
    terminal
        .show_cursor()
        .context("failed to show terminal cursor")
}
