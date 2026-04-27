//! Interactive terminal UI for the harness loop.
//!
//! Embeds [`harness_runtime::run_loop`] in a ratatui event loop. The loop
//! emits AgentEvents into a channel; the renderer drains the channel each
//! tick and updates the UI state (message scrollback, tool panel, status
//! bar). Keyboard input editor below the scrollback enqueues steering or
//! follow-up messages on the runtime.

pub mod app;
pub mod bash;
pub mod clipboard;
pub mod fuzzy;
pub mod image;
pub mod input;
pub mod keybindings;
pub mod markdown;
pub mod render;
pub mod sink;
pub mod slash;
pub mod slots;
pub mod theme;
pub mod watcher;

pub use app::{
    App, AppStatus, ImagePayload, MessageRole, PendingAttachment, RenderedMessage,
    RenderedToolCall, RuntimeHandle, ToolState, TreeFilter,
};
pub use fuzzy::FuzzyIndex;
pub use image::{detect_protocol, ImageProtocol};
pub use input::EditorBuffer;
pub use keybindings::{KeyAction, Keybinding, KeybindingsFile, KeybindingsManager};
pub use render::{EscapeJob, PostDrawEscapes};
pub use sink::ChannelSink;
pub use slash::{parse_slash, ParsedSlash, SlashCommandRegistry, SlashEntry};
pub use slots::{BuiltinStatus, BuiltinWidget, SlotRegistry, StatusPosition};
pub use theme::{Theme, ThemeColors, ThemeError, ThemeModifiers};
pub use watcher::{ConfigReloadEvent, ConfigWatcher};
