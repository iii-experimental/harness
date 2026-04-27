//! Minimal markdown -> ratatui `Line` renderer for assistant messages.
//!
//! Supports bold, italic, inline code, fenced code blocks, headings, lists,
//! links, and blockquotes. Anything else falls back to plain text.

use pulldown_cmark::{Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use crate::theme;

/// Parse the input as markdown and return a vector of styled lines suitable
/// for embedding in scrollback.
pub fn parse_to_lines(text: &str, theme: &Theme) -> Vec<Line<'static>> {
    let mut opts = Options::empty();
    opts.insert(Options::ENABLE_STRIKETHROUGH);
    let parser = Parser::new_ext(text, opts);

    let mut state = RenderState::new(theme);
    for event in parser {
        state.handle(event);
    }
    state.finish()
}

/// Renderer-facing snapshot of the colour palette. Decoupled so the markdown
/// module never has to know about `crate::theme` directly inside callers.
#[derive(Debug, Clone, Copy)]
pub struct Theme {
    pub heading_h1: Color,
    pub heading_other: Color,
    pub inline_code: Color,
    pub fenced_code: Color,
    pub link: Color,
}

impl Theme {
    pub fn from_palette() -> Self {
        Self {
            heading_h1: theme::COLOR_HEADER,
            heading_other: Color::Cyan,
            inline_code: Color::Yellow,
            fenced_code: Color::Cyan,
            link: Color::Cyan,
        }
    }
}

/// Internal pulldown-cmark stream consumer. We accumulate spans into the
/// "current" line and flush on hard breaks / block boundaries.
struct RenderState<'a> {
    theme: &'a Theme,
    lines: Vec<Line<'static>>,
    current: Vec<Span<'static>>,
    style_stack: Vec<Style>,
    in_code_block: bool,
    code_lang: Option<String>,
    code_buffer: String,
    in_heading: Option<HeadingLevel>,
    list_depth: usize,
    ordered_counters: Vec<u64>,
    in_blockquote: bool,
    pending_link_url: Option<String>,
}

impl<'a> RenderState<'a> {
    fn new(theme: &'a Theme) -> Self {
        Self {
            theme,
            lines: Vec::new(),
            current: Vec::new(),
            style_stack: vec![Style::default()],
            in_code_block: false,
            code_lang: None,
            code_buffer: String::new(),
            in_heading: None,
            list_depth: 0,
            ordered_counters: Vec::new(),
            in_blockquote: false,
            pending_link_url: None,
        }
    }

    fn current_style(&self) -> Style {
        *self.style_stack.last().unwrap_or(&Style::default())
    }

    fn push_style(&mut self, modify: impl FnOnce(Style) -> Style) {
        let next = modify(self.current_style());
        self.style_stack.push(next);
    }

    fn pop_style(&mut self) {
        if self.style_stack.len() > 1 {
            self.style_stack.pop();
        }
    }

    fn flush_line(&mut self) {
        let spans = std::mem::take(&mut self.current);
        if spans.is_empty() {
            return;
        }
        self.lines.push(Line::from(spans));
    }

