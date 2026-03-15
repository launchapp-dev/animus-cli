use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap},
    Frame,
};

use crate::services::tui::app_state::{AppState, CreateTaskField, FocusPane, ModalState};
use crate::services::tui::task_snapshot::STATUS_CYCLE;

pub(crate) fn render(frame: &mut Frame<'_>, app: &AppState) {
    let root = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(10), Constraint::Length(5)])
        .split(frame.area());

    let selected = app
        .selected_profile()
        .map(|profile| format!("{} / {}", profile.tool, profile.model_id))
        .unwrap_or_else(|| "none".to_string());
    let header = Paragraph::new(format!(
        "AO Agent Console (MCP locked to ao mcp serve)\nMCP endpoint: {}\nSelected: {}",
        app.mcp_endpoint, selected
    ))
    .block(Block::default().borders(Borders::ALL).title("Session"));
    frame.render_widget(header, root[0]);

    let body = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(30), Constraint::Percentage(40), Constraint::Percentage(30)])
        .split(root[1]);

    let models_focused = app.focus == FocusPane::Models;
    let model_items: Vec<ListItem<'_>> = app
        .profiles
        .iter()
        .enumerate()
        .map(|(index, profile)| {
            let marker = if index == app.selected_profile_idx { ">" } else { " " };
            let detail = profile.details.as_deref().map(|value| format!(" ({value})")).unwrap_or_default();
            ListItem::new(format!("{marker} {}{detail}", profile.label()))
        })
        .collect();
    let models_title = if models_focused { "Models [FOCUS] (j/k)" } else { "Models (Tab to focus)" };
    let model_list = List::new(model_items).block(
        Block::default().borders(Borders::ALL).title(models_title).border_style(if models_focused {
            Style::default().fg(Color::Cyan)
        } else {
            Style::default()
        }),
    );
    frame.render_widget(model_list, body[0]);

    let output_lines = app.history_lines(120).into_iter().map(ListItem::new).collect::<Vec<_>>();
    let output_list = List::new(output_lines).block(Block::default().borders(Borders::ALL).title("Agent Output"));
    frame.render_widget(output_list, body[1]);

    let tasks_focused = app.focus == FocusPane::Tasks;
    let task_items: Vec<ListItem<'_>> = app.tasks.iter().map(|task| ListItem::new(task.label())).collect();
    let tasks_title = if tasks_focused {
        "Tasks [FOCUS] (j/k s=status a=assign c=create d=delete Enter=detail)"
    } else {
        "Tasks (Tab to focus)"
    };
    let task_list = List::new(task_items)
        .block(Block::default().borders(Borders::ALL).title(tasks_title).border_style(if tasks_focused {
            Style::default().fg(Color::Yellow)
        } else {
            Style::default()
        }))
        .highlight_style(Style::default().bg(Color::DarkGray).add_modifier(Modifier::BOLD))
        .highlight_symbol("» ");

    let mut task_list_state = ListState::default();
    task_list_state.select(Some(app.task_selected_idx));
    frame.render_stateful_widget(task_list, body[2], &mut task_list_state);

    let footer_text = if app.focus == FocusPane::Tasks {
        format!(
            "Status: {}\nMode: {}\nPrompt: {}\nTab=switch  j/k=nav  Enter=detail  s=status  a=assign  c=create  d=delete  r=refresh  Ctrl+L=clear  q=quit",
            app.status_line,
            if app.print_mode { "print/raw" } else { "summary" },
            app.prompt
        )
    } else {
        format!(
            "Status: {}\nMode: {}\nPrompt: {}\nTab=switch  Enter=run  j/k=model  p=toggle print  Backspace=edit  Esc=clear  r=refresh  Ctrl+L=clear  q=quit",
            app.status_line,
            if app.print_mode { "print/raw" } else { "summary" },
            app.prompt
        )
    };
    let footer = Paragraph::new(footer_text)
        .block(Block::default().borders(Borders::ALL).title("Controls"))
        .wrap(Wrap { trim: false });
    frame.render_widget(footer, root[2]);

    render_modal(frame, app);
}

fn centered_rect(width: u16, height: u16, area: Rect) -> Rect {
    let x = area.x + area.width.saturating_sub(width) / 2;
    let y = area.y + area.height.saturating_sub(height) / 2;
    Rect { x, y, width: width.min(area.width), height: height.min(area.height) }
}

