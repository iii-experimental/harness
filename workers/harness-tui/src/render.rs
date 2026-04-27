//! ratatui drawing layer. Pure function of `&App` — no I/O of its own.
//!
//! Native image escape sequences (Kitty / iTerm2) cannot be expressed in a
//! ratatui buffer because ratatui only knows about styled cells; the raw
//! escape bytes have to land on the underlying terminal stream verbatim.
//! The renderer captures those bytes into a [`PostDrawEscapes`] collector
//! during the draw pass; the caller writes them after `Terminal::draw`
//! returns so they overlay the placeholder cells ratatui already painted.

use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};
use ratatui::Frame;

use crate::app::{App, AppStatus, MessageRole, RenderedMessage};
use crate::image::ImageProtocol;
use crate::keybindings::Keybinding;
use crate::markdown;
use crate::theme;

const MAX_INPUT_ROWS: u16 = 10;

/// One escape-sequence write queued during draw.
///
/// The renderer fills these in while ratatui paints the placeholder rows;
/// the caller flushes them after `Terminal::draw` returns so the escape
/// bytes land on top of the cells ratatui just wrote.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EscapeJob {
    /// Cell column (zero-indexed from the screen origin).
    pub col: u16,
    /// Cell row (zero-indexed from the screen origin).
    pub row: u16,
    /// Raw escape bytes to emit at `(col, row)`. Already includes the
    /// terminator (`ESC \\` for Kitty, `BEL` for iTerm2).
    pub payload: String,
}

/// Bundle of post-draw escape writes. Empty after a draw on terminals that
/// don't speak any image protocol — caller can skip writing entirely in
/// that case.
#[derive(Debug, Default, Clone)]
pub struct PostDrawEscapes {
    pub jobs: Vec<EscapeJob>,
}

impl PostDrawEscapes {
    fn push(&mut self, col: u16, row: u16, payload: String) {
        if payload.is_empty() {
            return;
        }
        self.jobs.push(EscapeJob { col, row, payload });
    }
}

/// Top-level draw entry point. Header + scrollback + widget area + queue
/// indicator + input + status bar layout. Overlays paint on top.
///
/// `escapes` collects any native image-protocol escape bytes that should be
/// written *after* ratatui's own draw flushes — see [`PostDrawEscapes`] for
/// the rationale. On terminals without Kitty/iTerm2 support this stays empty.
pub fn draw(f: &mut Frame, app: &App, escapes: &mut PostDrawEscapes) {
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
    draw_messages(f, chunks[1], app, escapes);
    draw_widgets(f, chunks[2], app);
    draw_queue_indicator(f, chunks[3], app);
    draw_input(f, chunks[4], app);
    draw_status(f, chunks[5], app);

    if app.command_picker_visible {
        draw_command_picker(f, chunks[1], chunks[4], app);
    } else if app.file_picker_visible {
        draw_file_picker(f, chunks[1], chunks[4], app);
    }

    // Fullscreen overlays sit above everything, including the pickers. When a
    // tree or hotkeys overlay is up the message area is hidden, so the image
    // escapes queued for it would render under the overlay; drop them.
    if app.tree_visible || app.hotkeys_visible {
        escapes.jobs.clear();
    }
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

fn draw_messages(f: &mut Frame, area: Rect, app: &App, escapes: &mut PostDrawEscapes) {
    let md_theme = markdown::Theme::from_palette();
    let mut lines: Vec<Line<'static>> = Vec::with_capacity(app.messages.len() * 2);
    // Track every image we render so the post-draw step can compute screen
    // coordinates from the line index. We collect (line_index, ImagePayload)
    // here and resolve coordinates after the full line list is built.
    let mut image_anchors: Vec<(usize, &crate::app::ImagePayload)> = Vec::new();

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
        // Image placeholders are one line per image in the placeholder
        // fallback. When native rendering is enabled we still emit them so
        // ratatui reserves the row, and then queue an escape job to overlay
        // the real image on top of that row in the post-draw pass.
        let placeholder_lines = render_image_placeholders(m);
        for (i, img) in m.images.iter().enumerate() {
            image_anchors.push((lines.len(), img));
            lines.push(
                placeholder_lines
                    .get(i)
                    .cloned()
                    .unwrap_or_else(|| Line::from(Span::raw(""))),
            );
        }
    }

    // Inner content rect ratatui will paint into. The outer block draws a top
    // and bottom border so the content rect is `area` shrunk by 1 row top and
    // 1 row bottom; column padding is zero.
    let content_x = area.x;
    let content_y = area.y.saturating_add(1);
    let content_h = area.height.saturating_sub(2);

    if app.image_render_native && !matches!(app.image_protocol, ImageProtocol::None) {
        for (line_idx, img) in image_anchors {
            // Compute screen row for this anchor accounting for the current
            // scroll offset. Anchors above the viewport or below it are
            // skipped — they'll come into view on a future scroll redraw.
            let scroll = app.scroll_offset as usize;
            if line_idx < scroll {
                continue;
            }
            let visible_row = (line_idx - scroll) as u16;
            if visible_row >= content_h {
                continue;
            }
            let row = content_y + visible_row;
            push_image_escapes(
                escapes,
                app.image_protocol,
                img,
                row,
                content_x,
                content_h.saturating_sub(visible_row),
            );
        }
    }

    let p = Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .scroll((app.scroll_offset, 0))
        .block(Block::default().borders(Borders::TOP | Borders::BOTTOM));
    f.render_widget(p, area);
}

