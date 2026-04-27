//! Runtime traits used by the loop. Production binds these to iii-engine
//! primitives; tests use the [`MemoryRuntime`] variant.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use harness_types::{
    AgentEvent, AgentMessage, AgentTool, AssistantMessage, ContentBlock, StopReason, ToolCall,
    ToolResult, ToolResultMessage,
};

/// Result of a single tool call after `prepare → execute → finalize`.
#[derive(Debug, Clone)]
pub struct FinalizedTool {
    pub tool_call: ToolCall,
    pub result: ToolResult,
    pub is_error: bool,
}

/// Result of executing all tool calls produced by one assistant turn.
#[derive(Debug, Clone, Default)]
pub struct BatchOutcome {
    pub messages: Vec<ToolResultMessage>,
    /// `true` only when EVERY finalized tool result in the batch sets `terminate: true`.
    pub terminate: bool,
}

/// Outcome of `before_tool_call` pubsub fan-out.
#[derive(Debug, Clone, Default)]
pub struct HookOutcome {
    pub block: bool,
    pub reason: Option<String>,
}

/// Sink the loop emits AgentEvents into.
#[async_trait]
pub trait EventSink: Send + Sync {
    async fn emit(&self, event: AgentEvent);
}

/// Tool handler invoked by the loop's tool dispatcher.
#[async_trait]
pub trait ToolHandler: Send + Sync {
    async fn execute(&self, tool_call: &ToolCall) -> ToolResult;
}

/// Everything the loop needs to do its job. One trait, many implementations.
#[async_trait]
pub trait LoopRuntime: Send + Sync {
    /// Stream an assistant response. Implementations call into a provider worker
    /// and assemble the final `AssistantMessage` from the event sequence.
    async fn stream_assistant(
        &self,
        session_id: &str,
        messages: &[AgentMessage],
        tools: &[AgentTool],
    ) -> AssistantMessage;

    /// Resolve a tool by name. Built-in tools are handled inline; this hook
    /// exists for registry-discovered tools (any worker registering `tool::*`).
    async fn resolve_tool(&self, name: &str) -> Option<Arc<dyn ToolHandler>>;

    /// `before_tool_call` pubsub fan-out. First subscriber returning `block:true` wins.
    async fn before_tool_call(&self, tool_call: &ToolCall) -> HookOutcome;

    /// `after_tool_call` pubsub fan-out. Field-by-field merge in registration order.
    async fn after_tool_call(&self, tool_call: &ToolCall, result: ToolResult) -> ToolResult;

    /// `transform_context` pubsub pipeline. Each subscriber transforms messages.
    async fn transform_context(&self, messages: Vec<AgentMessage>) -> Vec<AgentMessage>;

    /// Drain the steering queue at end of each tool batch.
    async fn drain_steering(&self, session_id: &str) -> Vec<AgentMessage>;

    /// Drain the follow-up queue when the agent would otherwise stop.
    async fn drain_followup(&self, session_id: &str) -> Vec<AgentMessage>;

    /// Whether the abort signal is set for this session.
    async fn abort_signal(&self, session_id: &str) -> bool;
}

/// In-memory implementation of all runtime concerns. Used by tests, replay
/// tools, and examples that don't require an iii engine connection.
#[derive(Clone)]
pub struct MemoryRuntime {
    pub sink: Arc<dyn EventSink>,
    pub assistant_responses: Arc<Mutex<Vec<AssistantMessage>>>,
    pub tools: HashMap<String, Arc<dyn ToolHandler>>,
    pub steering: Arc<Mutex<HashMap<String, Vec<AgentMessage>>>>,
    pub followup: Arc<Mutex<HashMap<String, Vec<AgentMessage>>>>,
    pub aborts: Arc<Mutex<HashMap<String, bool>>>,
}