fn render_modal(frame: &mut Frame<'_>, app: &AppState) {
    match &app.modal {
        ModalState::None => {}

        ModalState::TaskDetail => {
            if let Some(task) = app.selected_task() {
                let area = centered_rect(70, 14, frame.area());
                frame.render_widget(Clear, area);
                let assignee_display = if task.assignee_label.is_empty() { "unassigned" } else { &task.assignee_label };
                let text = format!(
                    "ID:       {}\nStatus:   {}\nAssignee: {}\n\n{}",
                    task.id,
                    task.status_label(),
                    assignee_display,
                    task.description
                );
                let modal = Paragraph::new(text)
                    .block(
                        Block::default()
                            .borders(Borders::ALL)
                            .title(format!(" {} ", task.title))
                            .border_style(Style::default().fg(Color::Cyan)),
                    )
                    .wrap(Wrap { trim: false });
                frame.render_widget(modal, area);
                let hint_area = Rect {
                    x: area.x + 1,
                    y: area.y + area.height.saturating_sub(1),
                    width: area.width.saturating_sub(2),
                    height: 1,
                };
                frame.render_widget(
                    Paragraph::new(" Esc/Enter/q = close ").style(Style::default().fg(Color::DarkGray)),
                    hint_area,
                );
            }
        }

        ModalState::StatusPicker { selected } => {
            if let Some(task) = app.selected_task() {
                let height = STATUS_CYCLE.len() as u16 + 4;
                let area = centered_rect(40, height, frame.area());
                frame.render_widget(Clear, area);
                let items: Vec<ListItem<'_>> = STATUS_CYCLE
                    .iter()
                    .enumerate()
                    .map(|(i, s)| {
                        let lbl = s.to_string();
                        if i == *selected {
                            ListItem::new(format!("» {lbl}"))
                                .style(Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD))
                        } else {
                            ListItem::new(format!("  {lbl}"))
                        }
                    })
                    .collect();
                let list = List::new(items).block(
                    Block::default()
                        .borders(Borders::ALL)
                        .title(format!(" Status: {} ", task.id))
                        .border_style(Style::default().fg(Color::Yellow)),
                );
                frame.render_widget(list, area);
                let hint_area = Rect {
                    x: area.x + 1,
                    y: area.y + area.height.saturating_sub(1),
                    width: area.width.saturating_sub(2),
                    height: 1,
                };
                frame.render_widget(
                    Paragraph::new(" j/k=nav  Enter=apply  Esc=cancel ").style(Style::default().fg(Color::DarkGray)),
                    hint_area,
                );
            }
        }

        ModalState::AssignInput { input } => {
            if let Some(task) = app.selected_task() {
                let area = centered_rect(50, 5, frame.area());
                frame.render_widget(Clear, area);
                let modal = Paragraph::new(format!("Assignee: {input}_"))
                    .block(
                        Block::default()
                            .borders(Borders::ALL)
                            .title(format!(" Assign: {} ", task.id))
                            .border_style(Style::default().fg(Color::Green)),
                    )
                    .wrap(Wrap { trim: false });
                frame.render_widget(modal, area);
                let hint_area = Rect {
                    x: area.x + 1,
                    y: area.y + area.height.saturating_sub(1),
                    width: area.width.saturating_sub(2),
                    height: 1,
                };
                frame.render_widget(
                    Paragraph::new(" Enter=confirm  Esc=cancel ").style(Style::default().fg(Color::DarkGray)),
                    hint_area,
                );
            }
        }

        ModalState::CreateTask { title_input, description_input, focused_field } => {
            let area = centered_rect(60, 7, frame.area());
            frame.render_widget(Clear, area);
            let title_cursor = if *focused_field == CreateTaskField::Title { "_" } else { "" };
            let desc_cursor = if *focused_field == CreateTaskField::Description { "_" } else { "" };
            let title_focus_marker = if *focused_field == CreateTaskField::Title { " [*]" } else { "" };
            let desc_focus_marker = if *focused_field == CreateTaskField::Description { " [*]" } else { "" };
            let modal = Paragraph::new(format!(
                "Title{}:       {}{}\nDescription{}: {}{}",
                title_focus_marker, title_input, title_cursor, desc_focus_marker, description_input, desc_cursor,
            ))
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(" Create New Task ")
                    .border_style(Style::default().fg(Color::Magenta)),
            )
            .wrap(Wrap { trim: false });
            frame.render_widget(modal, area);
            let hint_area = Rect {
                x: area.x + 1,
                y: area.y + area.height.saturating_sub(1),
                width: area.width.saturating_sub(2),
                height: 1,
            };
            frame.render_widget(
                Paragraph::new(" Tab=switch field  Enter=create  Esc=cancel ")
                    .style(Style::default().fg(Color::DarkGray)),
                hint_area,
            );
        }

        ModalState::DeleteTask { confirm } => {
            if let Some(task) = app.selected_task() {
                let area = centered_rect(50, 5, frame.area());
                frame.render_widget(Clear, area);
                let (text, border_color) = if *confirm {
                    (format!("Delete task {}?\nPress y/Enter to confirm, n/q to cancel", task.id), Color::Red)
                } else {
                    (format!("Delete task {}?\nPress y/Enter to confirm, n/q to cancel", task.id), Color::Yellow)
                };
                let modal = Paragraph::new(text)
                    .block(
                        Block::default()
                            .borders(Borders::ALL)
                            .title(format!(" Delete {} ", task.id))
                            .border_style(Style::default().fg(border_color)),
                    )
                    .wrap(Wrap { trim: false });
                frame.render_widget(modal, area);
                let hint_area = Rect {
                    x: area.x + 1,
                    y: area.y + area.height.saturating_sub(1),
                    width: area.width.saturating_sub(2),
                    height: 1,
                };
                frame.render_widget(
                    Paragraph::new(" y/Enter=confirm  n/q=cancel ").style(Style::default().fg(Color::DarkGray)),
                    hint_area,
                );
            }
        }
    }
}
