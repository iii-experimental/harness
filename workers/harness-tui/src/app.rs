//! UI state machine. Folds `AgentEvent` values into the `App` struct and
//! exposes the small `RuntimeHandle` trait that the input layer calls when the
//! user submits, steers, or aborts a run.

use std::sync::Arc;

use harness_types::{AgentEvent, AgentMessage, ContentBlock, Usage};
use tokio::sync::mpsc::UnboundedReceiver;

use crate::input::EditorBuffer;

/// High-level loop state surfaced in the status bar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppStatus {
    Idle,
    Running,
    Aborted,
    Errored(String),
}

/// Origin of a rendered scrollback line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageRole {
    User,
    Assistant,
    ToolResult,
    Notification,
}

/// One scrollback entry. The renderer walks `messages` and emits styled lines.
#[derive(Debug, Clone)]
pub struct RenderedMessage {
    pub role: MessageRole,
    pub text: String,
    pub timestamp: i64,
}

/// State of an in-flight tool call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolState {
    Pending,
    Running,
    Done,
    Error,
}

/// Mirror of a tool call inserted into the scrollback so the renderer can
/// style call args + result preview together.
#[derive(Debug, Clone)]
pub struct RenderedToolCall {
    pub tool_call_id: String,
    pub tool_name: String,
    pub args: serde_json::Value,
    pub state: ToolState,
    pub result_preview: Option<String>,
}

/// What the input layer needs to talk back to the runtime. Implemented by the
/// binary against `MemoryRuntime` (or any other `LoopRuntime` flavour).
pub trait RuntimeHandle: Send + Sync {
    fn enqueue_steering(&self, session_id: &str, message: AgentMessage);
    fn enqueue_followup(&self, session_id: &str, message: AgentMessage);
    fn abort(&self, session_id: &str);
}

/// Holds every piece of state the renderer needs.
pub struct App {
    pub session_id: String,
    pub provider_name: String,
    pub model: String,
    pub cwd: String,
    pub messages: Vec<RenderedMessage>,
    pub tool_calls: Vec<RenderedToolCall>,
    pub status: AppStatus,
    pub usage: Usage,
    pub turn_count: u32,
    pub editor: EditorBuffer,
    pub scroll_offset: u16,
    pub history: Vec<String>,
    pub history_cursor: Option<usize>,
    pub tool_truncate: bool,
    pub event_rx: UnboundedReceiver<AgentEvent>,
    pub runtime: Arc<dyn RuntimeHandle>,
    pub should_quit: bool,
    pub context_window: u64,
}

impl App {
    pub fn new(
        session_id: String,
        provider_name: String,
        model: String,
        cwd: String,
        event_rx: UnboundedReceiver<AgentEvent>,
        runtime: Arc<dyn RuntimeHandle>,
    ) -> Self {
        Self {
            session_id,
            provider_name,
            model,
            cwd,
            messages: Vec::new(),
            tool_calls: Vec::new(),
            status: AppStatus::Idle,
            usage: Usage::default(),
            turn_count: 0,
            editor: EditorBuffer::new(),
            scroll_offset: 0,
            history: Vec::new(),
            history_cursor: None,
            tool_truncate: true,
            event_rx,
            runtime,
            should_quit: false,
            context_window: 200_000,
        }
    }

    /// Drain every event currently waiting in the channel. Called once per
    /// tick; never blocks.
    pub fn drain_events(&mut self) {
        while let Ok(ev) = self.event_rx.try_recv() {
            self.apply_event(ev);
        }
    }

