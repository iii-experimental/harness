//! Colour palette + reusable style helpers used by the renderer.
//!
//! In 0.1 themes were hardcoded constants. This module now ships a runtime
//! [`Theme`] struct that the renderer consults via methods on `&Theme`. Two
//! defaults are baked in (`dark`, `light`); user themes ship as TOML files in
//! `~/.harness/themes/<name>.toml`.
//!
//! The legacy free-function helpers (`user_style()` etc.) remain so the
//! existing render code can call them transparently — they delegate to a
//! global default (dark) palette. Future hot-reload moves the active theme
//! onto `App` and the renderer will switch to `app.theme.user_style()`.

use std::path::Path;

use harness_types::ThinkingLevel;
use ratatui::style::{Color, Modifier, Style};
use serde::{Deserialize, Serialize};

/// Backwards-compatible legacy user colour.
///
/// The renderer still calls into `theme::header_style()` etc. We keep these
/// populated from the dark palette so existing behaviour is preserved if no
/// `App.theme` is consulted.
pub const COLOR_USER: Color = Color::Cyan;
pub const COLOR_ASSISTANT: Color = Color::Reset;
pub const COLOR_TOOL_CALL: Color = Color::DarkGray;
pub const COLOR_TOOL_OK: Color = Color::Green;
pub const COLOR_TOOL_ERR: Color = Color::Red;
pub const COLOR_HEADER: Color = Color::Yellow;
pub const COLOR_STATUS: Color = Color::DarkGray;
pub const COLOR_NOTIFICATION: Color = Color::Magenta;

/// Errors raised when loading a theme from disk.
#[derive(Debug)]
pub enum ThemeError {
    Io(std::io::Error),
    Parse(String),
    NotFound(String),
}

impl std::fmt::Display for ThemeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "io: {e}"),
            Self::Parse(s) => write!(f, "parse: {s}"),
            Self::NotFound(s) => write!(f, "theme not found: {s}"),
        }
    }
}

impl std::error::Error for ThemeError {}

impl From<std::io::Error> for ThemeError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

/// Bool toggles applied alongside foreground colours.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThemeModifiers {
    pub header_bold: bool,
    pub user_bold: bool,
}

impl Default for ThemeModifiers {
    fn default() -> Self {
        Self {
            header_bold: true,
            user_bold: true,
        }
    }
}

/// Foreground colours per role. Stored as `RatColor` so TOML can deserialize
/// hex strings ("#RRGGBB") or named colours ("Cyan").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ThemeColors {
    pub user: Color,
    pub assistant: Color,
    pub tool_call: Color,
    pub tool_ok: Color,
    pub tool_err: Color,
    pub header: Color,
    pub status: Color,
    pub notification: Color,
    pub thinking_off: Color,
    pub thinking_minimal: Color,
    pub thinking_low: Color,
    pub thinking_medium: Color,
    pub thinking_high: Color,
    pub thinking_xhigh: Color,
    pub border_idle: Color,
    pub border_running: Color,
    pub border_aborted: Color,
    pub border_errored: Color,
}

/// Serde proxy for `ThemeColors` that accepts string colours.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ThemeColorsRaw {
    user: String,
    assistant: String,
    tool_call: String,
    tool_ok: String,
    tool_err: String,
    header: String,
    status: String,
    notification: String,
    thinking_off: String,
    thinking_minimal: String,
    thinking_low: String,
    thinking_medium: String,
    thinking_high: String,
    thinking_xhigh: String,
    border_idle: String,
    border_running: String,
    border_aborted: String,
    border_errored: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ThemeFile {
    name: String,
    #[serde(default)]
    modifiers: Option<ThemeModifiers>,
    colors: ThemeColorsRaw,
}

/// A complete loaded theme. Consumed by the renderer via the `*_style`
/// methods.
#[derive(Debug, Clone)]
pub struct Theme {
    pub name: String,
    pub colors: ThemeColors,
    pub modifiers: ThemeModifiers,
}

impl Default for Theme {
    fn default() -> Self {
        Self::dark_default()
    }
}

