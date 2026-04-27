//! ratatui drawing layer. Pure function of `&App` — no side effects, no I/O.

use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use ratatui::Frame;

use crate::app::{App, MessageRole};
use crate::theme;

/// Top-level draw entry point. Three rows + status bar layout.
pub fn draw(f: &mut Frame, app: &App) {
    let area = f.area();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(1),
            Constraint::Length(3),
            Constraint::Length(1),
        ])
        .split(area);

    draw_header(f, chunks[0], app);
    draw_messages(f, chunks[1], app);
    draw_input(f, chunks[2], app);
    draw_status(f, chunks[3], app);
}

fn draw_header(f: &mut Frame, area: Rect, app: &App) {
    let text = format!(
        "harness  ·  {} {}  ·  cwd: {}",
        app.provider_name, app.model, app.cwd
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
    let prompt = format!("> {}", app.editor.text());
    let p = Paragraph::new(prompt).block(block).style(Style::default());
    f.render_widget(p, area);
    // cursor: 1 char for "> ", + display_cursor; +1 for left border, +1 for top border row offset
    let x = area.x + 3 + app.editor.display_cursor() as u16;
    let y = area.y + 1;
    f.set_cursor_position((x.min(area.x + area.width.saturating_sub(2)), y));
}

fn draw_status(f: &mut Frame, area: Rect, app: &App) {
    let p = Paragraph::new(app.status_line())
        .style(theme::status_style())
        .alignment(Alignment::Right);
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
        // Push an event into the channel and drain into the app.
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
}