    /// Fold one event into the UI state. Public so unit tests can drive it
    /// directly without an `mpsc`.
    pub fn apply_event(&mut self, ev: AgentEvent) {
        match ev {
            AgentEvent::AgentStart => {
                self.status = AppStatus::Running;
            }
            AgentEvent::AgentEnd { .. } => {
                if !matches!(self.status, AppStatus::Aborted | AppStatus::Errored(_)) {
                    self.status = AppStatus::Idle;
                }
            }
            AgentEvent::TurnStart => {}
            AgentEvent::TurnEnd { .. } => {
                self.turn_count = self.turn_count.saturating_add(1);
            }
            AgentEvent::MessageStart { message } => {
                self.push_message(&message);
            }
            AgentEvent::MessageUpdate { .. } => {
                // 0.1 renders the final assistant message at MessageStart time
                // rather than streaming deltas. Streamed deltas land in a later
                // iteration once we add a "live message" placeholder.
            }
            AgentEvent::MessageEnd { .. } => {}
            AgentEvent::ToolExecutionStart {
                tool_call_id,
                tool_name,
                args,
            } => {
                self.tool_calls.push(RenderedToolCall {
                    tool_call_id,
                    tool_name,
                    args,
                    state: ToolState::Running,
                    result_preview: None,
                });
            }
            AgentEvent::ToolExecutionUpdate { .. } => {}
            AgentEvent::ToolExecutionEnd {
                tool_call_id,
                is_error,
                result,
                ..
            } => {
                let preview = result
                    .content
                    .iter()
                    .find_map(|c| match c {
                        ContentBlock::Text(t) => Some(t.text.clone()),
                        _ => None,
                    })
                    .unwrap_or_default();
                let preview_short: String = preview.chars().take(160).collect();
                if let Some(tc) = self
                    .tool_calls
                    .iter_mut()
                    .rev()
                    .find(|t| t.tool_call_id == tool_call_id)
                {
                    tc.state = if is_error {
                        ToolState::Error
                    } else {
                        ToolState::Done
                    };
                    tc.result_preview = Some(preview_short.clone());
                }
                let role = if is_error {
                    MessageRole::Notification
                } else {
                    MessageRole::ToolResult
                };
                self.messages.push(RenderedMessage {
                    role,
                    text: format!(
                        "    [tool {}] {}",
                        if is_error { "err" } else { "ok" },
                        preview_short
                    ),
                    timestamp: chrono::Utc::now().timestamp_millis(),
                });
            }
        }
    }

    fn push_message(&mut self, message: &AgentMessage) {
        match message {
            AgentMessage::User(u) => {
                let text = collect_text(&u.content);
                if !text.is_empty() {
                    self.messages.push(RenderedMessage {
                        role: MessageRole::User,
                        text: format!(">>> user: {text}"),
                        timestamp: u.timestamp,
                    });
                }
            }
            AgentMessage::Assistant(a) => {
                let text = collect_text(&a.content);
                if !text.is_empty() {
                    self.messages.push(RenderedMessage {
                        role: MessageRole::Assistant,
                        text: format!("<<< assistant: {text}"),
                        timestamp: a.timestamp,
                    });
                }
                for c in &a.content {
                    if let ContentBlock::ToolCall {
                        name, arguments, ..
                    } = c
                    {
                        self.messages.push(RenderedMessage {
                            role: MessageRole::ToolResult,
                            text: format!("    -> tool call: {name}({arguments})"),
                            timestamp: a.timestamp,
                        });
                    }
                }
                if let Some(usage) = a.usage {
                    self.usage.input = self.usage.input.saturating_add(usage.input);
                    self.usage.output = self.usage.output.saturating_add(usage.output);
                    self.usage.cache_read = self.usage.cache_read.saturating_add(usage.cache_read);
                    self.usage.cache_write =
                        self.usage.cache_write.saturating_add(usage.cache_write);
                    self.usage.cost_usd = match (self.usage.cost_usd, usage.cost_usd) {
                        (Some(a), Some(b)) => Some(a + b),
                        (Some(a), None) => Some(a),
                        (None, Some(b)) => Some(b),
                        _ => None,
                    };
                }
            }
            AgentMessage::ToolResult(_) => {
                // Already rendered via ToolExecutionEnd; skipping avoids
                // double entries.
            }
            AgentMessage::Custom(c) => {
                if let Some(d) = &c.display {
                    self.messages.push(RenderedMessage {
                        role: MessageRole::Notification,
                        text: d.clone(),
                        timestamp: c.timestamp,
                    });
                }
            }
        }
    }

