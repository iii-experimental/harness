//! Pluggable status-line + widget slots.
//!
//! 0.1 ships an enum-based registry of builtin items rather than the trait
//! objects originally sketched. The point at this stage is reserving the
//! layout space + the composition contract — not a public extension API.
//! When we wire hook subscribers, this module will grow `register_*` methods
//! that accept boxed trait objects.
//!
//! Status-line items render as left/right-aligned spans, separated by ` · `.
//! Widgets reserve a fixed number of rows above the input area; if no widgets
//! are registered, the area collapses to zero rows.

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::app::{App, AppStatus};
use crate::theme;

/// Where a status item renders along the bottom strip.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusPosition {
    Left,
    Right,
}

/// The set of builtin status items. New variants here = new chips on the
/// status line. `App` stores a `SlotRegistry` whose status order is the order
/// added.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuiltinStatus {
    Model,
    Provider,
    Cwd,
    Turns,
    Tokens,
    Cost,
    CtxUsed,
    ThinkingLevel,
}

impl BuiltinStatus {
    pub fn id(self) -> &'static str {
        match self {
            Self::Model => "model",
            Self::Provider => "provider",
            Self::Cwd => "cwd",
            Self::Turns => "turns",
            Self::Tokens => "tokens",
            Self::Cost => "cost",
            Self::CtxUsed => "ctx_used",
            Self::ThinkingLevel => "thinking_level",
        }
    }

    pub fn position(self) -> StatusPosition {
        match self {
            Self::Model | Self::Provider | Self::Cwd => StatusPosition::Left,
            _ => StatusPosition::Right,
        }
    }

    pub fn render(self, app: &App) -> Option<Span<'static>> {
        let text = match self {
            Self::Model => format!("model:{}", app.model),
            Self::Provider => format!("provider:{}", app.provider_name),
            Self::Cwd => format!("cwd:{}", short_cwd(&app.cwd)),
            Self::Turns => format!("{} turns", app.turn_count),
            Self::Tokens => format!("up{} dn{}", app.usage.input, app.usage.output),
            Self::Cost => app
                .usage
                .cost_usd
                .map_or_else(|| "$-".to_string(), |c| format!("${c:.4}")),
            Self::CtxUsed => {
                let total = app.usage.input.saturating_add(app.usage.output);
                format!("ctx {}k/{}k", total / 1000, app.context_window / 1000)
            }
            Self::ThinkingLevel => {
                format!("think:{}", theme::thinking_level_label(app.thinking_level))
            }
        };
        Some(Span::styled(text, theme::status_style()))
    }
}

/// Builtin widget variants. Empty for 0.1 — `Status` is a placeholder for the
/// status-line / spinner row that the renderer paints separately. Future
/// hook subscribers add variants here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuiltinWidget {
    /// One-row banner that mirrors run state. Shipped as a demonstration of
    /// the contract; renderers can opt out by not registering it.
    StatusBanner,
}

impl BuiltinWidget {
    pub fn id(self) -> &'static str {
        match self {
            Self::StatusBanner => "status_banner",
        }
    }

    pub fn min_height(self) -> u16 {
        match self {
            Self::StatusBanner => 1,
        }
    }

    pub fn render(self, app: &App, _width: u16) -> Vec<Line<'static>> {
        match self {
            Self::StatusBanner => {
                let text = match &app.status {
                    AppStatus::Idle => "ready".to_string(),
                    AppStatus::Running => format!("running · {}", app.elapsed_label()),
                    AppStatus::Aborted => "aborted".to_string(),
                    AppStatus::Errored(e) => format!("error: {e}"),
                };
                vec![Line::from(Span::styled(
                    text,
                    Style::default().add_modifier(Modifier::DIM),
                ))]
            }
        }
    }
}

/// Composable registry of status items + widgets.
#[derive(Debug, Clone, Default)]
pub struct SlotRegistry {
    pub status_items: Vec<BuiltinStatus>,
    pub widgets: Vec<BuiltinWidget>,
}

impl SlotRegistry {
    pub fn new() -> Self {
        Self {
            status_items: Vec::new(),
            widgets: Vec::new(),
        }
    }