impl Theme {
    /// The baked-in dark palette. Matches the legacy hardcoded constants.
    pub fn dark_default() -> Self {
        Self {
            name: "dark".into(),
            colors: ThemeColors {
                user: Color::Cyan,
                assistant: Color::Reset,
                tool_call: Color::DarkGray,
                tool_ok: Color::Green,
                tool_err: Color::Red,
                header: Color::Yellow,
                status: Color::DarkGray,
                notification: Color::Magenta,
                thinking_off: Color::DarkGray,
                thinking_minimal: Color::Gray,
                thinking_low: Color::Cyan,
                thinking_medium: Color::Blue,
                thinking_high: Color::Magenta,
                thinking_xhigh: Color::Red,
                border_idle: Color::DarkGray,
                border_running: Color::Yellow,
                border_aborted: Color::Red,
                border_errored: Color::LightRed,
            },
            modifiers: ThemeModifiers::default(),
        }
    }

    /// Light-background palette: swap dark grays for darker text-friendly
    /// shades and dim the header.
    pub fn light_default() -> Self {
        Self {
            name: "light".into(),
            colors: ThemeColors {
                user: Color::Blue,
                assistant: Color::Black,
                tool_call: Color::Gray,
                tool_ok: Color::Green,
                tool_err: Color::Red,
                header: Color::DarkGray,
                status: Color::Gray,
                notification: Color::Magenta,
                thinking_off: Color::Gray,
                thinking_minimal: Color::DarkGray,
                thinking_low: Color::Blue,
                thinking_medium: Color::Cyan,
                thinking_high: Color::Magenta,
                thinking_xhigh: Color::Red,
                border_idle: Color::Gray,
                border_running: Color::DarkGray,
                border_aborted: Color::Red,
                border_errored: Color::LightRed,
            },
            modifiers: ThemeModifiers {
                header_bold: true,
                user_bold: false,
            },
        }
    }

    /// Load a theme by name from `~/.harness/themes/<name>.toml`. The two
    /// well-known names `dark` and `light` resolve to the baked defaults
    /// without touching disk.
    pub fn load_named(name: &str) -> Result<Self, ThemeError> {
        match name {
            "dark" => return Ok(Self::dark_default()),
            "light" => return Ok(Self::light_default()),
            _ => {}
        }
        let dir = themes_dir().ok_or_else(|| ThemeError::NotFound(name.to_string()))?;
        let path = dir.join(format!("{name}.toml"));
        if !path.exists() {
            return Err(ThemeError::NotFound(name.to_string()));
        }
        Self::load_from_path(&path)
    }

    /// Load + parse a theme from a specific TOML file.
    pub fn load_from_path(path: &Path) -> Result<Self, ThemeError> {
        let raw = std::fs::read_to_string(path)?;
        let parsed: ThemeFile =
            toml::from_str(&raw).map_err(|e| ThemeError::Parse(e.to_string()))?;
        let modifiers = parsed.modifiers.unwrap_or_default();
        let colors = ThemeColors {
            user: parse_color(&parsed.colors.user)?,
            assistant: parse_color(&parsed.colors.assistant)?,
            tool_call: parse_color(&parsed.colors.tool_call)?,
            tool_ok: parse_color(&parsed.colors.tool_ok)?,
            tool_err: parse_color(&parsed.colors.tool_err)?,
            header: parse_color(&parsed.colors.header)?,
            status: parse_color(&parsed.colors.status)?,
            notification: parse_color(&parsed.colors.notification)?,
            thinking_off: parse_color(&parsed.colors.thinking_off)?,
            thinking_minimal: parse_color(&parsed.colors.thinking_minimal)?,
            thinking_low: parse_color(&parsed.colors.thinking_low)?,
            thinking_medium: parse_color(&parsed.colors.thinking_medium)?,
            thinking_high: parse_color(&parsed.colors.thinking_high)?,
            thinking_xhigh: parse_color(&parsed.colors.thinking_xhigh)?,
            border_idle: parse_color(&parsed.colors.border_idle)?,
            border_running: parse_color(&parsed.colors.border_running)?,
            border_aborted: parse_color(&parsed.colors.border_aborted)?,
            border_errored: parse_color(&parsed.colors.border_errored)?,
        };
        Ok(Self {
            name: parsed.name,
            colors,
            modifiers,
        })
    }

