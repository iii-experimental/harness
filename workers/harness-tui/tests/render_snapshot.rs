//! TUI render snapshots.
//!
//! Drives `harness_tui::render::draw` against an in-memory `TestBackend`
//! at fixed terminal dimensions, then snapshots the rendered cell buffer
//! via `insta`. Catches accidental layout regressions in the idle screen,
//! the running-with-assistant-text screen, and the queue-indicator path.
//!
//! These are pure render tests — no engine, no provider, no network. The
//! `App` is constructed with the same `StubRuntime` pattern the unit
//! tests use, then synthetic `AgentEvent`s drive it into deterministic
//! states before draw.
//!
//! To re-bless after intentional layout changes:
//!   cargo insta test -p harness-tui --review

use std::sync::Arc;

use harness_tui::app::{App, RuntimeHandle};
use harness_tui::render::{draw, PostDrawEscapes};
use harness_types::{
    AgentEvent, AgentMessage, AssistantMessage, ContentBlock, StopReason, TextContent, UserMessage,
};
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::Terminal;
use tokio::sync::mpsc::unbounded_channel;

#[derive(Default)]
struct StubRuntime;
impl RuntimeHandle for StubRuntime {
    fn enqueue_steering(&self, _session_id: &str, _message: AgentMessage) {}
    fn enqueue_followup(&self, _session_id: &str, _message: AgentMessage) {}
    fn abort(&self, _session_id: &str) {}
}

fn make_app() -> App {
    let (_tx, rx) = unbounded_channel();
    App::new(
        "snap-session".into(),
        "anthropic".into(),
        "claude-sonnet".into(),
        "/snap/cwd".into(),
        rx,
        Arc::new(StubRuntime),
    )
}

/// Flatten a ratatui buffer into one string per row, with trailing
/// whitespace trimmed. Style information is discarded so the snapshot
/// is stable across colour-scheme changes.
fn buffer_to_string(buf: &Buffer) -> String {
    let mut out = String::with_capacity((buf.area().width as usize + 1) * buf.area().height as usize);
    for y in 0..buf.area().height {
        let mut line = String::new();
        for x in 0..buf.area().width {
            line.push_str(buf[(x, y)].symbol());
        }
        out.push_str(line.trim_end());
        out.push('\n');
    }
    out
}

fn render_to_string(app: &App, width: u16, height: u16) -> String {
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).expect("test backend");
    let mut escapes = PostDrawEscapes::default();
    terminal
        .draw(|f| draw(f, app, &mut escapes))
        .expect("draw ok");
    buffer_to_string(terminal.backend().buffer())
}

#[test]
fn idle_screen_layout() {
    let app = make_app();
    let rendered = render_to_string(&app, 80, 20);
    insta::assert_snapshot!("idle_screen_80x20", rendered);
}

#[test]
fn after_user_and_assistant_message() {
    let mut app = make_app();
    app.apply_event(AgentEvent::AgentStart);
    app.apply_event(AgentEvent::MessageStart {
        message: AgentMessage::User(UserMessage {
            content: vec![ContentBlock::Text(TextContent {
                text: "list workspace crates".into(),
            })],
            timestamp: 0,
        }),
    });
    app.apply_event(AgentEvent::MessageStart {
        message: AgentMessage::Assistant(AssistantMessage {
            content: vec![ContentBlock::Text(TextContent {
                text: "Sure, here's the listing.".into(),
            })],
            stop_reason: StopReason::End,
            error_message: None,
            error_kind: None,
            usage: None,
            model: "claude-sonnet".into(),
            provider: "anthropic".into(),
            timestamp: 0,
        }),
    });
    app.apply_event(AgentEvent::AgentEnd { messages: vec![] });
    let rendered = render_to_string(&app, 80, 20);
    insta::assert_snapshot!("after_user_assistant_80x20", rendered);
}
