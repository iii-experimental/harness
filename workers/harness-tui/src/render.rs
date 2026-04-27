//! ratatui drawing layer. Pure function of `&App` — no side effects, no I/O.

use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};
use ratatui::Frame;

use crate::app::{App, AppStatus, MessageRole, RenderedMessage};
use crate::keybindings::Keybinding;
use crate::markdown;
use crate::theme;

const MAX_INPUT_ROWS: u16 = 10;

/// Top-level draw entry point. Header + scrollback + widget area + queue
/// indicator + input + status bar layout. Overlays paint on top.
pub fn draw(f: &mut Frame, app: &App) {
    let area = f.area();
    let input_inner_rows = app.editor.line_count().clamp(1, MAX_INPUT_ROWS as usize) as u16;
    let input_height = input_inner_rows + 2; // 2 for borders
    let widget_height = app.slots.widget_height();

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(0),
            Constraint::Length(widget_height),
            Constraint::Length(1),
            Constraint::Length(input_height),
            Constraint::Length(1),
        ])
        .split(area);

    draw_header(f, chunks[0], app);
    draw_messages(f, chunks[1], app);
    draw_widgets(f, chunks[2], app);
    draw_queue_indicator(f, chunks[3], app);
    draw_input(f, chunks[4], app);
    draw_status(f, chunks[5], app);

    if app.command_picker_visible {
        draw_command_picker(f, chunks[1], chunks[4], app);
    } else if app.file_picker_visible {
        draw_file_picker(f, chunks[1], chunks[4], app);
    }

    // Fullscreen overlays sit above everything, including the pickers.
    if app.tree_visible {
        draw_tree_overlay(f, area, app);
    } else if app.hotkeys_visible {
        draw_hotkeys_overlay(f, area, app);
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
    let md_theme = markdown::Theme::from_palette();
    let mut lines: Vec<Line<'static>> = Vec::with_capacity(app.messages.len() * 2);

    for m in &app.messages {
        match m.role {
            MessageRole::User => {
                lines.push(Line::from(Span::styled(
                    m.text.clone(),
                    theme::user_style(),
                )));
            }
            MessageRole::Assistant => {
                let parsed = markdown::parse_to_lines(&m.text, &md_theme);
                lines.extend(parsed);
            }
            MessageRole::Thinking => {
                if app.expand_thinking {
                    let header = Line::from(Span::styled(
                        "[thinking]".to_string(),
                        theme::thinking_style(),
                    ));
                    lines.push(header);
                    for raw in m.text.lines() {
                        lines.push(Line::from(Span::styled(
                            format!("  {raw}"),
                            theme::thinking_style(),
                        )));
                    }
                } else {
                    let label = match m.thinking_token_count {
                        Some(n) => format!("[thinking ~{n} tokens — Ctrl+T expand]"),
                        None => "[thinking — Ctrl+T expand]".to_string(),
                    };
                    lines.push(Line::from(Span::styled(label, theme::thinking_style())));
                }
            }
            MessageRole::ToolResult => {
                lines.extend(render_tool_result_lines(m, app));
            }
            MessageRole::Notification => {
                lines.push(Line::from(Span::styled(
                    m.text.clone(),
                    theme::notification_style(),
                )));
            }
        }
        for line in render_image_placeholders(m) {
            lines.push(line);
        }
    }

    let p = Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .scroll((app.scroll_offset, 0))
        .block(Block::default().borders(Borders::TOP | Borders::BOTTOM));
    f.render_widget(p, area);
}