    pub fn header_style(&self) -> Style {
        let mut s = Style::default().fg(self.colors.header);
        if self.modifiers.header_bold {
            s = s.add_modifier(Modifier::BOLD);
        }
        s
    }

    pub fn user_style(&self) -> Style {
        let mut s = Style::default().fg(self.colors.user);
        if self.modifiers.user_bold {
            s = s.add_modifier(Modifier::BOLD);
        }
        s
    }

    pub fn assistant_style(&self) -> Style {
        Style::default().fg(self.colors.assistant)
    }

    pub fn tool_call_style(&self) -> Style {
        Style::default().fg(self.colors.tool_call)
    }

    pub fn tool_ok_style(&self) -> Style {
        Style::default().fg(self.colors.tool_ok)
    }

    pub fn tool_err_style(&self) -> Style {
        Style::default().fg(self.colors.tool_err)
    }

    pub fn status_style(&self) -> Style {
        Style::default().fg(self.colors.status)
    }

    pub fn notification_style(&self) -> Style {
        Style::default()
            .fg(self.colors.notification)
            .add_modifier(Modifier::ITALIC)
    }

    pub fn thinking_style(&self) -> Style {
        Style::default()
            .fg(self.colors.thinking_off)
            .add_modifier(Modifier::ITALIC | Modifier::DIM)
    }

    pub fn spinner_style(&self) -> Style {
        Style::default()
            .fg(self.colors.header)
            .add_modifier(Modifier::BOLD)
    }

    pub fn queue_style(&self) -> Style {
        Style::default()
            .fg(self.colors.notification)
            .add_modifier(Modifier::BOLD)
    }

    pub fn thinking_level_color(&self, level: ThinkingLevel) -> Color {
        match level {
            ThinkingLevel::Off => self.colors.thinking_off,
            ThinkingLevel::Minimal => self.colors.thinking_minimal,
            ThinkingLevel::Low => self.colors.thinking_low,
            ThinkingLevel::Medium => self.colors.thinking_medium,
            ThinkingLevel::High => self.colors.thinking_high,
            ThinkingLevel::Xhigh => self.colors.thinking_xhigh,
        }
    }
}

fn themes_dir() -> Option<std::path::PathBuf> {
    let home = directories::UserDirs::new()?;
    Some(home.home_dir().join(".harness").join("themes"))
}

/// Accept hex (`#RRGGBB`) or named ratatui colours (case-insensitive). Empty
/// or whitespace input maps to `Color::Reset`.
fn parse_color(s: &str) -> Result<Color, ThemeError> {
    let s = s.trim();
    if s.is_empty() || s.eq_ignore_ascii_case("reset") {
        return Ok(Color::Reset);
    }
    if let Some(hex) = s.strip_prefix('#') {
        if hex.len() != 6 {
            return Err(ThemeError::Parse(format!("hex must be 6 digits: {s}")));
        }
        let r = u8::from_str_radix(&hex[0..2], 16)
            .map_err(|e| ThemeError::Parse(format!("bad red byte in {s}: {e}")))?;
        let g = u8::from_str_radix(&hex[2..4], 16)
            .map_err(|e| ThemeError::Parse(format!("bad green byte in {s}: {e}")))?;
        let b = u8::from_str_radix(&hex[4..6], 16)
            .map_err(|e| ThemeError::Parse(format!("bad blue byte in {s}: {e}")))?;
        return Ok(Color::Rgb(r, g, b));
    }
    let lower = s.to_ascii_lowercase();
    let c = match lower.as_str() {
        "black" => Color::Black,
        "red" => Color::Red,
        "green" => Color::Green,
        "yellow" => Color::Yellow,
        "blue" => Color::Blue,
        "magenta" => Color::Magenta,
        "cyan" => Color::Cyan,
        "gray" | "grey" => Color::Gray,
        "darkgray" | "darkgrey" | "dark_gray" | "dark_grey" => Color::DarkGray,
        "lightred" | "light_red" => Color::LightRed,
        "lightgreen" | "light_green" => Color::LightGreen,
        "lightyellow" | "light_yellow" => Color::LightYellow,
        "lightblue" | "light_blue" => Color::LightBlue,
        "lightmagenta" | "light_magenta" => Color::LightMagenta,
        "lightcyan" | "light_cyan" => Color::LightCyan,
        "white" => Color::White,
        _ => return Err(ThemeError::Parse(format!("unknown colour: {s}"))),
    };
    Ok(c)
}