    /// Submit the editor buffer as a user message. If idle, returns the text
    /// for the binary to inject as the run's initial prompt; if running, the
    /// message is queued onto the runtime's steering channel.
    ///
    /// Returns `Some(text)` only when the loop is idle (caller starts a run).
    pub fn submit_message(&mut self) -> Option<String> {
        let text = self.editor.take();
        if text.trim().is_empty() {
            return None;
        }
        self.history.push(text.clone());
        self.history_cursor = None;

        match &self.status {
            AppStatus::Idle | AppStatus::Aborted | AppStatus::Errored(_) => Some(text),
            AppStatus::Running => {
                self.runtime
                    .enqueue_steering(&self.session_id, user_message(&text));
                self.messages.push(RenderedMessage {
                    role: MessageRole::Notification,
                    text: format!("[steering queued] {text}"),
                    timestamp: chrono::Utc::now().timestamp_millis(),
                });
                None
            }
        }
    }

    /// Submit as follow-up — agent finishes the current run, then processes.
    pub fn submit_followup(&mut self) -> Option<String> {
        let text = self.editor.take();
        if text.trim().is_empty() {
            return None;
        }
        self.history.push(text.clone());
        self.history_cursor = None;

        match &self.status {
            AppStatus::Idle | AppStatus::Aborted | AppStatus::Errored(_) => Some(text),
            AppStatus::Running => {
                self.runtime
                    .enqueue_followup(&self.session_id, user_message(&text));
                self.messages.push(RenderedMessage {
                    role: MessageRole::Notification,
                    text: format!("[follow-up queued] {text}"),
                    timestamp: chrono::Utc::now().timestamp_millis(),
                });
                None
            }
        }
    }

    /// `Esc` semantics: clear the editor if it has content; otherwise abort
    /// any running session. Called by input.
    pub fn handle_escape(&mut self) {
        if !self.editor.is_empty() {
            self.editor.clear();
            return;
        }
        if matches!(self.status, AppStatus::Running) {
            self.runtime.abort(&self.session_id);
            self.status = AppStatus::Aborted;
            self.messages.push(RenderedMessage {
                role: MessageRole::Notification,
                text: "[abort signalled]".into(),
                timestamp: chrono::Utc::now().timestamp_millis(),
            });
        }
    }

    pub fn clear_scrollback(&mut self) {
        self.messages.clear();
        self.tool_calls.clear();
        self.scroll_offset = 0;
    }

    pub fn toggle_tool_truncation(&mut self) {
        self.tool_truncate = !self.tool_truncate;
    }

    pub fn scroll_up(&mut self, n: u16) {
        self.scroll_offset = self.scroll_offset.saturating_add(n);
    }

    pub fn scroll_down(&mut self, n: u16) {
        self.scroll_offset = self.scroll_offset.saturating_sub(n);
    }

    /// Walk back through submitted-history into the editor.
    pub fn history_prev(&mut self) {
        if self.history.is_empty() {
            return;
        }
        let next = match self.history_cursor {
            None => self.history.len() - 1,
            Some(0) => 0,
            Some(i) => i - 1,
        };
        self.history_cursor = Some(next);
        self.editor.set(self.history[next].clone());
    }

    pub fn history_next(&mut self) {
        match self.history_cursor {
            None => {}
            Some(i) if i + 1 >= self.history.len() => {
                self.history_cursor = None;
                self.editor.clear();
            }
            Some(i) => {
                self.history_cursor = Some(i + 1);
                self.editor.set(self.history[i + 1].clone());
            }
        }
    }