fn render_tool_result_lines(m: &crate::app::RenderedMessage, app: &App) -> Vec<Line<'static>> {
    // Tool-call args header lines: "      -> tool call: name(args)" — render
    // as plain dim line, no expansion behaviour.
    if m.text.trim_start().starts_with("-> tool call:") {
        return vec![Line::from(Span::styled(
            m.text.clone(),
            theme::tool_call_style(),
        ))];
    }

    // Otherwise this is a tool result line. Look up the matching call to
    // decide between collapsed/expanded.
    let call = m
        .tool_call_id
        .as_ref()
        .and_then(|id| app.tool_calls.iter().find(|tc| &tc.tool_call_id == id));

    let style = if m.is_error {
        theme::tool_err_style()
    } else {
        theme::tool_ok_style()
    };
    let header_label = if m.is_error {
        "      [tool err]"
    } else {
        "      [tool ok]"
    };

    let collapsed = match call {
        Some(c) => app.tools_collapsed && c.collapsed,
        None => app.tools_collapsed,
    };

    if collapsed {
        let preview: String = call
            .and_then(|c| c.result_preview.as_deref())
            .unwrap_or("")
            .chars()
            .take(160)
            .collect();
        let suffix = if preview.is_empty() {
            "<expand: Ctrl+O>".to_string()
        } else {
            format!("{preview}  <expand: Ctrl+O>")
        };
        return vec![Line::from(vec![
            Span::styled(format!("{header_label} "), style),
            Span::styled(
                suffix,
                Style::default()
                    .fg(ratatui::style::Color::DarkGray)
                    .add_modifier(Modifier::DIM),
            ),
        ])];
    }

    let body = call
        .and_then(|c| c.result_full.as_deref())
        .unwrap_or_else(|| m.text.as_str());
    let mut out = Vec::new();
    out.push(Line::from(Span::styled(header_label.to_string(), style)));
    for raw in body.lines() {
        out.push(Line::from(Span::styled(format!("      {raw}"), style)));
    }
    out
}

fn render_image_placeholders(m: &RenderedMessage) -> Vec<Line<'static>> {
    if m.images.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::with_capacity(m.images.len());
    for img in &m.images {
        let bytes = img.bytes.len();
        let (kb_or_b, unit) = if bytes >= 1024 {
            (bytes as f32 / 1024.0, "KB")
        } else {
            (bytes as f32, "B")
        };
        let dims = if img.width_px == 0 || img.height_px == 0 {
            String::new()
        } else {
            format!(" {}x{}", img.width_px, img.height_px)
        };
        let label = format!("[image: {}{} {:.1} {}]", img.mime, dims, kb_or_b, unit);
        out.push(Line::from(Span::styled(
            label,
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::ITALIC),
        )));
    }
    out
}

fn draw_input(f: &mut Frame, area: Rect, app: &App) {
    let attach_n = app.pending_attachments.len();
    if attach_n > 0 {
        // The 1-row queue indicator slot above us is already painted; we
        // can't grow this Rect, so we layer a single attachment notice on
        // the top border title.
    }
    let title = if attach_n == 0 {
        "input".to_string()
    } else {
        format!("input  ·  {attach_n} image attached")
    };
    let border_color = match &app.status {
        AppStatus::Errored(_) => Color::LightRed,
        AppStatus::Aborted => Color::Red,
        _ => theme::thinking_level_color(app.thinking_level),
    };
    let mut border_style = Style::default().fg(border_color);
    if matches!(app.status, AppStatus::Running) {
        border_style = border_style.add_modifier(Modifier::BOLD);
    }
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(border_style)
        .title(title);
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

fn draw_queue_indicator(f: &mut Frame, area: Rect, app: &App) {
    let mut left_spans: Vec<Span<'static>> = Vec::new();

    if matches!(app.status, AppStatus::Running) {
        let glyph = app.spinner_glyph();
        left_spans.push(Span::styled(format!("{glyph} "), theme::spinner_style()));
        let elapsed = app.elapsed_label();
        if !elapsed.is_empty() {
            left_spans.push(Span::styled(format!("{elapsed}  "), theme::status_style()));
        }
    }

    if app.queued_steering_count > 0 {
        left_spans.push(Span::styled(
            format!("! {} queued  ", app.queued_steering_count),
            theme::queue_style(),
        ));
    }
    if app.queued_followup_count > 0 {
        left_spans.push(Span::styled(
            format!("> {} follow-up  ", app.queued_followup_count),
            theme::queue_style(),
        ));
    }

    let right_text = app
        .current_tool
        .as_ref()
        .map(|t| format!("tool: {t}"))
        .unwrap_or_default();

    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Min(1), Constraint::Length(40)])
        .split(area);

    let left = Paragraph::new(Line::from(left_spans)).alignment(Alignment::Left);
    let right = Paragraph::new(Line::from(Span::styled(right_text, theme::status_style())))
        .alignment(Alignment::Right);
    f.render_widget(left, cols[0]);
    f.render_widget(right, cols[1]);
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

fn draw_widgets(f: &mut Frame, area: Rect, app: &App) {
    if area.height == 0 || app.slots.widgets.is_empty() {
        return;
    }
    let mut y = area.y;
    let width = area.width;
    for w in &app.slots.widgets {
        let h = w.min_height();
        if y + h > area.y + area.height {
            break;
        }
        let lines = w.render(app, width);
        let rect = Rect {
            x: area.x,
            y,
            width,
            height: h,
        };
        let p = Paragraph::new(lines);
        f.render_widget(p, rect);
        y += h;
    }
}

