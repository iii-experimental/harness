//! UI state machine. Folds `AgentEvent` values into the `App` struct and
//! exposes the small `RuntimeHandle` trait that the input layer calls when the
//! user submits, steers, or aborts a run.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use harness_types::{AgentEvent, AgentMessage, ContentBlock, Usage};
use tokio::sync::mpsc::UnboundedReceiver;

use crate::fuzzy::FuzzyIndex;
use crate::input::EditorBuffer;
use crate::slash::{parse_slash, SlashCommandRegistry};

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
    Thinking,
    ToolResult,
    Notification,
}

/// One scrollback entry. The renderer walks `messages` and emits styled lines.
#[derive(Debug, Clone)]
pub struct RenderedMessage {
    pub role: MessageRole,
    pub text: String,
    pub timestamp: i64,
    /// Approximate token count for `MessageRole::Thinking` lines, otherwise `None`.
    pub thinking_token_count: Option<u32>,
    /// Tool-call id this scrollback entry is associated with (for `ToolResult`).
    pub tool_call_id: Option<String>,
    /// True for tool-result lines that came back as errors.
    pub is_error: bool,
}

impl RenderedMessage {
    fn plain(role: MessageRole, text: String, timestamp: i64) -> Self {
        Self {
            role,
            text,
            timestamp,
            thinking_token_count: None,
            tool_call_id: None,
            is_error: false,
        }
    }
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
    /// Full tool result body (only populated when the call ends).
    pub result_full: Option<String>,
    /// Per-call collapse state. Defaults to `true`.
    pub collapsed: bool,
}

/// What the input layer needs to talk back to the runtime. Implemented by the
/// binary against `MemoryRuntime` (or any other `LoopRuntime` flavour).
pub trait RuntimeHandle: Send + Sync {
    fn enqueue_steering(&self, session_id: &str, message: AgentMessage);
    fn enqueue_followup(&self, session_id: &str, message: AgentMessage);
    fn abort(&self, session_id: &str);
}

/// Outcome of routing a slash command. Used by the binary to decide whether to
/// quit, change cwd, etc.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SlashOutcome {
    Handled,
    Quit,
    Chdir(PathBuf),
    NotFound,
}