    /// Status-bar line. Pulled out so the renderer can right-align it.
    pub fn status_line(&self) -> String {
        let cost = self
            .usage
            .cost_usd
            .map_or_else(|| "$-".to_string(), |c| format!("${c:.4}"));
        let total_tokens = self.usage.input.saturating_add(self.usage.output);
        let ctx = format!(
            "ctx {}k/{}k",
            total_tokens / 1000,
            self.context_window / 1000
        );
        let status = match &self.status {
            AppStatus::Idle => "idle".to_string(),
            AppStatus::Running => "running".to_string(),
            AppStatus::Aborted => "aborted".to_string(),
            AppStatus::Errored(e) => format!("error: {e}"),
        };
        format!(
            "{} turns · ↑{} ↓{} · {} · {} · {}",
            self.turn_count, self.usage.input, self.usage.output, cost, ctx, status
        )
    }
}

fn collect_text(blocks: &[ContentBlock]) -> String {
    blocks
        .iter()
        .filter_map(|b| match b {
            ContentBlock::Text(t) => Some(t.text.clone()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Wrap a string into a `User` `AgentMessage` with the current timestamp.
pub fn user_message(text: &str) -> AgentMessage {
    AgentMessage::User(harness_types::UserMessage {
        content: vec![ContentBlock::Text(harness_types::TextContent {
            text: text.to_string(),
        })],
        timestamp: chrono::Utc::now().timestamp_millis(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use harness_types::{
        AssistantMessage, ContentBlock, StopReason, TextContent, ToolResult, UserMessage,
    };
    use std::sync::Mutex;
    use tokio::sync::mpsc::unbounded_channel;

    #[derive(Default)]
    struct StubRuntime {
        steering: Mutex<Vec<(String, AgentMessage)>>,
        followup: Mutex<Vec<(String, AgentMessage)>>,
        aborts: Mutex<Vec<String>>,
    }

    impl RuntimeHandle for StubRuntime {
        fn enqueue_steering(&self, session_id: &str, message: AgentMessage) {
            self.steering
                .lock()
                .unwrap()
                .push((session_id.to_string(), message));
        }
        fn enqueue_followup(&self, session_id: &str, message: AgentMessage) {
            self.followup
                .lock()
                .unwrap()
                .push((session_id.to_string(), message));
        }
        fn abort(&self, session_id: &str) {
            self.aborts.lock().unwrap().push(session_id.to_string());
        }
    }

    fn make_app() -> (App, Arc<StubRuntime>) {
        let (_tx, rx) = unbounded_channel();
        let rt = Arc::new(StubRuntime::default());
        let app = App::new(
            "s1".into(),
            "anthropic".into(),
            "claude".into(),
            "/tmp".into(),
            rx,
            rt.clone(),
        );
        (app, rt)
    }

    #[test]
    fn agent_start_sets_running() {
        let (mut app, _) = make_app();
        app.apply_event(AgentEvent::AgentStart);
        assert_eq!(app.status, AppStatus::Running);
    }

    #[test]
    fn agent_end_sets_idle() {
        let (mut app, _) = make_app();
        app.apply_event(AgentEvent::AgentStart);
        app.apply_event(AgentEvent::AgentEnd {
            messages: Vec::new(),
        });
        assert_eq!(app.status, AppStatus::Idle);
    }

    #[test]
    fn message_start_pushes_user_to_scrollback() {
        let (mut app, _) = make_app();
        let msg = AgentMessage::User(UserMessage {
            content: vec![ContentBlock::Text(TextContent { text: "hi".into() })],
            timestamp: 1,
        });
        app.apply_event(AgentEvent::MessageStart { message: msg });
        assert_eq!(app.messages.len(), 1);
        assert_eq!(app.messages[0].role, MessageRole::User);
        assert!(app.messages[0].text.contains("hi"));
    }

    #[test]
    fn assistant_with_tool_call_renders_both_lines() {
        let (mut app, _) = make_app();
        let assistant = AssistantMessage {
            content: vec![
                ContentBlock::Text(TextContent {
                    text: "calling tool".into(),
                }),
                ContentBlock::ToolCall {
                    id: "c1".into(),
                    name: "read".into(),
                    arguments: serde_json::json!({"path": "/x"}),
                },
            ],
            stop_reason: StopReason::Tool,
            error_message: None,
            error_kind: None,
            usage: None,
            model: "m".into(),
            provider: "p".into(),
            timestamp: 5,
        };
        app.apply_event(AgentEvent::MessageStart {
            message: AgentMessage::Assistant(assistant),
        });
        assert_eq!(app.messages.len(), 2);
        assert_eq!(app.messages[0].role, MessageRole::Assistant);
        assert!(app.messages[1].text.contains("tool call"));
    }

    #[test]
    fn tool_execution_end_marks_done_and_pushes_preview() {
        let (mut app, _) = make_app();
        app.apply_event(AgentEvent::ToolExecutionStart {
            tool_call_id: "c1".into(),
            tool_name: "read".into(),
            args: serde_json::json!({}),
        });
        app.apply_event(AgentEvent::ToolExecutionEnd {
            tool_call_id: "c1".into(),
            tool_name: "read".into(),
            result: ToolResult {
                content: vec![ContentBlock::Text(TextContent {
                    text: "file contents".into(),
                })],
                details: serde_json::json!({}),
                terminate: false,
            },
            is_error: false,
        });
        assert_eq!(app.tool_calls.len(), 1);
        assert_eq!(app.tool_calls[0].state, ToolState::Done);
        assert!(app
            .messages
            .last()
            .is_some_and(|m| m.text.contains("tool ok")));
    }

    #[test]
    fn turn_end_increments_count() {
        let (mut app, _) = make_app();
        let assistant = AssistantMessage {
            content: vec![],
            stop_reason: StopReason::End,
            error_message: None,
            error_kind: None,
            usage: None,
            model: "m".into(),
            provider: "p".into(),
            timestamp: 1,
        };
        app.apply_event(AgentEvent::TurnEnd {
            message: AgentMessage::Assistant(assistant),
            tool_results: Vec::new(),
        });
        assert_eq!(app.turn_count, 1);
    }

    #[test]
    fn submit_idle_returns_text_and_records_history() {
        let (mut app, _) = make_app();
        app.editor.set("hello");
        let out = app.submit_message();
        assert_eq!(out.as_deref(), Some("hello"));
        assert_eq!(app.history, vec!["hello"]);
    }

    #[test]
    fn submit_running_queues_steering() {
        let (mut app, rt) = make_app();
        app.status = AppStatus::Running;
        app.editor.set("steer me");
        let out = app.submit_message();
        assert!(out.is_none());
        let g = rt.steering.lock().unwrap();
        assert_eq!(g.len(), 1);
        assert_eq!(g[0].0, "s1");
    }

    #[test]
    fn submit_followup_running_queues_followup() {
        let (mut app, rt) = make_app();
        app.status = AppStatus::Running;
        app.editor.set("after");
        let out = app.submit_followup();
        assert!(out.is_none());
        assert_eq!(rt.followup.lock().unwrap().len(), 1);
    }

    #[test]
    fn escape_clears_editor_first_then_aborts() {
        let (mut app, rt) = make_app();
        app.status = AppStatus::Running;
        app.editor.set("draft");
        app.handle_escape();
        assert!(app.editor.is_empty());
        assert_eq!(rt.aborts.lock().unwrap().len(), 0);
        app.handle_escape();
        assert_eq!(rt.aborts.lock().unwrap().len(), 1);
        assert_eq!(app.status, AppStatus::Aborted);
    }

    #[test]
    fn history_prev_pulls_last_submitted() {
        let (mut app, _) = make_app();
        app.editor.set("a");
        let _ = app.submit_message();
        app.editor.set("b");
        let _ = app.submit_message();
        app.history_prev();
        assert_eq!(app.editor.text(), "b");
        app.history_prev();
        assert_eq!(app.editor.text(), "a");
        app.history_next();
        assert_eq!(app.editor.text(), "b");
    }
}
