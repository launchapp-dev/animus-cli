use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use orchestrator_core::services::ServiceHub;
use orchestrator_core::TaskCreateInput;
use orchestrator_core::TaskStatus;
use ratatui::{backend::CrosstermBackend, Terminal};
use tokio::sync::mpsc::unbounded_channel;

use crate::services::tui::app_event::AppEvent;
use crate::services::tui::app_state::{AppState, CreateTaskField, FocusPane, ModalState};
use crate::services::tui::mcp_bridge::AoCliMcpBridge;
use crate::services::tui::render::render;
use crate::services::tui::run_agent::run_agent_session;
use crate::services::tui::task_snapshot::{TaskSnapshot, STATUS_CYCLE};
use crate::TuiArgs;

pub(crate) async fn handle_tui(args: TuiArgs, hub: Arc<dyn ServiceHub>, project_root: &str, json: bool) -> Result<()> {
    if json {
        return Err(anyhow!("`ao tui` does not support --json output"));
    }

    let model_filter = args.model;
    let tool_filter = args.tool.map(|value| value.to_ascii_lowercase());
    let headless = args.headless;
    let headless_prompt = args.prompt;

    let bridge = AoCliMcpBridge::start(project_root).await.context("failed to start AO CLI MCP bridge")?;
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
        return Err(anyhow!("selected profile {} [{}] is {}", profile.tool, profile.model_id, profile.availability));
    }

    let (event_tx, mut event_rx) = unbounded_channel();
    let project_root = project_root.to_string();
    let tool = profile.tool.clone();
    let model = profile.model_id.clone();
    let endpoint = mcp_endpoint.to_string();

    eprintln!("headless run: tool={} model={} mcp_endpoint={}", tool, model, endpoint);

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
            let _ = event_tx.send(AppEvent::AgentFinished { summary: error.to_string(), success: false });
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
            AppEvent::TasksRefreshed(_) | AppEvent::TaskOpError(_) => {}
        }
    }
    Ok(())
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

        let should_quit = match &app.modal.clone() {
            ModalState::None => handle_key_normal(app, hub, project_root, key).await?,
            ModalState::TaskDetail => {
                if matches!(key.code, KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q')) {
                    app.modal = ModalState::None;
                }
                false
            }
            ModalState::StatusPicker { selected } => {
                let selected = *selected;
                handle_key_status_picker(app, hub, key.code, selected).await;
                false
            }
            ModalState::AssignInput { .. } => {
                handle_key_assign_input(app, hub, key.code).await;
                false
            }
            ModalState::CreateTask { .. } => {
                handle_key_create_task(app, hub, key.code).await;
                false
            }
            ModalState::DeleteTask { .. } => {
                handle_key_delete_task(app, hub, key.code).await;
                false
            }
        };

        if should_quit {
            break;
        }
    }

    Ok(())
}

