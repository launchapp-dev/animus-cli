use ratatui::{
    layout::{Constraint, Direction, Layout},
    widgets::{Block, Borders, List, ListItem, Paragraph, Wrap},
    Frame,
};

use crate::services::tui::app_state::AppState;

pub(crate) fn render(frame: &mut Frame<'_>, app: &AppState) {
    let root = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(10),
            Constraint::Length(5),
        ])
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
        .constraints([
            Constraint::Percentage(30),
            Constraint::Percentage(40),
            Constraint::Percentage(30),
        ])
        .split(root[1]);

    let model_items: Vec<ListItem<'_>> = app
        .profiles
        .iter()
        .enumerate()
        .map(|(index, profile)| {
            let marker = if index == app.selected_profile_idx {
                ">"
            } else {
                " "
            };
            let detail = profile
                .details
                .as_deref()
                .map(|value| format!(" ({value})"))
                .unwrap_or_default();
            ListItem::new(format!("{marker} {}{detail}", profile.label()))
        })
        .collect();
    let model_list =
        List::new(model_items).block(Block::default().borders(Borders::ALL).title("Models (j/k)"));
    frame.render_widget(model_list, body[0]);

    let output_lines = app
        .history_lines(120)
        .into_iter()
        .map(ListItem::new)
        .collect::<Vec<_>>();
    let output_list =
        List::new(output_lines).block(Block::default().borders(Borders::ALL).title("Agent Output"));
    frame.render_widget(output_list, body[1]);

    let task_items = app
        .tasks
        .iter()
        .map(|task| ListItem::new(task.label()))
        .collect::<Vec<_>>();
    let task_list = List::new(task_items).block(
        Block::default()
            .borders(Borders::ALL)
            .title("Prioritized Tasks"),
    );
    frame.render_widget(task_list, body[2]);

    let footer = Paragraph::new(format!(
        "Status: {}\nMode: {}\nPrompt: {}\nEnter=run  p=toggle print mode  Backspace=edit  Esc=clear  r=refresh  Ctrl+L=clear output  q=quit",
        app.status_line,
        if app.print_mode { "print/raw" } else { "summary" },
        app.prompt
    ))
    .block(Block::default().borders(Borders::ALL).title("Controls"))
    .wrap(Wrap { trim: false });
    frame.render_widget(footer, root[2]);
}