// ---- Legacy helpers (kept for backwards compat with existing renderer) ----

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
    use std::io::Write;

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

    #[test]
    fn dark_default_has_distinct_colors() {
        let t = Theme::dark_default();
        // user / header / tool_ok / tool_err should not collide.
        assert_ne!(t.colors.user, t.colors.header);
        assert_ne!(t.colors.tool_ok, t.colors.tool_err);
        assert_eq!(t.name, "dark");
    }

    #[test]
    fn light_default_has_distinct_colors() {
        let t = Theme::light_default();
        assert_ne!(t.colors.user, t.colors.header);
        assert_eq!(t.name, "light");
        // Light theme uses Black for assistant text.
        assert_eq!(t.colors.assistant, Color::Black);
    }

    #[test]
    fn load_from_path_parses_hex_colors() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("midnight.toml");
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(
            f,
            r##"name = "midnight"

[modifiers]
header_bold = true
user_bold = false

[colors]
user = "#00ffff"
assistant = "#e0e0e0"
tool_call = "#888888"
tool_ok = "#00ff00"
tool_err = "#ff0000"
header = "#ffaa00"
status = "#666666"
notification = "#ff00ff"
thinking_off = "#444444"
thinking_minimal = "#888888"
thinking_low = "#00aaff"
thinking_medium = "#0044ff"
thinking_high = "#aa00ff"
thinking_xhigh = "#ff0066"
border_idle = "#444444"
border_running = "#ffaa00"
border_aborted = "#ff0000"
border_errored = "#ff4444"
"##
        )
        .unwrap();
        let t = Theme::load_from_path(&path).expect("parse");
        assert_eq!(t.name, "midnight");
        assert_eq!(t.colors.user, Color::Rgb(0x00, 0xff, 0xff));
        assert_eq!(t.colors.assistant, Color::Rgb(0xe0, 0xe0, 0xe0));
        assert!(!t.modifiers.user_bold);
    }

    #[test]
    fn load_from_path_parses_named_colors() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("named.toml");
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(
            f,
            r#"name = "named"

[colors]
user = "Cyan"
assistant = "Reset"
tool_call = "DarkGray"
tool_ok = "Green"
tool_err = "Red"
header = "Yellow"
status = "DarkGray"
notification = "Magenta"
thinking_off = "DarkGray"
thinking_minimal = "Gray"
thinking_low = "Cyan"
thinking_medium = "Blue"
thinking_high = "Magenta"
thinking_xhigh = "Red"
border_idle = "DarkGray"
border_running = "Yellow"
border_aborted = "Red"
border_errored = "LightRed"
"#
        )
        .unwrap();
        let t = Theme::load_from_path(&path).expect("parse");
        assert_eq!(t.colors.user, Color::Cyan);
        assert_eq!(t.colors.assistant, Color::Reset);
        assert_eq!(t.colors.border_errored, Color::LightRed);
    }

    #[test]
    fn theme_apply_to_user_message_yields_correct_style() {
        let t = Theme::dark_default();
        let s = t.user_style();
        assert_eq!(s.fg, Some(Color::Cyan));
        let mods = s.add_modifier;
        assert!(mods.contains(Modifier::BOLD));
    }

    #[test]
    fn load_named_returns_baked_defaults_without_io() {
        let dark = Theme::load_named("dark").expect("dark");
        assert_eq!(dark.name, "dark");
        let light = Theme::load_named("light").expect("light");
        assert_eq!(light.name, "light");
    }

    #[test]
    fn parse_color_rejects_bad_hex() {
        assert!(parse_color("#zzzzzz").is_err());
        assert!(parse_color("#abc").is_err()); // 3-digit not supported
        assert!(parse_color("not-a-colour").is_err());
    }
}