impl MemoryRuntime {
    pub fn new(sink: Arc<dyn EventSink>) -> Self {
        Self {
            sink,
            assistant_responses: Arc::new(Mutex::new(Vec::new())),
            tools: HashMap::new(),
            steering: Arc::new(Mutex::new(HashMap::new())),
            followup: Arc::new(Mutex::new(HashMap::new())),
            aborts: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub fn queue_assistant(&self, message: AssistantMessage) {
        if let Ok(mut g) = self.assistant_responses.lock() {
            g.push(message);
        }
    }

    pub fn register_tool(&mut self, name: impl Into<String>, handler: Arc<dyn ToolHandler>) {
        self.tools.insert(name.into(), handler);
    }

    pub fn enqueue_steering(&self, session_id: &str, messages: Vec<AgentMessage>) {
        if let Ok(mut g) = self.steering.lock() {
            g.entry(session_id.to_string())
                .or_default()
                .extend(messages);
        }
    }

    pub fn enqueue_followup(&self, session_id: &str, messages: Vec<AgentMessage>) {
        if let Ok(mut g) = self.followup.lock() {
            g.entry(session_id.to_string())
                .or_default()
                .extend(messages);
        }
    }

    pub fn set_abort(&self, session_id: &str, abort: bool) {
        if let Ok(mut g) = self.aborts.lock() {
            g.insert(session_id.to_string(), abort);
        }
    }
}

#[async_trait]
impl LoopRuntime for MemoryRuntime {
    async fn stream_assistant(
        &self,
        _session_id: &str,
        _messages: &[AgentMessage],
        _tools: &[AgentTool],
    ) -> AssistantMessage {
        if let Ok(mut g) = self.assistant_responses.lock() {
            if !g.is_empty() {
                return g.remove(0);
            }
        }
        AssistantMessage {
            content: vec![ContentBlock::Text(harness_types::TextContent {
                text: "no canned response".into(),
            })],
            stop_reason: StopReason::Error,
            error_message: Some("no canned response queued".into()),
            error_kind: Some(harness_types::ErrorKind::Permanent),
            usage: None,
            model: "memory-runtime".into(),
            provider: "memory-runtime".into(),
            timestamp: chrono::Utc::now().timestamp_millis(),
        }
    }

    async fn resolve_tool(&self, name: &str) -> Option<Arc<dyn ToolHandler>> {
        self.tools.get(name).cloned()
    }

    async fn before_tool_call(&self, _tool_call: &ToolCall) -> HookOutcome {
        HookOutcome::default()
    }

    async fn after_tool_call(&self, _tool_call: &ToolCall, result: ToolResult) -> ToolResult {
        result
    }

    async fn transform_context(&self, messages: Vec<AgentMessage>) -> Vec<AgentMessage> {
        messages
    }

    async fn drain_steering(&self, session_id: &str) -> Vec<AgentMessage> {
        self.steering
            .lock()
            .ok()
            .and_then(|mut g| g.get_mut(session_id).map(std::mem::take))
            .unwrap_or_default()
    }

    async fn drain_followup(&self, session_id: &str) -> Vec<AgentMessage> {
        self.followup
            .lock()
            .ok()
            .and_then(|mut g| g.get_mut(session_id).map(std::mem::take))
            .unwrap_or_default()
    }

    async fn abort_signal(&self, session_id: &str) -> bool {
        self.aborts
            .lock()
            .ok()
            .and_then(|g| g.get(session_id).copied())
            .unwrap_or(false)
    }
}

/// Vec-backed event sink. Tests inspect the captured events.
#[derive(Default)]
pub struct CapturedEvents {
    inner: Mutex<Vec<AgentEvent>>,
}

impl CapturedEvents {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn snapshot(&self) -> Vec<AgentEvent> {
        self.inner.lock().map(|g| g.clone()).unwrap_or_default()
    }
}

#[async_trait]
impl EventSink for CapturedEvents {
    async fn emit(&self, event: AgentEvent) {
        if let Ok(mut g) = self.inner.lock() {
            g.push(event);
        }
    }
}

/// Trivial echo tool used by replay fixtures. Returns the args' `text` field
/// (or the entire args JSON if no text key) as the tool result content.
pub struct EchoTool;

#[async_trait]
impl ToolHandler for EchoTool {
    async fn execute(&self, tool_call: &ToolCall) -> ToolResult {
        let text = tool_call
            .arguments
            .get("text")
            .and_then(|v| v.as_str())
            .map_or_else(|| tool_call.arguments.to_string(), ToString::to_string);
        ToolResult {
            content: vec![ContentBlock::Text(harness_types::TextContent { text })],
            details: serde_json::json!({}),
            terminate: false,
        }
    }
}
