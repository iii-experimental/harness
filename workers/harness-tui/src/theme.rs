//! Colour palette + reusable style helpers used by the renderer.
//!
//! Centralised here so the terminal palette is easy to tweak in one place.

use harness_types::ThinkingLevel;
use ratatui::style::{Color, Modifier, Style};

pub const COLOR_USER: Color = Color::Cyan;
pub const COLOR_ASSISTANT: Color = Color::Reset;
pub const COLOR_TOOL_CALL: Color = Color::DarkGray;
pub const COLOR_TOOL_OK: Color = Color::Green;
pub const COLOR_TOOL_ERR: Color = Color::Red;
pub const COLOR_HEADER: Color = Color::Yellow;
pub const COLOR_STATUS: Color = Color::DarkGray;
pub const COLOR_NOTIFICATION: Color = Color::Magenta;

pub fn header_style() -> Style {
    Style::default()
        .fg(COLOR_HEADER)
        .add_modifier(Modifier::BOLD)
}

pub fn user_style() -> Style {
    Style::default().fg(COLOR_USER).add_modifier(Modifier::BOLD)
}

pub fn assistant_style() -> Style {
    Style::default().fg(COLOR_ASSISTANT)
}

pub fn tool_call_style() -> Style {
    Style::default().fg(COLOR_TOOL_CALL)
}

pub fn tool_ok_style() -> Style {
    Style::default().fg(COLOR_TOOL_OK)
}

pub fn tool_err_style() -> Style {
    Style::default().fg(COLOR_TOOL_ERR)
}

pub fn status_style() -> Style {
    Style::default().fg(COLOR_STATUS)
}

pub fn notification_style() -> Style {
    Style::default()
        .fg(COLOR_NOTIFICATION)
        .add_modifier(Modifier::ITALIC)
}

pub fn thinking_style() -> Style {
    Style::default()
        .fg(Color::DarkGray)
        .add_modifier(Modifier::ITALIC | Modifier::DIM)
}

pub fn spinner_style() -> Style {
    Style::default()
        .fg(COLOR_HEADER)
        .add_modifier(Modifier::BOLD)
}

pub fn queue_style() -> Style {
    Style::default()
        .fg(Color::Magenta)
        .add_modifier(Modifier::BOLD)
}

/// Map a `ThinkingLevel` to the editor border colour. Off is dim; tiers warm
/// up through cyan/blue/magenta and end at red for the highest tier.
pub fn thinking_level_color(level: ThinkingLevel) -> Color {
    match level {
        ThinkingLevel::Off => Color::DarkGray,
        ThinkingLevel::Minimal => Color::Gray,
        ThinkingLevel::Low => Color::Cyan,
        ThinkingLevel::Medium => Color::Blue,
        ThinkingLevel::High => Color::Magenta,
        ThinkingLevel::Xhigh => Color::Red,
    }
}

/// Short label rendered in the status bar chip (`think:high` etc.).
pub fn thinking_level_label(level: ThinkingLevel) -> &'static str {
    match level {
        ThinkingLevel::Off => "off",
        ThinkingLevel::Minimal => "minimal",
        ThinkingLevel::Low => "low",
        ThinkingLevel::Medium => "medium",
        ThinkingLevel::High => "high",
        ThinkingLevel::Xhigh => "xhigh",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn thinking_level_colors_are_distinct() {
        let mut set: HashSet<String> = HashSet::new();
        for lvl in [
            ThinkingLevel::Off,
            ThinkingLevel::Minimal,
            ThinkingLevel::Low,
            ThinkingLevel::Medium,
            ThinkingLevel::High,
            ThinkingLevel::Xhigh,
        ] {
            let c = thinking_level_color(lvl);
            assert!(set.insert(format!("{c:?}")), "duplicate colour for {lvl:?}");
        }
    }

    #[test]
    fn thinking_level_labels_are_distinct() {
        assert_eq!(thinking_level_label(ThinkingLevel::Off), "off");
        assert_eq!(thinking_level_label(ThinkingLevel::Xhigh), "xhigh");
    }
}