fn draw_tree_overlay(f: &mut Frame, area: Rect, app: &App) {
    let overlay = centered_rect(area, 90, 90);
    f.render_widget(Clear, overlay);

    let block = Block::default()
        .borders(Borders::ALL)
        .title("Session Tree (Esc to close)")
        .border_style(Style::default().fg(Color::Cyan));
    let inner = block.inner(overlay);
    f.render_widget(block, overlay);

    if inner.height < 4 {
        return;
    }

    let header_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Min(0),
            Constraint::Length(1),
        ])
        .split(inner);

    let search_line = Line::from(vec![
        Span::styled("Search: ", Style::default().add_modifier(Modifier::BOLD)),
        Span::styled(
            format!("{}_", app.tree_search),
            Style::default().fg(Color::Yellow),
        ),
        Span::raw("  Filter: "),
        Span::styled(
            app.tree_filter.label().to_string(),
            Style::default()
                .fg(Color::Magenta)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("  Bookmarks: "),
        Span::styled(
            app.tree_bookmarks.len().to_string(),
            Style::default().fg(Color::Green),
        ),
    ]);
    f.render_widget(Paragraph::new(search_line), header_chunks[0]);

    let modes_line = Line::from(vec![Span::styled(
        "Modes: Default | NoTools | UserOnly | Labeled | All  (Ctrl+O cycles)",
        Style::default().add_modifier(Modifier::DIM),
    )]);
    f.render_widget(Paragraph::new(modes_line), header_chunks[1]);

    let visible = app.visible_tree_indices();
    let body_h = header_chunks[2].height as usize;
    let cursor = app.tree_cursor.min(visible.len().saturating_sub(1));
    let scroll_top = if cursor >= body_h {
        cursor - body_h + 1
    } else {
        0
    };

    let lines: Vec<Line> = visible
        .iter()
        .enumerate()
        .skip(scroll_top)
        .take(body_h)
        .map(|(i, idx)| {
            let m = &app.messages[*idx];
            let mark = if i == cursor { "▶ " } else { "  " };
            let role = match m.role {
                MessageRole::User => "user",
                MessageRole::Assistant => "asst",
                MessageRole::Thinking => "think",
                MessageRole::ToolResult => "tool",
                MessageRole::Notification => "note",
            };
            let bookmark = if app.tree_bookmarks.contains(idx) {
                "*"
            } else {
                " "
            };
            let ts_prefix = if app.tree_show_timestamps {
                format!("[{}] ", m.timestamp)
            } else {
                String::new()
            };
            let preview: String = m.text.chars().take(120).collect();
            let row = format!("{mark}{bookmark} #{idx:>3} {role:<5} {ts_prefix}{preview}");
            let style = if i == cursor {
                Style::default()
                    .bg(Color::DarkGray)
                    .add_modifier(Modifier::BOLD)
            } else {
                role_style(m.role)
            };
            Line::from(Span::styled(row, style))
        })
        .collect();

    let body = Paragraph::new(lines).wrap(Wrap { trim: false });
    f.render_widget(body, header_chunks[2]);

    let footer = Line::from(vec![Span::styled(
        "Up/Down move · Enter pivot · Shift+L bookmark · Shift+T timestamps · Esc close",
        Style::default().add_modifier(Modifier::DIM),
    )]);
    f.render_widget(Paragraph::new(footer), header_chunks[3]);
}

fn role_style(role: MessageRole) -> Style {
    match role {
        MessageRole::User => theme::user_style(),
        MessageRole::Assistant => Style::default(),
        MessageRole::Thinking => theme::thinking_style(),
        MessageRole::ToolResult => theme::tool_call_style(),
        MessageRole::Notification => theme::notification_style(),
    }
}

fn draw_hotkeys_overlay(f: &mut Frame, area: Rect, app: &App) {
    let overlay = centered_rect(area, 80, 80);
    f.render_widget(Clear, overlay);

    let block = Block::default()
        .borders(Borders::ALL)
        .title("Hotkeys (Esc to close)")
        .border_style(Style::default().fg(Color::Yellow));
    let inner = block.inner(overlay);
    f.render_widget(block, overlay);

    if inner.height < 2 {
        return;
    }

    let mut lines: Vec<Line<'static>> = Vec::new();
    for section in app.keybindings.sections() {
        lines.push(Line::from(Span::styled(
            format!("[{section}]"),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )));
        let entries: Vec<Keybinding> = app.keybindings.for_section(section);
        for b in entries {
            let row = format!("  {:<24}  {:<14}  {}", b.action, b.key_combo, b.description);
            lines.push(Line::from(Span::raw(row)));
        }
        lines.push(Line::from(Span::raw("")));
    }

    let body_h = inner.height as usize;
    let scroll_top = app.hotkeys_cursor.min(lines.len().saturating_sub(body_h));

    let p = Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .scroll((scroll_top as u16, 0));
    f.render_widget(p, inner);
}

