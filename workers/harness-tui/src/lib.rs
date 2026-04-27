//! Interactive terminal UI for the harness loop.
//!
//! Embeds [`harness_runtime::run_loop`] in a ratatui event loop. The loop
//! emits AgentEvents into a channel; the renderer drains the channel each
//! tick and updates the UI state (message scrollback, tool panel, status
//! bar). Keyboard input editor below the scrollback enqueues steering or
//! follow-up messages on the runtime.

pub mod app;
pub mod bash;
pub mod fuzzy;
pub mod input;
pub mod render;
pub mod sink;
pub mod slash;
pub mod theme;

pub use app::{
    App, AppStatus, MessageRole, RenderedMessage, RenderedToolCall, RuntimeHandle, ToolState,
};
pub use fuzzy::FuzzyIndex;
pub use input::EditorBuffer;
pub use sink::ChannelSink;
pub use slash::{parse_slash, ParsedSlash, SlashCommandRegistry, SlashEntry};
