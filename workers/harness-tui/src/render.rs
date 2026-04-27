//! ratatui drawing layer. Pure function of `&App` — no side effects, no I/O.

use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};
use ratatui::Frame;

use crate::app::{App, MessageRole};
use crate::theme;

const MAX_INPUT_ROWS: u16 = 10;

/// Top-level draw entry point. Three rows + status bar layout.
pub fn draw(f: &mut Frame, app: &App) {
    let area = f.area();
    let input_inner_rows = app.editor.line_count().clamp(1, MAX_INPUT_ROWS as usize) as u16;
    let input_height = input_inner_rows + 2; // 2 for borders

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(1),
            Constraint::Length(input_height),
            Constraint::Length(1),
        ])
        .split(area);

    draw_header(f, chunks[0], app);
    draw_messages(f, chunks[1], app);
    draw_input(f, chunks[2], app);
    draw_status(f, chunks[3], app);

    if app.command_picker_visible {
        draw_command_picker(f, chunks[1], chunks[2], app);
    } else if app.file_picker_visible {
        draw_file_picker(f, chunks[1], chunks[2], app);
    }
}

fn draw_header(f: &mut Frame, area: Rect, app: &App) {
    let name = app
        .session_name
        .as_deref()
        .map_or(String::new(), |n| format!("[{n}] "));
    let text = format!(
        "harness  ·  {}{} {}  ·  cwd: {}",
        name, app.provider_name, app.model, app.cwd
    );
    let p = Paragraph::new(text).style(theme::header_style());
    f.render_widget(p, area);
}

fn draw_messages(f: &mut Frame, area: Rect, app: &App) {
    let lines: Vec<Line> = app
        .messages
        .iter()
        .map(|m| {
            let style = match m.role {
                MessageRole::User => theme::user_style(),
                MessageRole::Assistant => theme::assistant_style(),
                MessageRole::ToolResult => {
                    if m.text.contains("tool err") {
                        theme::tool_err_style()
                    } else if m.text.contains("tool ok") {
                        theme::tool_ok_style()
                    } else {
                        theme::tool_call_style()
                    }
                }
                MessageRole::Notification => theme::notification_style(),
            };
            Line::from(Span::styled(m.text.clone(), style))
        })
        .collect();

    let p = Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .scroll((app.scroll_offset, 0))
        .block(Block::default().borders(Borders::TOP | Borders::BOTTOM));
    f.render_widget(p, area);
}

fn draw_input(f: &mut Frame, area: Rect, app: &App) {
    let block = Block::default().borders(Borders::ALL).title("input");
    let inner_height = area.height.saturating_sub(2) as usize;
    let total = app.editor.line_count();
    let scroll_top = if total > inner_height {
        let cur = app.editor.cursor_row();
        (cur + 1).saturating_sub(inner_height)
    } else {
        0
    };

    let lines: Vec<Line> = app
        .editor
        .lines()
        .iter()
        .enumerate()
        .map(|(i, l)| {
            let prefix = if i == 0 { "> " } else { "  " };
            Line::from(format!("{prefix}{l}"))
        })
        .collect();

    let p = Paragraph::new(lines)
        .block(block)
        .style(Style::default())
        .scroll((scroll_top as u16, 0));
    f.render_widget(p, area);

    // Cursor: 2 chars for "> " or "  ", + display_cursor on current row;
    // +1 for left border, plus row offset accounting for scroll.
    let visible_row = app.editor.cursor_row().saturating_sub(scroll_top) as u16;
    let x = area.x + 1 + 2 + app.editor.display_cursor() as u16;
    let y = area.y + 1 + visible_row;
    f.set_cursor_position((
        x.min(area.x + area.width.saturating_sub(2)),
        y.min(area.y + area.height.saturating_sub(2)),
    ));
}

fn draw_status(f: &mut Frame, area: Rect, app: &App) {
    let p = Paragraph::new(app.status_line())
        .style(theme::status_style())
        .alignment(Alignment::Right);
    f.render_widget(p, area);
}

