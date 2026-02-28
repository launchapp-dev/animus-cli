use chrono::Utc;
use orchestrator_core::{WorkflowPhaseStatus, WorkflowStatus};
use ratatui::{
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, List, ListItem, Paragraph},
    Frame,
};

use super::state::{
    phase_status_icon, workflow_status_icon, OutputLine, OutputStreamType, WorkflowMonitorState,
};

pub(super) fn render(frame: &mut Frame<'_>, state: &WorkflowMonitorState) {
    let root = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(2)])
        .split(frame.area());

    let body = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(30), Constraint::Percentage(70)])
        .split(root[0]);

    render_tree(frame, state, body[0]);
    render_output(frame, state, body[1]);
    render_footer(frame, state, root[1]);
}

fn render_tree(frame: &mut Frame<'_>, state: &WorkflowMonitorState, area: ratatui::layout::Rect) {
    let workflows = state.filtered_workflows();

    let items: Vec<ListItem<'_>> = if workflows.is_empty() {
        vec![ListItem::new(Line::from(Span::styled(
            "No workflows found",
            Style::default().fg(Color::DarkGray),
        )))]
    } else {
        let mut items = Vec::new();
        for (i, workflow) in workflows.iter().enumerate() {
            let status_icon = workflow_status_icon(workflow.status);
            let selected_marker = if i == state.selected_idx { "▶" } else { " " };

            let wf_style = if i == state.selected_idx {
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD)
            } else {
                match workflow.status {
                    WorkflowStatus::Running => Style::default().fg(Color::Cyan),
                    WorkflowStatus::Completed => Style::default().fg(Color::Green),
                    WorkflowStatus::Failed => Style::default().fg(Color::Red),
                    WorkflowStatus::Paused => Style::default().fg(Color::Magenta),
                    WorkflowStatus::Cancelled => Style::default().fg(Color::DarkGray),
                    WorkflowStatus::Pending => Style::default().fg(Color::White),
                }
            };
            let wf_label = format!(
                "{selected_marker} {status_icon} {} [{}]",
                truncate(&workflow.id, 18),
                truncate(&workflow.task_id, 10),
            );
            items.push(ListItem::new(Line::from(Span::styled(wf_label, wf_style))));

            for phase in &workflow.phases {
                let phase_icon = phase_status_icon(phase.status);
                let attempt_suffix = if phase.attempt > 1 {
                    format!(" ×{}", phase.attempt)
                } else {
                    String::new()
                };
                let phase_style = match phase.status {
                    WorkflowPhaseStatus::Running => Style::default().fg(Color::Cyan),
                    WorkflowPhaseStatus::Success => Style::default().fg(Color::Green),
                    WorkflowPhaseStatus::Failed => Style::default().fg(Color::Red),
                    WorkflowPhaseStatus::Skipped => Style::default().fg(Color::DarkGray),
                    WorkflowPhaseStatus::Ready => Style::default().fg(Color::Blue),
                    WorkflowPhaseStatus::Pending => Style::default().fg(Color::Gray),
                };
                let phase_label = format!(
                    "  {phase_icon} {}{}",
                    truncate(&phase.phase_id, 22),
                    attempt_suffix
                );
                items.push(ListItem::new(Line::from(Span::styled(
                    phase_label,
                    phase_style,
                ))));
            }
        }
        items
    };

    let filter_suffix = if !state.filter.is_empty() {
        format!(" [filter: {}]", state.filter)
    } else {
        String::new()
    };
    let title = format!("Workflows{filter_suffix} (j/k r / q)");
    let list = List::new(items).block(Block::default().borders(Borders::ALL).title(title));
    frame.render_widget(list, area);
}

fn render_output(frame: &mut Frame<'_>, state: &WorkflowMonitorState, area: ratatui::layout::Rect) {
    let visible_height = area.height.saturating_sub(2) as usize;
    let total = state.output_buffer.len();

    let start = if state.scroll_lock {
        total.saturating_sub(visible_height)
    } else {
        state
            .scroll_offset
            .min(total.saturating_sub(visible_height))
    };

    let lines: Vec<Line<'_>> = state
        .output_buffer
        .iter()
        .skip(start)
        .take(visible_height)
        .map(format_output_line)
        .collect();

    let text = Text::from(lines);
    let scroll_indicator = if state.scroll_lock {
        "[auto-scroll]"
    } else {
        "[scroll-locked]"
    };
    let para = Paragraph::new(text).block(
        Block::default()
            .borders(Borders::ALL)
            .title(format!("Output {scroll_indicator} (Ctrl+L=clear s=toggle)")),
    );
    frame.render_widget(para, area);
}

fn render_footer(frame: &mut Frame<'_>, state: &WorkflowMonitorState, area: ratatui::layout::Rect) {
    let secs_ago = (Utc::now() - state.last_refresh).num_seconds();
    let filter_hint = if state.filter_mode {
        format!("  Filter: {}|", state.filter)
    } else {
        String::new()
    };
    let help = format!(
        "q=quit  j/k=navigate  r=refresh  /=filter  s=scroll  Ctrl+L=clear  Enter=inspect | {} ({secs_ago}s ago){}",
        state.status_line,
        filter_hint,
    );
    let para = Paragraph::new(help).style(Style::default().fg(Color::DarkGray));
    frame.render_widget(para, area);
}