    fn push_blank(&mut self) {
        self.lines.push(Line::from(Vec::<Span<'static>>::new()));
    }

    fn push_text(&mut self, text: &str) {
        if self.in_code_block {
            self.code_buffer.push_str(text);
            return;
        }
        let style = self.current_style();
        for (i, segment) in text.split('\n').enumerate() {
            if i > 0 {
                self.flush_line();
                if self.in_blockquote {
                    self.current.push(Span::styled(
                        "| ",
                        Style::default()
                            .fg(Color::DarkGray)
                            .add_modifier(Modifier::DIM),
                    ));
                }
            }
            if !segment.is_empty() {
                self.current.push(Span::styled(segment.to_string(), style));
            }
        }
    }

    fn handle(&mut self, event: Event<'_>) {
        match event {
            Event::Start(tag) => self.start_tag(tag),
            Event::End(tag) => self.end_tag(tag),
            Event::Text(t) => self.push_text(&t),
            Event::Code(c) => {
                let style = Style::default().fg(self.theme.inline_code);
                self.current.push(Span::styled(c.to_string(), style));
            }
            Event::SoftBreak => {
                self.current.push(Span::raw(" "));
            }
            Event::HardBreak => {
                self.flush_line();
            }
            Event::Rule => {
                self.flush_line();
                self.lines.push(Line::from(Span::styled(
                    "---",
                    Style::default().fg(Color::DarkGray),
                )));
            }
            Event::Html(h) | Event::InlineHtml(h) => {
                self.push_text(&h);
            }
            Event::FootnoteReference(_)
            | Event::TaskListMarker(_)
            | Event::InlineMath(_)
            | Event::DisplayMath(_) => {}
        }
    }

    fn start_tag(&mut self, tag: Tag<'_>) {
        match tag {
            Tag::Paragraph => {}
            Tag::Heading { level, .. } => {
                self.flush_line();
                self.in_heading = Some(level);
                let count = heading_count(level);
                let prefix: String = std::iter::repeat_n('#', count)
                    .chain(std::iter::once(' '))
                    .collect();
                let color = if matches!(level, HeadingLevel::H1) {
                    self.theme.heading_h1
                } else {
                    self.theme.heading_other
                };
                let style = Style::default().fg(color).add_modifier(Modifier::BOLD);
                self.current.push(Span::styled(prefix, style));
                self.push_style(|s| s.fg(color).add_modifier(Modifier::BOLD));
            }
            Tag::BlockQuote(_) => {
                self.flush_line();
                self.in_blockquote = true;
                self.current.push(Span::styled(
                    "| ",
                    Style::default()
                        .fg(Color::DarkGray)
                        .add_modifier(Modifier::DIM),
                ));
                self.push_style(|s| s.fg(Color::DarkGray).add_modifier(Modifier::DIM));
            }
            Tag::CodeBlock(kind) => {
                self.flush_line();
                self.push_blank();
                self.in_code_block = true;
                self.code_buffer.clear();
                self.code_lang = match kind {
                    pulldown_cmark::CodeBlockKind::Fenced(s) => {
                        let trimmed = s.trim();
                        if trimmed.is_empty() {
                            None
                        } else {
                            Some(trimmed.to_string())
                        }
                    }
                    pulldown_cmark::CodeBlockKind::Indented => None,
                };
            }
            Tag::List(start) => {
                self.flush_line();
                self.list_depth += 1;
                self.ordered_counters.push(start.unwrap_or(0));
            }
            Tag::Item => {
                self.flush_line();
                let indent: String = "  ".repeat(self.list_depth.saturating_sub(1));
                let bullet = if let Some(counter) = self.ordered_counters.last_mut() {
                    if *counter > 0 {
                        let n = *counter;
                        *counter += 1;
                        format!("{indent}{n}. ")
                    } else {
                        format!("{indent}* ")
                    }
                } else {
                    format!("{indent}* ")
                };
                self.current
                    .push(Span::styled(bullet, Style::default().fg(Color::DarkGray)));
            }
            Tag::Emphasis => {
                self.push_style(|s| s.add_modifier(Modifier::ITALIC));
            }
            Tag::Strong => {
                self.push_style(|s| s.add_modifier(Modifier::BOLD));
            }
            Tag::Strikethrough => {
                self.push_style(|s| s.add_modifier(Modifier::CROSSED_OUT));
            }
            Tag::Link { dest_url, .. } => {
                self.pending_link_url = Some(dest_url.to_string());
                self.push_style(|s| s.fg(self.theme.link).add_modifier(Modifier::UNDERLINED));
            }
            Tag::Image { dest_url, .. } => {
                let style = Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::DIM);
                self.current
                    .push(Span::styled(format!("[image: {dest_url}]"), style));
            }
            _ => {}
        }
    }

    fn end_tag(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Paragraph => {
                self.flush_line();
                self.push_blank();
            }
            TagEnd::Heading(_) => {
                self.in_heading = None;
                self.pop_style();
                self.flush_line();
                self.push_blank();
            }
            TagEnd::BlockQuote(_) => {
                self.flush_line();
                self.in_blockquote = false;
                self.pop_style();
                self.push_blank();
            }
            TagEnd::CodeBlock => {
                self.in_code_block = false;
                let style = Style::default()
                    .fg(self.theme.fenced_code)
                    .add_modifier(Modifier::DIM);
                if let Some(lang) = self.code_lang.take() {
                    self.lines
                        .push(Line::from(Span::styled(format!("  ```{lang}"), style)));
                } else {
                    self.lines
                        .push(Line::from(Span::styled("  ```".to_string(), style)));
                }
                let body = std::mem::take(&mut self.code_buffer);
                for raw in body.split('\n') {
                    if raw.is_empty() {
                        self.lines
                            .push(Line::from(Span::styled("  ".to_string(), style)));
                    } else {
                        self.lines
                            .push(Line::from(Span::styled(format!("  {raw}"), style)));
                    }
                }
                self.lines
                    .push(Line::from(Span::styled("  ```".to_string(), style)));
                self.push_blank();
            }
            TagEnd::List(_) => {
                self.flush_line();
                self.list_depth = self.list_depth.saturating_sub(1);
                self.ordered_counters.pop();
                if self.list_depth == 0 {
                    self.push_blank();
                }
            }
            TagEnd::Item => {
                self.flush_line();
            }
            TagEnd::Emphasis | TagEnd::Strong | TagEnd::Strikethrough => {
                self.pop_style();
            }
            TagEnd::Link => {
                self.pop_style();
                if let Some(url) = self.pending_link_url.take() {
                    self.current.push(Span::styled(
                        format!(" ({url})"),
                        Style::default()
                            .fg(Color::DarkGray)
                            .add_modifier(Modifier::DIM),
                    ));
                }
            }
            _ => {}
        }
    }