fn draw_command_picker(f: &mut Frame, msg_area: Rect, input_area: Rect, app: &App) {
    let entries = app.slash_registry.match_prefix(&app.command_picker_filter);
    if entries.is_empty() {
        return;
    }
    let count = entries.len().min(8) as u16;
    let height = count + 2;
    let width = msg_area.width.min(60);
    let x = msg_area.x;
    let y = input_area.y.saturating_sub(height);
    let area = Rect {
        x,
        y: y.max(msg_area.y),
        width,
        height,
    };
    let lines: Vec<Line> = entries
        .iter()
        .take(8)
        .enumerate()
        .map(|(i, e)| {
            let mark = if i == app.command_picker_index {
                "▶ "
            } else {
                "  "
            };
            let style = if i == app.command_picker_index {
                Style::default().add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };
            let label = if e.implemented {
                format!("{mark}/{}  — {}", e.name, e.description)
            } else {
                format!("{mark}/{}  — {} (planned)", e.name, e.description)
            };
            Line::from(Span::styled(label, style))
        })
        .collect();
    let p = Paragraph::new(lines).block(Block::default().borders(Borders::ALL).title("commands"));
    f.render_widget(Clear, area);
    f.render_widget(p, area);
}

fn draw_file_picker(f: &mut Frame, msg_area: Rect, input_area: Rect, app: &App) {
    let hits = match app.fuzzy_index.as_ref() {
        Some(idx) => idx.r#match(&app.file_picker_query, 8),
        None => return,
    };
    if hits.is_empty() {
        return;
    }
    let count = hits.len() as u16;
    let height = count + 2;
    let width = msg_area.width.min(80);
    let x = msg_area.x;
    let y = input_area.y.saturating_sub(height);
    let area = Rect {
        x,
        y: y.max(msg_area.y),
        width,
        height,
    };
    let lines: Vec<Line> = hits
        .iter()
        .enumerate()
        .map(|(i, (p, _))| {
            let mark = if i == app.file_picker_index {
                "▶ "
            } else {
                "  "
            };
            let style = if i == app.file_picker_index {
                Style::default().add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };
            Line::from(Span::styled(
                format!("{mark}{}", p.to_string_lossy()),
                style,
            ))
        })
        .collect();
    let p = Paragraph::new(lines).block(
        Block::default()
            .borders(Borders::ALL)
            .title(format!("files @{}", app.file_picker_query)),
    );
    f.render_widget(Clear, area);
    f.render_widget(p, area);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::App;
    use crate::sink::ChannelSink;
    use harness_types::AgentEvent;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use std::sync::Arc;

    struct Noop;
    impl crate::app::RuntimeHandle for Noop {
        fn enqueue_steering(&self, _: &str, _: harness_types::AgentMessage) {}
        fn enqueue_followup(&self, _: &str, _: harness_types::AgentMessage) {}
        fn abort(&self, _: &str) {}
    }

    #[test]
    fn draw_renders_header_and_status() {
        let backend = TestBackend::new(80, 12);
        let mut terminal = Terminal::new(backend).unwrap();
        let (sink, rx) = ChannelSink::new();
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            <ChannelSink as harness_runtime::EventSink>::emit(&sink, AgentEvent::AgentStart).await;
        });
        let mut app = App::new(
            "s1".into(),
            "anthropic".into(),
            "claude".into(),
            "/tmp".into(),
            rx,
            Arc::new(Noop),
        );
        app.drain_events();
        terminal.draw(|f| draw(f, &app)).unwrap();
        let buffer = terminal.backend().buffer().clone();
        let dump = buffer
            .content
            .iter()
            .map(|c| c.symbol())
            .collect::<String>();
        assert!(dump.contains("harness"));
        assert!(dump.contains("anthropic"));
        assert!(dump.contains("running") || dump.contains("idle"));
    }

    #[test]
    fn draw_shows_command_picker_when_visible() {
        let backend = TestBackend::new(80, 16);
        let mut terminal = Terminal::new(backend).unwrap();
        let (_sink, rx) = ChannelSink::new();
        let mut app = App::new(
            "s1".into(),
            "anthropic".into(),
            "claude".into(),
            "/tmp".into(),
            rx,
            Arc::new(Noop),
        );
        app.editor.set("/he");
        app.refresh_command_picker();
        terminal.draw(|f| draw(f, &app)).unwrap();
        let buffer = terminal.backend().buffer().clone();
        let dump = buffer
            .content
            .iter()
            .map(|c| c.symbol())
            .collect::<String>();
        assert!(dump.contains("/help"));
    }
}