fn format_output_line(line: &OutputLine) -> Line<'_> {
    let base_color = match line.stream_type {
        OutputStreamType::Stderr => Color::Red,
        OutputStreamType::System => Color::Yellow,
        OutputStreamType::Stdout => Color::White,
    };

    if line.is_json {
        Line::from(highlight_json_line(&line.text))
    } else {
        Line::from(Span::styled(
            line.text.as_str(),
            Style::default().fg(base_color),
        ))
    }
}

fn highlight_json_line(text: &str) -> Vec<Span<'_>> {
    let mut spans = Vec::new();
    let mut pos = 0;

    while pos < text.len() {
        let ch = match text[pos..].chars().next() {
            Some(c) => c,
            None => break,
        };
        let ch_len = ch.len_utf8();

        match ch {
            '"' => {
                let start = pos;
                pos += 1;
                let mut escaped = false;
                while pos < text.len() {
                    let c = match text[pos..].chars().next() {
                        Some(c) => c,
                        None => break,
                    };
                    let c_len = c.len_utf8();
                    if escaped {
                        escaped = false;
                    } else if c == '\\' {
                        escaped = true;
                    } else if c == '"' {
                        pos += c_len;
                        break;
                    }
                    pos += c_len;
                }
                let token = &text[start..pos.min(text.len())];
                let after = text[pos..].trim_start_matches([' ', '\t', '\r', '\n']);
                let color = if after.starts_with(':') {
                    Color::Cyan
                } else {
                    Color::Green
                };
                spans.push(Span::styled(token, Style::default().fg(color)));
            }
            c if c.is_ascii_digit()
                || (c == '-'
                    && text
                        .as_bytes()
                        .get(pos + 1)
                        .map_or(false, |b| b.is_ascii_digit())) =>
            {
                let start = pos;
                pos += ch_len;
                while pos < text.len() {
                    let nc = match text[pos..].chars().next() {
                        Some(c) => c,
                        None => break,
                    };
                    if nc.is_ascii_digit()
                        || nc == '.'
                        || nc == 'e'
                        || nc == 'E'
                        || nc == '+'
                        || nc == '-'
                    {
                        pos += nc.len_utf8();
                    } else {
                        break;
                    }
                }
                spans.push(Span::styled(
                    &text[start..pos],
                    Style::default().fg(Color::Magenta),
                ));
            }
            _ if text[pos..].starts_with("true") => {
                spans.push(Span::styled(
                    &text[pos..pos + 4],
                    Style::default().fg(Color::Yellow),
                ));
                pos += 4;
            }
            _ if text[pos..].starts_with("false") => {
                spans.push(Span::styled(
                    &text[pos..pos + 5],
                    Style::default().fg(Color::Yellow),
                ));
                pos += 5;
            }
            _ if text[pos..].starts_with("null") => {
                spans.push(Span::styled(
                    &text[pos..pos + 4],
                    Style::default().fg(Color::DarkGray),
                ));
                pos += 4;
            }
            _ => {
                spans.push(Span::styled(
                    &text[pos..pos + ch_len],
                    Style::default().fg(Color::Gray),
                ));
                pos += ch_len;
            }
        }
    }

    if spans.is_empty() {
        spans.push(Span::raw(text));
    }
    spans
}

fn truncate(s: &str, max_chars: usize) -> &str {
    match s.char_indices().nth(max_chars) {
        Some((i, _)) => &s[..i],
        None => s,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_short_string_returns_unchanged() {
        assert_eq!(truncate("short", 10), "short");
    }

    #[test]
    fn truncate_exact_length_returns_unchanged() {
        assert_eq!(truncate("exactly5", 8), "exactly5");
    }

    #[test]
    fn truncate_long_string_cuts_at_char_boundary() {
        assert_eq!(truncate("this is a long string", 7), "this is");
    }

    #[test]
    fn truncate_handles_unicode() {
        assert_eq!(truncate("日本語テスト", 3), "日本語");
        assert_eq!(truncate("🦀🦀🦀🦀", 2), "🦀🦀");
    }

    #[test]
    fn truncate_empty_string_returns_empty() {
        assert_eq!(truncate("", 5), "");
    }

    #[test]
    fn highlight_json_line_handles_empty() {
        let spans = highlight_json_line("");
        assert!(spans.is_empty() || (spans.len() == 1 && spans[0].content == ""));
    }

    #[test]
    fn highlight_json_line_string_key() {
        let spans = highlight_json_line(r#""key": "value""#);
        assert!(!spans.is_empty());
    }

    #[test]
    fn highlight_json_line_numbers() {
        let spans = highlight_json_line(r#"{"count": 42, "ratio": 3.14}"#);
        assert!(!spans.is_empty());
    }

    #[test]
    fn highlight_json_line_booleans_and_null() {
        let spans = highlight_json_line(r#"{"active": true, "disabled": false, "empty": null}"#);
        assert!(!spans.is_empty());
    }

    #[test]
    fn highlight_json_line_negative_numbers() {
        let spans = highlight_json_line(r#"{"temp": -10, "delta": -2.5}"#);
        assert!(!spans.is_empty());
    }

    #[test]
    fn highlight_json_line_scientific_notation() {
        let spans = highlight_json_line(r#"{"big": 1.5e10, "small": 2e-3}"#);
        assert!(!spans.is_empty());
    }

    #[test]
    fn highlight_json_line_escaped_strings() {
        let spans = highlight_json_line(r#"{"msg": "hello \"world\""}"#);
        assert!(!spans.is_empty());
    }
}