async fn handle_key_normal(
    app: &mut AppState,
    hub: &Arc<dyn ServiceHub>,
    project_root: &str,
    key: crossterm::event::KeyEvent,
) -> Result<bool> {
    match key.code {
        KeyCode::Char('q') => return Ok(true),
        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => return Ok(true),
        KeyCode::Tab | KeyCode::BackTab => app.cycle_focus(),
        KeyCode::Up | KeyCode::Char('k') => match app.focus {
            FocusPane::Models => app.move_selection_up(),
            FocusPane::Tasks => app.task_move_up(),
        },
        KeyCode::Down | KeyCode::Char('j') => match app.focus {
            FocusPane::Models => app.move_selection_down(),
            FocusPane::Tasks => app.task_move_down(),
        },
        KeyCode::Enter => match app.focus {
            FocusPane::Models => launch_selected_run(app, project_root),
            FocusPane::Tasks => {
                if app.selected_task().is_some() {
                    app.modal = ModalState::TaskDetail;
                }
            }
        },
        KeyCode::Char('s') if app.focus == FocusPane::Tasks => {
            if let Some(task) = app.selected_task() {
                let current_status = task.status;
                let current_pos = STATUS_CYCLE.iter().position(|s| *s == current_status).unwrap_or(0);
                app.modal = ModalState::StatusPicker { selected: current_pos };
            }
        }
        KeyCode::Char('a') if app.focus == FocusPane::Tasks => {
            if app.selected_task().is_some() {
                let current = app.selected_task().map(|t| t.assignee_label.clone()).unwrap_or_default();
                app.modal = ModalState::AssignInput { input: current };
            }
        }
        KeyCode::Char('c') if app.focus == FocusPane::Tasks => {
            app.modal = ModalState::CreateTask {
                title_input: String::new(),
                description_input: String::new(),
                focused_field: CreateTaskField::Title,
            };
        }
        KeyCode::Char('d') if app.focus == FocusPane::Tasks => {
            if app.selected_task().is_some() {
                app.modal = ModalState::DeleteTask { confirm: false };
            }
        }
        KeyCode::Backspace if app.focus == FocusPane::Models => app.pop_prompt_char(),
        KeyCode::Esc if app.focus == FocusPane::Models => app.clear_prompt(),
        KeyCode::Char('r') => {
            app.refresh_profiles();
            let tasks = load_task_snapshots(hub).await?;
            app.set_tasks(tasks);
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
        KeyCode::Char(ch) if app.focus == FocusPane::Models => app.append_prompt_char(ch),
        _ => {}
    }
    Ok(false)
}

async fn handle_key_status_picker(app: &mut AppState, hub: &Arc<dyn ServiceHub>, key_code: KeyCode, selected: usize) {
    match key_code {
        KeyCode::Esc => {
            app.modal = ModalState::None;
        }
        KeyCode::Up | KeyCode::Char('k') => {
            let new_sel = if selected > 0 { selected - 1 } else { STATUS_CYCLE.len() - 1 };
            app.modal = ModalState::StatusPicker { selected: new_sel };
        }
        KeyCode::Down | KeyCode::Char('j') => {
            let new_sel = (selected + 1) % STATUS_CYCLE.len();
            app.modal = ModalState::StatusPicker { selected: new_sel };
        }
        KeyCode::Enter => {
            if let Some(task) = app.selected_task().cloned() {
                let new_status: TaskStatus = STATUS_CYCLE[selected];
                app.modal = ModalState::None;
                app.status_line = format!("updating {} to {}...", task.id, new_status);
                let task_svc = hub.tasks();
                let tx = app.event_tx.clone();
                tokio::spawn(async move {
                    match task_svc.set_status(&task.id, new_status, true).await {
                        Ok(_) => match task_svc.list_prioritized().await {
                            Ok(tasks) => {
                                let snapshots: Vec<TaskSnapshot> =
                                    tasks.into_iter().take(24).map(TaskSnapshot::from_task).collect();
                                let _ = tx.send(AppEvent::TasksRefreshed(snapshots));
                            }
                            Err(e) => {
                                let _ = tx.send(AppEvent::TaskOpError(e.to_string()));
                            }
                        },
                        Err(e) => {
                            let _ = tx.send(AppEvent::TaskOpError(e.to_string()));
                        }
                    }
                });
            } else {
                app.modal = ModalState::None;
            }
        }
        _ => {}
    }
}

async fn handle_key_assign_input(app: &mut AppState, hub: &Arc<dyn ServiceHub>, key_code: KeyCode) {
    let input = match &app.modal {
        ModalState::AssignInput { input } => input.clone(),
        _ => return,
    };

    match key_code {
        KeyCode::Esc => {
            app.modal = ModalState::None;
        }
        KeyCode::Backspace => {
            let mut new_input = input;
            new_input.pop();
            app.modal = ModalState::AssignInput { input: new_input };
        }
        KeyCode::Enter => {
            if let Some(task) = app.selected_task().cloned() {
                let assignee_str = input.trim().to_string();
                app.modal = ModalState::None;
                if assignee_str.is_empty() {
                    app.status_line = "assign cancelled (empty input)".to_string();
                    return;
                }
                app.status_line = format!("assigning {} to {}...", task.id, assignee_str);
                let task_svc = hub.tasks();
                let tx = app.event_tx.clone();
                tokio::spawn(async move {
                    match task_svc.assign(&task.id, assignee_str).await {
                        Ok(_) => match task_svc.list_prioritized().await {
                            Ok(tasks) => {
                                let snapshots: Vec<TaskSnapshot> =
                                    tasks.into_iter().take(24).map(TaskSnapshot::from_task).collect();
                                let _ = tx.send(AppEvent::TasksRefreshed(snapshots));
                            }
                            Err(e) => {
                                let _ = tx.send(AppEvent::TaskOpError(e.to_string()));
                            }
                        },
                        Err(e) => {
                            let _ = tx.send(AppEvent::TaskOpError(e.to_string()));
                        }
                    }
                });
            } else {
                app.modal = ModalState::None;
            }
        }
        KeyCode::Char(ch) => {
            if !ch.is_control() {
                let mut new_input = input;
                new_input.push(ch);
                app.modal = ModalState::AssignInput { input: new_input };
            }
        }
        _ => {}
    }
}

async fn handle_key_create_task(app: &mut AppState, hub: &Arc<dyn ServiceHub>, key_code: KeyCode) {
    let (title_input, description_input, focused_field) = match &app.modal {
        ModalState::CreateTask { title_input, description_input, focused_field } => {
            (title_input.clone(), description_input.clone(), focused_field.clone())
        }
        _ => return,
    };

    match key_code {
        KeyCode::Esc => {
            app.modal = ModalState::None;
        }
        KeyCode::Tab => {
            let next_field = match focused_field {
                CreateTaskField::Title => CreateTaskField::Description,
                CreateTaskField::Description => CreateTaskField::Title,
            };
            app.modal = ModalState::CreateTask { title_input, description_input, focused_field: next_field };
        }
        KeyCode::Backspace => match focused_field {
            CreateTaskField::Title => {
                let mut new_title = title_input;
                new_title.pop();
                app.modal = ModalState::CreateTask {
                    title_input: new_title,
                    description_input,
                    focused_field: CreateTaskField::Title,
                };
            }
            CreateTaskField::Description => {
                let mut new_desc = description_input;
                new_desc.pop();
                app.modal = ModalState::CreateTask {
                    title_input,
                    description_input: new_desc,
                    focused_field: CreateTaskField::Description,
                };
            }
        },
        KeyCode::Enter => {
            let title = title_input.trim().to_string();
            let description = description_input.trim().to_string();
            app.modal = ModalState::None;
            if title.is_empty() {
                app.status_line = "create cancelled (empty title)".to_string();
                return;
            }
            app.status_line = format!("creating task '{title}'...");
            let task_svc = hub.tasks();
            let tx = app.event_tx.clone();
            tokio::spawn(async move {
                let input = TaskCreateInput {
                    title,
                    description,
                    task_type: None,
                    priority: None,
                    created_by: None,
                    tags: Vec::new(),
                    linked_requirements: Vec::new(),
                    linked_architecture_entities: Vec::new(),
                };
                match task_svc.create(input).await {
                    Ok(_) => match task_svc.list_prioritized().await {
                        Ok(tasks) => {
                            let snapshots: Vec<TaskSnapshot> =
                                tasks.into_iter().take(24).map(TaskSnapshot::from_task).collect();
                            let _ = tx.send(AppEvent::TasksRefreshed(snapshots));
                        }
                        Err(e) => {
                            let _ = tx.send(AppEvent::TaskOpError(e.to_string()));
                        }
                    },
                    Err(e) => {
                        let _ = tx.send(AppEvent::TaskOpError(e.to_string()));
                    }
                }
            });
        }
        KeyCode::Char(ch) => {
            if !ch.is_control() {
                match focused_field {
                    CreateTaskField::Title => {
                        app.modal = ModalState::CreateTask {
                            title_input: format!("{}{}", title_input, ch),
                            description_input,
                            focused_field: CreateTaskField::Title,
                        };
                    }
                    CreateTaskField::Description => {
                        app.modal = ModalState::CreateTask {
                            title_input,
                            description_input: format!("{}{}", description_input, ch),
                            focused_field: CreateTaskField::Description,
                        };
                    }
                }
            }
        }
        _ => {}
    }
}

async fn handle_key_delete_task(app: &mut AppState, hub: &Arc<dyn ServiceHub>, key_code: KeyCode) {
    let confirm = match &app.modal {
        ModalState::DeleteTask { confirm } => *confirm,
        _ => false,
    };

    match key_code {
        KeyCode::Esc => {
            app.modal = ModalState::None;
        }
        KeyCode::Char('y') | KeyCode::Enter if !confirm => {
            app.modal = ModalState::DeleteTask { confirm: true };
        }
        KeyCode::Char('y') | KeyCode::Enter if confirm => {
            if let Some(task) = app.selected_task().cloned() {
                app.modal = ModalState::None;
                app.status_line = format!("deleting {}...", task.id);
                let task_svc = hub.tasks();
                let tx = app.event_tx.clone();
                tokio::spawn(async move {
                    match task_svc.delete(&task.id).await {
                        Ok(_) => match task_svc.list_prioritized().await {
                            Ok(tasks) => {
                                let snapshots: Vec<TaskSnapshot> =
                                    tasks.into_iter().take(24).map(TaskSnapshot::from_task).collect();
                                let _ = tx.send(AppEvent::TasksRefreshed(snapshots));
                            }
                            Err(e) => {
                                let _ = tx.send(AppEvent::TaskOpError(e.to_string()));
                            }
                        },
                        Err(e) => {
                            let _ = tx.send(AppEvent::TaskOpError(e.to_string()));
                        }
                    }
                });
            } else {
                app.modal = ModalState::None;
            }
        }
        KeyCode::Char('n') | KeyCode::Char('q') => {
            app.modal = ModalState::None;
        }
        _ => {}
    }
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
        if print_mode { "print mode" } else { "summary mode" }
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
            let _ = event_tx.send(AppEvent::AgentFinished { summary: error.to_string(), success: false });
        }
    });
}

async fn load_task_snapshots(hub: &Arc<dyn ServiceHub>) -> Result<Vec<TaskSnapshot>> {
    let tasks = hub.tasks().list_prioritized().await.context("failed to load prioritized tasks for TUI")?;
    Ok(tasks.into_iter().take(24).map(TaskSnapshot::from_task).collect())
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
    execute!(terminal.backend_mut(), LeaveAlternateScreen).context("failed to leave alternate screen")?;
    terminal.show_cursor().context("failed to show terminal cursor")
}