/// Holds every piece of state the renderer needs.
pub struct App {
    pub session_id: String,
    pub provider_name: String,
    pub model: String,
    pub session_name: Option<String>,
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
    /// When `true`, every tool result renders as a single-line preview.
    pub tools_collapsed: bool,
    /// When `false`, thinking blocks render as a single dim placeholder.
    pub expand_thinking: bool,
    /// Animated braille spinner index, advanced once per UI tick.
    pub spinner_frame: usize,
    /// Wall-clock timestamp of the last `AgentStart`. Cleared on `AgentEnd`.
    pub run_started_at: Option<Instant>,
    /// Tool name currently executing, if any.
    pub current_tool: Option<String>,
    /// Number of pending steering submissions queued during a live run.
    pub queued_steering_count: usize,
    /// Number of pending follow-up submissions queued during a live run.
    pub queued_followup_count: usize,
    pub event_rx: UnboundedReceiver<AgentEvent>,
    pub runtime: Arc<dyn RuntimeHandle>,
    pub should_quit: bool,
    pub context_window: u64,
    pub slash_registry: SlashCommandRegistry,
    pub command_picker_visible: bool,
    pub command_picker_filter: String,
    pub command_picker_index: usize,
    pub fuzzy_index: Option<FuzzyIndex>,
    pub file_picker_visible: bool,
    pub file_picker_query: String,
    pub file_picker_index: usize,
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
            session_name: None,
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
            tools_collapsed: true,
            expand_thinking: false,
            spinner_frame: 0,
            run_started_at: None,
            current_tool: None,
            queued_steering_count: 0,
            queued_followup_count: 0,
            event_rx,
            runtime,
            should_quit: false,
            context_window: 200_000,
            slash_registry: SlashCommandRegistry::new(),
            command_picker_visible: false,
            command_picker_filter: String::new(),
            command_picker_index: 0,
            fuzzy_index: None,
            file_picker_visible: false,
            file_picker_query: String::new(),
            file_picker_index: 0,
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
                self.run_started_at = Some(Instant::now());
            }
            AgentEvent::AgentEnd { .. } => {
                if !matches!(self.status, AppStatus::Aborted | AppStatus::Errored(_)) {
                    self.status = AppStatus::Idle;
                }
                self.run_started_at = None;
                self.current_tool = None;
                self.queued_steering_count = 0;
                self.queued_followup_count = 0;
            }
            AgentEvent::TurnStart => {}
            AgentEvent::TurnEnd { .. } => {
                self.turn_count = self.turn_count.saturating_add(1);
                if self.queued_steering_count > 0 {
                    self.queued_steering_count -= 1;
                }
                if self.queued_followup_count > 0 {
                    self.queued_followup_count -= 1;
                }
            }
            AgentEvent::MessageStart { message } => {
                self.push_message(&message);
            }
            AgentEvent::MessageUpdate { .. } => {}
            AgentEvent::MessageEnd { .. } => {}
            AgentEvent::ToolExecutionStart {
                tool_call_id,
                tool_name,
                args,
            } => {
                self.current_tool = Some(tool_name.clone());
                self.tool_calls.push(RenderedToolCall {
                    tool_call_id,
                    tool_name,
                    args,
                    state: ToolState::Running,
                    result_preview: None,
                    result_full: None,
                    collapsed: true,
                });
            }
            AgentEvent::ToolExecutionUpdate { .. } => {}
            AgentEvent::ToolExecutionEnd {
                tool_call_id,
                is_error,
                result,
                ..
            } => {
                let full = result
                    .content
                    .iter()
                    .find_map(|c| match c {
                        ContentBlock::Text(t) => Some(t.text.clone()),
                        _ => None,
                    })
                    .unwrap_or_default();
                let preview_short: String = full.chars().take(160).collect();
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
                    tc.result_full = Some(full);
                }
                self.current_tool = None;
                let role = if is_error {
                    MessageRole::Notification
                } else {
                    MessageRole::ToolResult
                };
                self.messages.push(RenderedMessage {
                    role,
                    text: format!(
                        "      [tool {}] {}",
                        if is_error { "err" } else { "ok" },
                        preview_short
                    ),
                    timestamp: chrono::Utc::now().timestamp_millis(),
                    thinking_token_count: None,
                    tool_call_id: Some(tool_call_id),
                    is_error,
                });
            }
        }
    }

    /// Advance the spinner one frame. Called once per UI tick by main.
    pub fn tick(&mut self) {
        self.spinner_frame = self.spinner_frame.wrapping_add(1);
    }

    /// Toggle the global tool-collapse flag.
    pub fn toggle_tools_collapsed(&mut self) {
        self.tools_collapsed = !self.tools_collapsed;
    }

    /// Toggle thinking-block expansion.
    pub fn toggle_expand_thinking(&mut self) {
        self.expand_thinking = !self.expand_thinking;
    }

    fn push_message(&mut self, message: &AgentMessage) {
        match message {
            AgentMessage::User(u) => {
                let text = collect_text(&u.content);
                if !text.is_empty() {
                    self.messages.push(RenderedMessage::plain(
                        MessageRole::User,
                        format!(">>> user: {text}"),
                        u.timestamp,
                    ));
                }
            }
            AgentMessage::Assistant(a) => {
                for c in &a.content {
                    if let ContentBlock::Thinking { text, .. } = c {
                        if !text.is_empty() {
                            let approx_tokens = u32::try_from(text.len() / 4).unwrap_or(u32::MAX);
                            self.messages.push(RenderedMessage {
                                role: MessageRole::Thinking,
                                text: text.clone(),
                                timestamp: a.timestamp,
                                thinking_token_count: Some(approx_tokens),
                                tool_call_id: None,
                                is_error: false,
                            });
                        }
                    }
                }
                let text = collect_text(&a.content);
                if !text.is_empty() {
                    self.messages.push(RenderedMessage::plain(
                        MessageRole::Assistant,
                        text,
                        a.timestamp,
                    ));
                }
                for c in &a.content {
                    if let ContentBlock::ToolCall {
                        name, arguments, ..
                    } = c
                    {
                        self.messages.push(RenderedMessage::plain(
                            MessageRole::ToolResult,
                            format!("      -> tool call: {name}({arguments})"),
                            a.timestamp,
                        ));
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
            AgentMessage::ToolResult(_) => {}
            AgentMessage::Custom(c) => {
                if let Some(d) = &c.display {
                    self.messages.push(RenderedMessage::plain(
                        MessageRole::Notification,
                        d.clone(),
                        c.timestamp,
                    ));
                }
            }
        }
    }

    /// Submit the editor buffer as a user message. If idle, returns the text
    /// for the binary to inject as the run's initial prompt; if running, the
    /// message is queued onto the runtime's steering channel.
    pub fn submit_message(&mut self) -> Option<String> {
        let text = self.editor.take_text();
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
                self.queued_steering_count = self.queued_steering_count.saturating_add(1);
                self.messages.push(RenderedMessage::plain(
                    MessageRole::Notification,
                    format!("[steering queued] {text}"),
                    chrono::Utc::now().timestamp_millis(),
                ));
                None
            }
        }
    }

    /// Submit text without consuming the editor (for inline-bash output).
    pub fn submit_text_as_user(&mut self, text: String) -> Option<String> {
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
                self.queued_steering_count = self.queued_steering_count.saturating_add(1);
                self.messages.push(RenderedMessage::plain(
                    MessageRole::Notification,
                    format!("[steering queued] {text}"),
                    chrono::Utc::now().timestamp_millis(),
                ));
                None
            }
        }
    }

    /// Submit as follow-up — agent finishes the current run, then processes.
    pub fn submit_followup(&mut self) -> Option<String> {
        let text = self.editor.take_text();
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
                self.queued_followup_count = self.queued_followup_count.saturating_add(1);
                self.messages.push(RenderedMessage::plain(
                    MessageRole::Notification,
                    format!("[follow-up queued] {text}"),
                    chrono::Utc::now().timestamp_millis(),
                ));
                None
            }
        }
    }

    /// `Esc` semantics: close any open picker first; clear the editor next;
    /// otherwise abort any running session.
    pub fn handle_escape(&mut self) {
        if self.command_picker_visible {
            self.command_picker_visible = false;
            return;
        }
        if self.file_picker_visible {
            self.file_picker_visible = false;
            return;
        }
        if !self.editor.is_empty() {
            self.editor.clear();
            return;
        }
        if matches!(self.status, AppStatus::Running) {
            self.runtime.abort(&self.session_id);
            self.status = AppStatus::Aborted;
            self.messages.push(RenderedMessage::plain(
                MessageRole::Notification,
                "[abort signalled]".into(),
                chrono::Utc::now().timestamp_millis(),
            ));
        }
    }

    pub fn clear_scrollback(&mut self) {
        self.messages.clear();
        self.tool_calls.clear();
        self.scroll_offset = 0;
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

    /// Push a notification line into scrollback.
    pub fn push_notification(&mut self, text: impl Into<String>) {
        self.messages.push(RenderedMessage::plain(
            MessageRole::Notification,
            text.into(),
            chrono::Utc::now().timestamp_millis(),
        ));
    }

    /// Compute the spinner glyph for the current frame.
    pub fn spinner_glyph(&self) -> char {
        const FRAMES: &[char] = &['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
        FRAMES[self.spinner_frame % FRAMES.len()]
    }

    /// Format `<elapsed>` for the running run as e.g. `0:08`. Returns empty
    /// when no run is in flight.
    pub fn elapsed_label(&self) -> String {
        let Some(start) = self.run_started_at else {
            return String::new();
        };
        let secs = start.elapsed().as_secs();
        let m = secs / 60;
        let s = secs % 60;
        format!("{m}:{s:02}")
    }

    /// Recompute command picker state from the current editor buffer. Pickers
    /// are only meaningful when the buffer is single-line and starts with `/`.
    pub fn refresh_command_picker(&mut self) {
        let text = self.editor.text();
        if !self.editor.is_multiline() && text.starts_with('/') {
            self.command_picker_visible = true;
            self.command_picker_filter.clone_from(&text);
            let n = self.slash_registry.match_prefix(&text).len().max(1);
            if self.command_picker_index >= n {
                self.command_picker_index = 0;
            }
        } else {
            self.command_picker_visible = false;
            self.command_picker_filter.clear();
            self.command_picker_index = 0;
        }
    }

    /// Recompute file picker state from the current editor buffer + cursor.
    pub fn refresh_file_picker(&mut self) {
        let line = self.editor.current_line();
        let col = self.editor.cursor_col();
        let prefix = &line[..col];
        if let Some(at_idx) = prefix.rfind('@') {
            let q = &prefix[at_idx + 1..];
            if q.chars().any(char::is_whitespace) {
                self.file_picker_visible = false;
                self.file_picker_query.clear();
                self.file_picker_index = 0;
                return;
            }
            self.file_picker_visible = true;
            self.file_picker_query = q.to_string();
            let limit = self
                .fuzzy_index
                .as_ref()
                .map_or(0, |idx| idx.r#match(&self.file_picker_query, 8).len());
            if self.file_picker_index >= limit.max(1) {
                self.file_picker_index = 0;
            }
        } else {
            self.file_picker_visible = false;
            self.file_picker_query.clear();
            self.file_picker_index = 0;
        }
    }

    pub fn picker_select_prev(&mut self) {
        if self.command_picker_visible {
            let n = self
                .slash_registry
                .match_prefix(&self.command_picker_filter)
                .len();
            if n > 0 {
                if self.command_picker_index == 0 {
                    self.command_picker_index = n - 1;
                } else {
                    self.command_picker_index -= 1;
                }
            }
        } else if self.file_picker_visible {
            let n = self
                .fuzzy_index
                .as_ref()
                .map_or(0, |idx| idx.r#match(&self.file_picker_query, 8).len());
            if n > 0 {
                if self.file_picker_index == 0 {
                    self.file_picker_index = n - 1;
                } else {
                    self.file_picker_index -= 1;
                }
            }
        }
    }

    pub fn picker_select_next(&mut self) {
        if self.command_picker_visible {
            let n = self
                .slash_registry
                .match_prefix(&self.command_picker_filter)
                .len();
            if n > 0 {
                self.command_picker_index = (self.command_picker_index + 1) % n;
            }
        } else if self.file_picker_visible {
            let n = self
                .fuzzy_index
                .as_ref()
                .map_or(0, |idx| idx.r#match(&self.file_picker_query, 8).len());
            if n > 0 {
                self.file_picker_index = (self.file_picker_index + 1) % n;
            }
        }
    }

    /// Tab-complete the slash command picker. Returns `true` when a completion
    /// was applied.
    pub fn complete_slash(&mut self) -> bool {
        let cur = self.editor.text();
        if !cur.starts_with('/') || self.editor.is_multiline() {
            return false;
        }
        if let Some(name) = self.slash_registry.complete(&cur) {
            let target = format!("/{name}");
            if target != cur {
                self.editor.set(target);
                self.refresh_command_picker();
                return true;
            }
        }
        false
    }

    /// Tab-complete the highlighted file picker entry.
    pub fn complete_file(&mut self) -> bool {
        if !self.file_picker_visible {
            return false;
        }
        let pick = {
            let idx = match self.fuzzy_index.as_ref() {
                Some(i) => i,
                None => return false,
            };
            let hits = idx.r#match(&self.file_picker_query, 8);
            if hits.is_empty() {
                return false;
            }
            let n = self.file_picker_index.min(hits.len() - 1);
            hits[n].0.to_string_lossy().to_string()
        };

        let line = self.editor.current_line().to_string();
        let col = self.editor.cursor_col();
        let prefix = &line[..col];
        let at_idx = match prefix.rfind('@') {
            Some(i) => i,
            None => return false,
        };

        let new_line = format!("{}@{} {}", &line[..at_idx], pick, &line[col..]);
        let new_col = at_idx + 1 + pick.len() + 1;

        let mut full_lines: Vec<String> = self.editor.lines().to_vec();
        full_lines[self.editor.cursor_row()] = new_line;
        let row = self.editor.cursor_row();
        self.editor.set(full_lines.join("\n"));
        // restore cursor on row.
        for _ in 0..row {
            self.editor.move_down();
        }
        self.editor.home();
        for _ in 0..new_col {
            self.editor.move_right();
        }

        self.file_picker_visible = false;
        self.file_picker_query.clear();
        self.file_picker_index = 0;
        true
    }

    /// Route a slash command. Mutates `App` for the simple ones; returns
    /// `Chdir`/`Quit` for ones the caller has to action against the process.
    pub fn route_slash(&mut self, line: &str) -> SlashOutcome {
        let parsed = match parse_slash(line) {
            Some(p) => p,
            None => return SlashOutcome::NotFound,
        };
        let entry = match self.slash_registry.get(&parsed.name) {
            Some(e) => e,
            None => {
                self.push_notification(format!("[slash] unknown command: /{}", parsed.name));
                return SlashOutcome::NotFound;
            }
        };
        if !entry.implemented {
            self.push_notification(format!(
                "[slash] /{} not yet implemented in this build",
                parsed.name
            ));
            return SlashOutcome::Handled;
        }

        match parsed.name.as_str() {
            "help" | "hotkeys" => {
                for line in HOTKEY_LINES {
                    self.push_notification((*line).to_string());
                }
                SlashOutcome::Handled
            }
            "model" => {
                if parsed.args.is_empty() {
                    self.push_notification(format!("[slash] current model: {}", self.model));
                } else {
                    self.model.clone_from(&parsed.args);
                    self.push_notification(format!("[slash] model set to {}", parsed.args));
                }
                SlashOutcome::Handled
            }
            "new" => {
                self.session_id = format!("tui-{}", chrono::Utc::now().timestamp_millis());
                self.clear_scrollback();
                self.usage = Usage::default();
                self.turn_count = 0;
                self.session_name = None;
                self.push_notification(format!("[slash] new session: {}", self.session_id));
                SlashOutcome::Handled
            }
            "name" => {
                if parsed.args.is_empty() {
                    self.push_notification("[slash] /name requires a name".to_string());
                } else {
                    self.session_name = Some(parsed.args.clone());
                    self.push_notification(format!("[slash] session name: {}", parsed.args));
                }
                SlashOutcome::Handled
            }
            "session" => {
                let name = self.session_name.as_deref().unwrap_or("(unnamed)");
                self.push_notification(format!(
                    "[slash] session id={} name={} messages={} turns={} input={} output={}",
                    self.session_id,
                    name,
                    self.messages.len(),
                    self.turn_count,
                    self.usage.input,
                    self.usage.output
                ));
                SlashOutcome::Handled
            }
            "copy" => match copy_last_assistant(&self.messages) {
                Ok(s) => {
                    self.push_notification(format!("[slash] copied {s} bytes to clipboard"));
                    SlashOutcome::Handled
                }
                Err(e) => {
                    self.push_notification(format!("[slash] copy failed: {e}"));
                    SlashOutcome::Handled
                }
            },
            "clear" => {
                self.clear_scrollback();
                SlashOutcome::Handled
            }
            "quit" => {
                self.should_quit = true;
                SlashOutcome::Quit
            }
            "cwd" => {
                if parsed.args.is_empty() {
                    self.push_notification(format!("[slash] cwd: {}", self.cwd));
                    SlashOutcome::Handled
                } else {
                    SlashOutcome::Chdir(PathBuf::from(parsed.args))
                }
            }
            "abort" => {
                if matches!(self.status, AppStatus::Running) {
                    self.runtime.abort(&self.session_id);
                    self.status = AppStatus::Aborted;
                    self.push_notification("[slash] abort signalled".to_string());
                } else {
                    self.push_notification("[slash] no run to abort".to_string());
                }
                SlashOutcome::Handled
            }
            _ => {
                self.push_notification(format!(
                    "[slash] /{} not yet implemented in this build",
                    parsed.name
                ));
                SlashOutcome::Handled
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

const HOTKEY_LINES: &[&str] = &[
    "[hotkeys] Enter: submit   Shift+Enter: newline   Alt+Enter: follow-up",
    "[hotkeys] Esc: clear / abort   Ctrl+C: abort or quit",
    "[hotkeys] Ctrl+L: clear scrollback   Ctrl+O: toggle tool collapse",
    "[hotkeys] Ctrl+T: toggle thinking blocks",
    "[hotkeys] PgUp/PgDn: scroll   Up/Down: history (single-line) or row nav",
    "[hotkeys] / opens command picker, Tab completes; @ opens file picker",
    "[hotkeys] !cmd runs bash and submits result, !!cmd runs and prints only",
];

fn copy_last_assistant(messages: &[RenderedMessage]) -> Result<usize, String> {
    let last = messages
        .iter()
        .rev()
        .find(|m| m.role == MessageRole::Assistant)
        .ok_or_else(|| "no assistant message in scrollback".to_string())?;
    let text = last.text.clone();
    let len = text.len();
    let mut clip = arboard::Clipboard::new().map_err(|e| e.to_string())?;
    clip.set_text(text).map_err(|e| e.to_string())?;
    Ok(len)
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

    #[test]
    fn slash_clear_routes_to_clear_scrollback() {
        let (mut app, _) = make_app();
        app.push_notification("noise".to_string());
        assert_eq!(app.messages.len(), 1);
        let out = app.route_slash("/clear");
        assert_eq!(out, SlashOutcome::Handled);
        assert!(app.messages.is_empty());
    }

    #[test]
    fn slash_quit_returns_quit_and_sets_flag() {
        let (mut app, _) = make_app();
        let out = app.route_slash("/quit");
        assert_eq!(out, SlashOutcome::Quit);
        assert!(app.should_quit);
    }

    #[test]
    fn slash_model_updates_model() {
        let (mut app, _) = make_app();
        let out = app.route_slash("/model claude-opus-4");
        assert_eq!(out, SlashOutcome::Handled);
        assert_eq!(app.model, "claude-opus-4");
    }

    #[test]
    fn slash_unimplemented_prints_notification() {
        let (mut app, _) = make_app();
        let out = app.route_slash("/tree");
        assert_eq!(out, SlashOutcome::Handled);
        assert!(app
            .messages
            .last()
            .is_some_and(|m| m.text.contains("not yet implemented")));
    }

    #[test]
    fn slash_unknown_returns_not_found() {
        let (mut app, _) = make_app();
        let out = app.route_slash("/nope");
        assert_eq!(out, SlashOutcome::NotFound);
    }

    #[test]
    fn complete_slash_expands_unique_prefix() {
        let (mut app, _) = make_app();
        app.editor.set("/he");
        app.refresh_command_picker();
        assert!(app.complete_slash());
        assert_eq!(app.editor.text(), "/help");
    }

    #[test]
    fn refresh_file_picker_detects_at_query() {
        let (mut app, _) = make_app();
        app.editor.set("look at @main");
        app.refresh_file_picker();
        assert!(app.file_picker_visible);
        assert_eq!(app.file_picker_query, "main");
    }

    #[test]
    fn toggle_tools_collapsed_flips_state() {
        let (mut app, _) = make_app();
        assert!(app.tools_collapsed);
        app.toggle_tools_collapsed();
        assert!(!app.tools_collapsed);
        app.toggle_tools_collapsed();
        assert!(app.tools_collapsed);
    }

    #[test]
    fn toggle_expand_thinking_flips_state() {
        let (mut app, _) = make_app();
        assert!(!app.expand_thinking);
        app.toggle_expand_thinking();
        assert!(app.expand_thinking);
        app.toggle_expand_thinking();
        assert!(!app.expand_thinking);
    }

    #[test]
    fn agent_start_records_run_started_at() {
        let (mut app, _) = make_app();
        assert!(app.run_started_at.is_none());
        app.apply_event(AgentEvent::AgentStart);
        assert!(app.run_started_at.is_some());
        app.apply_event(AgentEvent::AgentEnd {
            messages: Vec::new(),
        });
        assert!(app.run_started_at.is_none());
    }

    #[test]
    fn tick_advances_spinner_frame() {
        let (mut app, _) = make_app();
        let before = app.spinner_frame;
        app.tick();
        assert_eq!(app.spinner_frame, before.wrapping_add(1));
        app.tick();
        assert_eq!(app.spinner_frame, before.wrapping_add(2));
    }

    #[test]
    fn submit_steering_while_running_increments_queue() {
        let (mut app, _rt) = make_app();
        app.apply_event(AgentEvent::AgentStart);
        app.editor.set("steer me");
        let _ = app.submit_message();
        assert_eq!(app.queued_steering_count, 1);
        app.editor.set("steer again");
        let _ = app.submit_message();
        assert_eq!(app.queued_steering_count, 2);
    }

    #[test]
    fn submit_followup_while_running_increments_followup_queue() {
        let (mut app, _rt) = make_app();
        app.apply_event(AgentEvent::AgentStart);
        app.editor.set("then this");
        let _ = app.submit_followup();
        assert_eq!(app.queued_followup_count, 1);
    }

    #[test]
    fn tool_execution_start_sets_current_tool_and_clears_on_end() {
        let (mut app, _) = make_app();
        app.apply_event(AgentEvent::ToolExecutionStart {
            tool_call_id: "c1".into(),
            tool_name: "read".into(),
            args: serde_json::json!({}),
        });
        assert_eq!(app.current_tool.as_deref(), Some("read"));
        app.apply_event(AgentEvent::ToolExecutionEnd {
            tool_call_id: "c1".into(),
            tool_name: "read".into(),
            result: ToolResult {
                content: vec![ContentBlock::Text(TextContent { text: "ok".into() })],
                details: serde_json::json!({}),
                terminate: false,
            },
            is_error: false,
        });
        assert!(app.current_tool.is_none());
    }

    #[test]
    fn tool_execution_end_records_full_result() {
        let (mut app, _) = make_app();
        app.apply_event(AgentEvent::ToolExecutionStart {
            tool_call_id: "c2".into(),
            tool_name: "read".into(),
            args: serde_json::json!({}),
        });
        let big: String = "x".repeat(500);
        app.apply_event(AgentEvent::ToolExecutionEnd {
            tool_call_id: "c2".into(),
            tool_name: "read".into(),
            result: ToolResult {
                content: vec![ContentBlock::Text(TextContent { text: big.clone() })],
                details: serde_json::json!({}),
                terminate: false,
            },
            is_error: false,
        });
        assert_eq!(app.tool_calls[0].result_full.as_deref(), Some(big.as_str()));
        assert!(app.tool_calls[0].collapsed);
    }

    #[test]
    fn thinking_block_pushes_thinking_message() {
        let (mut app, _) = make_app();
        let assistant = AssistantMessage {
            content: vec![ContentBlock::Thinking {
                text: "thinking text".into(),
                signature: None,
            }],
            stop_reason: StopReason::End,
            error_message: None,
            error_kind: None,
            usage: None,
            model: "m".into(),
            provider: "p".into(),
            timestamp: 7,
        };
        app.apply_event(AgentEvent::MessageStart {
            message: AgentMessage::Assistant(assistant),
        });
        let thinking = app
            .messages
            .iter()
            .find(|m| m.role == MessageRole::Thinking)
            .expect("thinking message present");
        assert_eq!(thinking.text, "thinking text");
        assert!(thinking.thinking_token_count.is_some());
    }
}