/// Encode the image with the given protocol and queue the resulting escape
/// strings as a [`PostDrawEscapes`] job at `(col, row)`. Kitty splits its
/// payload into one escape per chunk; iTerm2 emits a single OSC 1337.
///
/// `max_rows` is the viewport ceiling: the image is clamped to fit.
fn push_image_escapes(
    escapes: &mut PostDrawEscapes,
    protocol: ImageProtocol,
    img: &crate::app::ImagePayload,
    row: u16,
    col: u16,
    max_rows: u16,
) {
    if max_rows == 0 || img.bytes.is_empty() {
        return;
    }
    // Reserve at most 20 rows or `max_rows`, whichever is smaller. Cell
    // metrics aren't available here without an OS-specific syscall, so use
    // standard 8x16 px cells as the canonical assumption.
    let cell_rows =
        crate::image::calculate_image_rows(img.width_px, img.height_px, 8, 16, max_rows.min(20));
    if cell_rows == 0 {
        return;
    }
    match protocol {
        ImageProtocol::Kitty => {
            // Kitty graphics writes the image at the cursor position; the
            // chunks have to be written contiguously without intervening
            // moves so the protocol sees them as one transmission.
            let chunks = crate::image::encode_kitty(&img.bytes, cell_rows);
            let mut combined = String::new();
            for c in chunks {
                combined.push_str(&c);
            }
            escapes.push(col, row, combined);
        }
        ImageProtocol::ITerm2 => {
            let payload = crate::image::encode_iterm2(&img.bytes, cell_rows);
            escapes.push(col, row, payload);
        }
        ImageProtocol::None => {}
    }
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

/// Build per-row prefix glyphs for a tree given each node's `(id, parent_id)`
/// in *display order*. Returns one prefix string per input node, in the same
/// order. Roots get the empty prefix.
///
/// Uses the standard four-glyph alphabet:
/// - `├─ ` — a non-last child at this depth
/// - `└─ ` — the last child at this depth
/// - `│  ` — pass-through column for an ancestor that still has siblings
/// - `   ` — pass-through column for an ancestor that was a last child
///
/// Children of a parent are taken in the order they appear in `entries`, and
/// the same ordering drives "is-last" detection. That matches how the tree
/// would render after a depth-first walk.
pub fn build_tree_prefixes<Id>(entries: &[(Id, Option<Id>)]) -> Vec<String>
where
    Id: Eq + Clone + std::hash::Hash,
{
    if entries.is_empty() {
        return Vec::new();
    }
    // Group children by parent to compute depth + last-child status without
    // building an explicit tree node graph — we only ever need the answers
    // for nodes that appear in `entries`, in input order.
    let mut child_count: std::collections::HashMap<usize, usize> = std::collections::HashMap::new();
    let mut child_seen: std::collections::HashMap<usize, usize> = std::collections::HashMap::new();
    let mut parent_index: Vec<Option<usize>> = vec![None; entries.len()];
    let mut depth: Vec<usize> = vec![0; entries.len()];
    let mut id_to_index: std::collections::HashMap<&Id, usize> =
        std::collections::HashMap::with_capacity(entries.len());
    for (i, (id, _)) in entries.iter().enumerate() {
        id_to_index.insert(id, i);
    }
    for (i, (_, parent)) in entries.iter().enumerate() {
        if let Some(p) = parent {
            if let Some(&pi) = id_to_index.get(p) {
                parent_index[i] = Some(pi);
                *child_count.entry(pi).or_insert(0) += 1;
                depth[i] = depth[pi] + 1;
            }
        }
    }
    // Per node, decide if it is the *last* child of its parent. That requires
    // knowing the total child count up-front (computed above) and the
    // running tally as we visit children in input order (computed below).
    let mut is_last: Vec<bool> = vec![false; entries.len()];
    for i in 0..entries.len() {
        if let Some(pi) = parent_index[i] {
            let total = *child_count.get(&pi).unwrap_or(&0);
            let seen = child_seen.entry(pi).or_insert(0);
            *seen += 1;
            is_last[i] = *seen == total;
        }
    }
    // For each node, walk back up the parent chain to fill ancestor columns.
    let mut out: Vec<String> = Vec::with_capacity(entries.len());
    for i in 0..entries.len() {
        let d = depth[i];
        if d == 0 {
            out.push(String::new());
            continue;
        }
        // Build columns top-down: ancestor at depth 1 is the deepest root
        // child, etc. The column glyph is "│  " if that ancestor was *not*
        // the last child of its own parent, "   " otherwise. Then the leaf
        // column uses "├─ " or "└─ " based on the node's own is_last flag.
        let mut chain: Vec<usize> = Vec::with_capacity(d);
        let mut cur = parent_index[i];
        while let Some(p) = cur {
            chain.push(p);
            cur = parent_index[p];
        }
        chain.reverse();
        let mut s = String::with_capacity(d * 3);
        // chain has all ancestors from root to immediate parent; the ancestor
        // columns are computed from chain[1..] (skip root, which has no
        // column to draw above it). For nodes at depth 1 there are zero
        // ancestor columns to draw.
        for ancestor in chain.iter().skip(1) {
            if is_last[*ancestor] {
                s.push_str("   ");
            } else {
                s.push_str("│  ");
            }
        }
        if is_last[i] {
            s.push_str("└─ ");
        } else {
            s.push_str("├─ ");
        }
        out.push(s);
    }
    out
}

/// Derive `(message_index, parent_index)` pairs from a flat scrollback. The
/// rules mirror what a viewer expects at a glance:
/// - `User` and `Notification` start a new turn at depth 0 (no parent).
/// - `Assistant` and `Thinking` attach to the most recent `User` (depth 1).
/// - `ToolResult` attaches to the most recent `Assistant` if there is one,
///   otherwise to the most recent `User`.
///
/// Each returned tuple uses the message's vector index as its id; that's
/// also what `App.visible_tree_indices` returns.
pub fn derive_tree_edges(messages: &[RenderedMessage]) -> Vec<(usize, Option<usize>)> {
    let mut last_user: Option<usize> = None;
    let mut last_assistant: Option<usize> = None;
    let mut out = Vec::with_capacity(messages.len());
    for (i, m) in messages.iter().enumerate() {
        let parent = match m.role {
            MessageRole::User | MessageRole::Notification => {
                last_user = Some(i);
                last_assistant = None;
                None
            }
            MessageRole::Assistant | MessageRole::Thinking => {
                let p = last_user;
                if matches!(m.role, MessageRole::Assistant) {
                    last_assistant = Some(i);
                }
                p
            }
            MessageRole::ToolResult => last_assistant.or(last_user),
        };
        out.push((i, parent));
    }
    out
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

    // Build branch glyph prefixes off the *full* message vector first so that
    // depth and is-last-child are computed across the whole conversation
    // tree. Filtering / search just hides rows; the tree shape underneath
    // stays stable. Index into `prefixes` by the actual message index.
    let edges = derive_tree_edges(&app.messages);
    let prefixes = build_tree_prefixes(&edges);

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
            let glyph = prefixes.get(*idx).cloned().unwrap_or_default();
            let row = format!("{mark}{bookmark} #{idx:>3} {role:<5} {glyph}{ts_prefix}{preview}");
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
        let mut escapes = PostDrawEscapes::default();
        terminal.draw(|f| draw(f, &app, &mut escapes)).unwrap();
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
        let mut escapes = PostDrawEscapes::default();
        terminal.draw(|f| draw(f, &app, &mut escapes)).unwrap();
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
        let mut escapes = PostDrawEscapes::default();
        terminal.draw(|f| draw(f, &app, &mut escapes)).unwrap();
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

    // ── tree-prefix builder ──────────────────────────────────────────────

    #[test]
    fn build_prefixes_handles_empty_input() {
        let prefixes = build_tree_prefixes::<&str>(&[]);
        assert!(prefixes.is_empty());
    }

    #[test]
    fn build_prefixes_root_has_empty_prefix() {
        let edges = vec![("root", None)];
        let prefixes = build_tree_prefixes(&edges);
        assert_eq!(prefixes, vec![""]);
    }

    #[test]
    fn build_prefixes_marks_last_child_with_corner() {
        // root
        // ├─ a
        // └─ b
        let edges = vec![("r", None), ("a", Some("r")), ("b", Some("r"))];
        let prefixes = build_tree_prefixes(&edges);
        assert_eq!(prefixes[0], "");
        assert_eq!(prefixes[1], "├─ ");
        assert_eq!(prefixes[2], "└─ ");
    }

    #[test]
    fn build_prefixes_threads_pipe_through_non_last_ancestor() {
        // r
        // ├─ a
        // │  └─ a1
        // └─ b
        // a1's prefix should start with "│  " because `a` is not the last
        // child of `r`, then "└─ " for itself (last child of `a`).
        let edges = vec![
            ("r", None),
            ("a", Some("r")),
            ("a1", Some("a")),
            ("b", Some("r")),
        ];
        let prefixes = build_tree_prefixes(&edges);
        assert_eq!(prefixes[2], "│  └─ ");
        assert_eq!(prefixes[3], "└─ ");
    }

    #[test]
    fn build_prefixes_uses_blank_column_after_last_ancestor() {
        // r
        // └─ a
        //    ├─ a1
        //    └─ a2
        let edges = vec![
            ("r", None),
            ("a", Some("r")),
            ("a1", Some("a")),
            ("a2", Some("a")),
        ];
        let prefixes = build_tree_prefixes(&edges);
        assert_eq!(prefixes[1], "└─ ");
        assert_eq!(prefixes[2], "   ├─ ");
        assert_eq!(prefixes[3], "   └─ ");
    }

    #[test]
    fn derive_tree_edges_groups_assistant_under_user() {
        use crate::app::RenderedMessage;
        let msgs = vec![
            RenderedMessage {
                role: MessageRole::User,
                text: "u1".into(),
                timestamp: 0,
                thinking_token_count: None,
                tool_call_id: None,
                is_error: false,
                images: Vec::new(),
            },
            RenderedMessage {
                role: MessageRole::Assistant,
                text: "a1".into(),
                timestamp: 0,
                thinking_token_count: None,
                tool_call_id: None,
                is_error: false,
                images: Vec::new(),
            },
            RenderedMessage {
                role: MessageRole::ToolResult,
                text: "      [tool ok]".into(),
                timestamp: 0,
                thinking_token_count: None,
                tool_call_id: None,
                is_error: false,
                images: Vec::new(),
            },
        ];
        let edges = derive_tree_edges(&msgs);
        assert_eq!(edges[0].1, None);
        assert_eq!(edges[1].1, Some(0));
        assert_eq!(edges[2].1, Some(1));
    }

    #[test]
    fn derive_tree_edges_starts_new_root_on_each_user() {
        use crate::app::RenderedMessage;
        let mk = |role| RenderedMessage {
            role,
            text: String::new(),
            timestamp: 0,
            thinking_token_count: None,
            tool_call_id: None,
            is_error: false,
            images: Vec::new(),
        };
        let msgs = vec![
            mk(MessageRole::User),
            mk(MessageRole::Assistant),
            mk(MessageRole::User),
            mk(MessageRole::Assistant),
        ];
        let edges = derive_tree_edges(&msgs);
        assert_eq!(edges[0].1, None);
        assert_eq!(edges[1].1, Some(0));
        assert_eq!(edges[2].1, None);
        assert_eq!(edges[3].1, Some(2));
    }

    // ── image-escape collector ───────────────────────────────────────────

    #[test]
    fn image_escape_collector_skips_when_protocol_none() {
        use crate::app::ImagePayload;
        let img = ImagePayload {
            mime: "image/png".into(),
            bytes: vec![1, 2, 3, 4],
            width_px: 16,
            height_px: 16,
        };
        let mut esc = PostDrawEscapes::default();
        push_image_escapes(&mut esc, ImageProtocol::None, &img, 0, 0, 5);
        assert!(esc.jobs.is_empty());
    }

    #[test]
    fn image_escape_collector_emits_kitty_payload() {
        use crate::app::ImagePayload;
        let img = ImagePayload {
            mime: "image/png".into(),
            bytes: vec![1, 2, 3, 4],
            width_px: 32,
            height_px: 32,
        };
        let mut esc = PostDrawEscapes::default();
        push_image_escapes(&mut esc, ImageProtocol::Kitty, &img, 4, 2, 8);
        assert_eq!(esc.jobs.len(), 1);
        let j = &esc.jobs[0];
        assert_eq!(j.row, 4);
        assert_eq!(j.col, 2);
        assert!(j.payload.starts_with("\x1b_G"), "payload: {:?}", j.payload);
        assert!(j.payload.ends_with("\x1b\\"));
    }

    #[test]
    fn image_escape_collector_emits_iterm2_osc() {
        use crate::app::ImagePayload;
        let img = ImagePayload {
            mime: "image/jpeg".into(),
            bytes: vec![1, 2, 3, 4],
            width_px: 32,
            height_px: 32,
        };
        let mut esc = PostDrawEscapes::default();
        push_image_escapes(&mut esc, ImageProtocol::ITerm2, &img, 1, 0, 6);
        assert_eq!(esc.jobs.len(), 1);
        let j = &esc.jobs[0];
        assert!(j.payload.starts_with("\x1b]1337;"));
        assert!(j.payload.ends_with('\x07'));
    }

    #[test]
    fn image_escape_collector_skips_zero_size_or_empty_bytes() {
        use crate::app::ImagePayload;
        let mut esc = PostDrawEscapes::default();
        let empty = ImagePayload {
            mime: "image/png".into(),
            bytes: Vec::new(),
            width_px: 16,
            height_px: 16,
        };
        push_image_escapes(&mut esc, ImageProtocol::Kitty, &empty, 0, 0, 4);
        let zero_dim = ImagePayload {
            mime: "image/png".into(),
            bytes: vec![1, 2, 3],
            width_px: 0,
            height_px: 0,
        };
        push_image_escapes(&mut esc, ImageProtocol::Kitty, &zero_dim, 0, 0, 4);
        assert!(esc.jobs.is_empty());
    }
}