    /// Default config registered on `App::new`. Mirrors the inline status
    /// string the renderer used to assemble.
    pub fn defaults() -> Self {
        Self {
            status_items: vec![
                BuiltinStatus::Model,
                BuiltinStatus::Provider,
                BuiltinStatus::Cwd,
                BuiltinStatus::Turns,
                BuiltinStatus::Tokens,
                BuiltinStatus::Cost,
                BuiltinStatus::CtxUsed,
                BuiltinStatus::ThinkingLevel,
            ],
            widgets: Vec::new(),
        }
    }

    pub fn register_status(&mut self, item: BuiltinStatus) {
        self.status_items.push(item);
    }

    pub fn register_widget(&mut self, widget: BuiltinWidget) {
        self.widgets.push(widget);
    }

    /// Sum of `min_height` across registered widgets. Renderer reserves this
    /// many rows; zero means the widget area collapses entirely.
    pub fn widget_height(&self) -> u16 {
        self.widgets.iter().map(|w| w.min_height()).sum()
    }

    /// Compose status spans into a single line, joined by ` · `. Items are
    /// emitted left-first then right-first; the renderer typically right-
    /// aligns this whole line.
    pub fn render_status_line(&self, app: &App) -> Line<'static> {
        let mut spans: Vec<Span<'static>> = Vec::new();
        let mut first = true;
        for &item in &self.status_items {
            if let Some(span) = item.render(app) {
                if !first {
                    spans.push(Span::raw(" · "));
                }
                spans.push(span);
                first = false;
            }
        }
        // Append run status as a final chip so the ex-status_line shape is
        // preserved (idle / running / aborted / errored).
        let status_chip = match &app.status {
            AppStatus::Idle => "idle".to_string(),
            AppStatus::Running => "running".to_string(),
            AppStatus::Aborted => "aborted".to_string(),
            AppStatus::Errored(e) => format!("error: {e}"),
        };
        if !first {
            spans.push(Span::raw(" · "));
        }
        spans.push(Span::styled(status_chip, theme::status_style()));
        Line::from(spans)
    }
}

/// Trim long cwd values to the trailing two segments to keep the chip short.
fn short_cwd(cwd: &str) -> String {
    let parts: Vec<&str> = cwd.rsplit('/').filter(|s| !s.is_empty()).collect();
    if parts.len() <= 2 {
        return cwd.to_string();
    }
    format!(".../{}/{}", parts[1], parts[0])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::App;
    use crate::sink::ChannelSink;
    use std::sync::Arc;

    struct Noop;
    impl crate::app::RuntimeHandle for Noop {
        fn enqueue_steering(&self, _: &str, _: harness_types::AgentMessage) {}
        fn enqueue_followup(&self, _: &str, _: harness_types::AgentMessage) {}
        fn abort(&self, _: &str) {}
    }

    fn fixture() -> App {
        let (_sink, rx) = ChannelSink::new();
        App::new(
            "s1".into(),
            "anthropic".into(),
            "claude".into(),
            "/tmp".into(),
            rx,
            Arc::new(Noop),
        )
    }

    #[test]
    fn default_status_items_registered() {
        let r = SlotRegistry::defaults();
        let ids: Vec<&str> = r.status_items.iter().map(|i| i.id()).collect();
        for required in [
            "model",
            "provider",
            "cwd",
            "turns",
            "tokens",
            "cost",
            "ctx_used",
            "thinking_level",
        ] {
            assert!(ids.contains(&required), "missing {required} in {ids:?}");
        }
    }

    #[test]
    fn status_item_render_returns_span() {
        let app = fixture();
        let s = BuiltinStatus::Model.render(&app).expect("render");
        assert!(s.content.contains("claude"));
        let p = BuiltinStatus::Provider.render(&app).expect("render");
        assert!(p.content.contains("anthropic"));
    }

    #[test]
    fn widget_min_height_aggregates_correctly() {
        let mut r = SlotRegistry::new();
        assert_eq!(r.widget_height(), 0);
        r.register_widget(BuiltinWidget::StatusBanner);
        assert_eq!(r.widget_height(), 1);
        r.register_widget(BuiltinWidget::StatusBanner);
        assert_eq!(r.widget_height(), 2);
    }

    #[test]
    fn empty_widget_registry_zero_height() {
        let r = SlotRegistry::new();
        assert_eq!(r.widget_height(), 0);
    }

    #[test]
    fn render_status_line_includes_run_state() {
        let app = fixture();
        let r = SlotRegistry::defaults();
        let line = r.render_status_line(&app);
        let dump: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(dump.contains("idle"));
        assert!(dump.contains("model:claude"));
    }
}