/// Centred sub-rect computed as a percentage of the parent.
fn centered_rect(parent: Rect, percent_x: u16, percent_y: u16) -> Rect {
    let w = parent.width * percent_x / 100;
    let h = parent.height * percent_y / 100;
    let x = parent.x + (parent.width.saturating_sub(w)) / 2;
    let y = parent.y + (parent.height.saturating_sub(h)) / 2;
    Rect {
        x,
        y,
        width: w,
        height: h,
    }
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
        let backend = TestBackend::new(80, 14);
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
        let backend = TestBackend::new(80, 18);
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

    #[test]
    fn collapsed_tool_result_shows_single_line_with_ctrl_o_hint() {
        use crate::app::{RenderedMessage, RenderedToolCall, ToolState};

        let (_sink, rx) = ChannelSink::new();
        let mut app = App::new(
            "s1".into(),
            "anthropic".into(),
            "claude".into(),
            "/tmp".into(),
            rx,
            Arc::new(Noop),
        );
        app.tool_calls.push(RenderedToolCall {
            tool_call_id: "c9".into(),
            tool_name: "read".into(),
            args: serde_json::json!({}),
            state: ToolState::Done,
            result_preview: Some("first line of result".into()),
            result_full: Some("first line of result\nsecond line\nthird line".into()),
            collapsed: true,
        });
        app.messages.push(RenderedMessage {
            role: MessageRole::ToolResult,
            text: "      [tool ok] first line of result".into(),
            timestamp: 0,
            thinking_token_count: None,
            tool_call_id: Some("c9".into()),
            is_error: false,
            images: Vec::new(),
        });

        let lines = render_tool_result_lines(&app.messages[0], &app);
        assert_eq!(lines.len(), 1);
        let dumped: String = lines[0].spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(dumped.contains("[tool ok]"));
        assert!(dumped.contains("Ctrl+O"));
    }

    #[test]
    fn expanded_tool_result_emits_multiple_indented_lines() {
        use crate::app::{RenderedMessage, RenderedToolCall, ToolState};

        let (_sink, rx) = ChannelSink::new();
        let mut app = App::new(
            "s1".into(),
            "anthropic".into(),
            "claude".into(),
            "/tmp".into(),
            rx,
            Arc::new(Noop),
        );
        app.tools_collapsed = false;
        app.tool_calls.push(RenderedToolCall {
            tool_call_id: "c9".into(),
            tool_name: "read".into(),
            args: serde_json::json!({}),
            state: ToolState::Done,
            result_preview: Some("first".into()),
            result_full: Some("alpha\nbeta\ngamma".into()),
            collapsed: false,
        });
        app.messages.push(RenderedMessage {
            role: MessageRole::ToolResult,
            text: "      [tool ok] alpha".into(),
            timestamp: 0,
            thinking_token_count: None,
            tool_call_id: Some("c9".into()),
            is_error: false,
            images: Vec::new(),
        });

        let lines = render_tool_result_lines(&app.messages[0], &app);
        assert!(
            lines.len() >= 4,
            "expected header + 3 body, got {}",
            lines.len()
        );
        let last: String = lines[3].spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(last.contains("gamma"));
        assert!(last.starts_with("      "));
    }

    #[test]
    fn queue_indicator_shows_counts_and_spinner() {
        let backend = TestBackend::new(80, 14);
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
        app.apply_event(AgentEvent::AgentStart);
        app.queued_steering_count = 2;
        app.queued_followup_count = 1;
        terminal.draw(|f| draw(f, &app)).unwrap();
        let buffer = terminal.backend().buffer().clone();
        let dump = buffer
            .content
            .iter()
            .map(|c| c.symbol())
            .collect::<String>();
        assert!(dump.contains("2 queued"), "missing queue marker in {dump}");
        assert!(
            dump.contains("1 follow-up"),
            "missing follow-up marker in {dump}"
        );
    }
}
