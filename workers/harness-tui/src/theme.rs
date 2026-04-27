//! Colour palette + reusable style helpers used by the renderer.
//!
//! Centralised here so the terminal palette is easy to tweak in one place.

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