    fn finish(mut self) -> Vec<Line<'static>> {
        if !self.current.is_empty() {
            self.flush_line();
        }
        // Strip trailing blank lines so callers can join cleanly.
        while self
            .lines
            .last()
            .is_some_and(|l| l.spans.iter().all(|s| s.content.is_empty()))
        {
            self.lines.pop();
        }
        if self.lines.is_empty() {
            self.lines.push(Line::from(""));
        }
        self.lines
    }
}

fn heading_count(level: HeadingLevel) -> usize {
    match level {
        HeadingLevel::H1 => 1,
        HeadingLevel::H2 => 2,
        HeadingLevel::H3 => 3,
        HeadingLevel::H4 => 4,
        HeadingLevel::H5 => 5,
        HeadingLevel::H6 => 6,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn render(text: &str) -> Vec<Line<'static>> {
        parse_to_lines(text, &Theme::from_palette())
    }

    fn line_text(line: &Line<'_>) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn bold_yields_bold_modifier() {
        let lines = render("**hello**");
        let text: String = line_text(&lines[0]);
        assert!(text.contains("hello"));
        let has_bold = lines[0]
            .spans
            .iter()
            .any(|s| s.content == "hello" && s.style.add_modifier.contains(Modifier::BOLD));
        assert!(has_bold, "expected bold span: {:?}", lines[0].spans);
    }

    #[test]
    fn italic_yields_italic_modifier() {
        let lines = render("*tilt*");
        let has_italic = lines[0]
            .spans
            .iter()
            .any(|s| s.content == "tilt" && s.style.add_modifier.contains(Modifier::ITALIC));
        assert!(has_italic);
    }

    #[test]
    fn inline_code_is_yellow() {
        let lines = render("call `do_thing()` now");
        let has_code = lines[0]
            .spans
            .iter()
            .any(|s| s.content == "do_thing()" && s.style.fg == Some(Color::Yellow));
        assert!(has_code, "expected yellow inline code span");
    }

    #[test]
    fn fenced_code_block_emits_multiple_lines() {
        let src = "before\n\n```rust\nfn main() {}\nlet x = 1;\n```\n\nafter";
        let lines = render(src);
        let joined: String = lines
            .iter()
            .map(|l| line_text(l))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(joined.contains("```rust"));
        assert!(joined.contains("fn main() {}"));
        assert!(joined.contains("let x = 1;"));
        // body should be indented 2 spaces.
        assert!(joined.contains("  fn main() {}"));
    }

    #[test]
    fn h1_heading_has_hash_prefix_and_bold() {
        let lines = render("# Title");
        let text = line_text(&lines[0]);
        assert!(text.starts_with("# "), "expected '# ' prefix, got {text:?}");
        let has_bold = lines[0]
            .spans
            .iter()
            .any(|s| s.style.add_modifier.contains(Modifier::BOLD));
        assert!(has_bold, "expected bold heading span");
    }

    #[test]
    fn list_item_is_indented_with_bullet() {
        let lines = render("- one\n- two");
        let texts: Vec<String> = lines.iter().map(line_text).collect();
        let has_bullet = texts.iter().any(|t| t.starts_with("* one"));
        assert!(has_bullet, "expected bullet line, got {texts:?}");
    }

    #[test]
    fn link_renders_text_then_dimmed_url() {
        let lines = render("[click](https://example.com)");
        let text = line_text(&lines[0]);
        assert!(text.contains("click"));
        assert!(text.contains("https://example.com"));
    }

    #[test]
    fn plain_text_passes_through() {
        let lines = render("just words here");
        let text = line_text(&lines[0]);
        assert_eq!(text, "just words here");
    }

    #[test]
    fn blockquote_has_pipe_prefix() {
        let lines = render("> quoted");
        let text: String = lines.iter().map(line_text).collect::<Vec<_>>().join("\n");
        assert!(
            text.contains("| "),
            "expected blockquote pipe, got {text:?}"
        );
    }
}
