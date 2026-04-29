//! [`harness_runtime::LoopRuntime`] implemented over an [`IiiClientLike`].
//!
//! Every method routes to engine-builtin functions on the bus. Hot paths
//! (state reads, pubsub fan-out, tool dispatch) are factored as small
//! free functions so the test suite can exercise them with a fake client.

use std::sync::Arc;

use async_trait::async_trait;
use harness_runtime::{HookOutcome, LoopRuntime, ToolHandler};
use harness_types::{
    AgentMessage, AgentTool, AssistantMessage, ContentBlock, ErrorKind, StopReason, TextContent,
    ToolCall, ToolResult,
};
use serde_json::{json, Value};

use crate::client::IiiClientLike;
use crate::hooks;

/// Default per-hook collection window. Subscribers that don't reply within
/// this many milliseconds are dropped from the merge.
const DEFAULT_HOOK_TIMEOUT_MS: u64 = 5_000;

/// Pubsub topic and state-key constants. Centralised so tests and downstream
/// crates can refer to them without restating the layout.
pub mod state_keys {
    /// State scope shared by all session keys.
    pub const SCOPE: &str = "agent";

    /// `state::set` key for a session's full message log.
    pub fn messages(session_id: &str) -> String {
        format!("session/{session_id}/messages")
    }

    /// `state::set` key for engine-side session state (tool execution mode,
    /// per-tool counters, etc.).
    pub fn state(session_id: &str) -> String {
        format!("session/{session_id}/state")
    }

    /// Steering queue (mid-run injection by the user).
    pub fn steering(session_id: &str) -> String {
        format!("session/{session_id}/steering")
    }

    /// Follow-up queue (post-stop continuations).
    pub fn followup(session_id: &str) -> String {
        format!("session/{session_id}/followup")
    }

    /// Boolean abort signal.
    pub fn abort_signal(session_id: &str) -> String {
        format!("session/{session_id}/abort_signal")
    }

    /// Hook topic names.
    pub const TOPIC_BEFORE: &str = "agent::before_tool_call";
    pub const TOPIC_AFTER: &str = "agent::after_tool_call";
    pub const TOPIC_TRANSFORM: &str = "agent::transform_context";
}

// No prefix. Function id == LLM-visible tool name. iii has three
// primitives (Worker, Function, Trigger); a tool is an iii Function
// whose id matches the name the LLM emits in `ContentBlock::ToolCall`.

/// `LoopRuntime` implementation backed by the iii bus.
pub struct IiiBridgeRuntime<C: IiiClientLike + 'static> {
    client: Arc<C>,
    stream_assistant: Arc<StreamAssistantHandler>,
    hook_timeout_ms: u64,
}

/// Provider plug-in for `stream_assistant`. Passed in at construction so
/// the bridge can stay agnostic to which provider crate the consumer uses.
pub type StreamAssistantHandler = dyn Fn(
        StreamAssistantInput,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = AssistantMessage> + Send>>
    + Send
    + Sync;

/// Inputs to the `stream_assistant` hook.
pub struct StreamAssistantInput {
    pub session_id: String,
    pub messages: Vec<AgentMessage>,
    pub tools: Vec<AgentTool>,
}

impl<C: IiiClientLike + 'static> IiiBridgeRuntime<C> {
    /// Build a runtime that delegates the LLM call to `stream_assistant`.
    pub fn new(client: Arc<C>, stream_assistant: Arc<StreamAssistantHandler>) -> Self {
        Self {
            client,
            stream_assistant,
            hook_timeout_ms: DEFAULT_HOOK_TIMEOUT_MS,
        }
    }

    /// Override the per-hook collection window. Defaults to 5 seconds.
    pub fn with_hook_timeout_ms(mut self, ms: u64) -> Self {
        self.hook_timeout_ms = ms;
        self
    }

    /// Underlying client (escape hatch for tests and advanced consumers).
    pub fn client(&self) -> &C {
        &self.client
    }

    /// Atomically pop all entries from a queue key. Reads the current value,
    /// writes back an empty list, and returns the popped batch.
    ///
    /// iii-sdk 0.11 has no native CAS primitive, so this is read-modify-write
    /// rather than truly atomic. Callers that need strict ordering against
    /// concurrent producers should serialise pushes through a single worker.
    async fn drain_queue(&self, session_id: &str, key_fn: fn(&str) -> String) -> Vec<AgentMessage> {
        let key = key_fn(session_id);
        let current = match self.client.state_get(state_keys::SCOPE, &key).await {
            Ok(Some(v)) => v,
            _ => return Vec::new(),
        };
        let messages: Vec<AgentMessage> = serde_json::from_value(current).unwrap_or_default();
        if messages.is_empty() {
            return messages;
        }
        let _ = self
            .client
            .state_set(state_keys::SCOPE, &key, json!([]))
            .await;
        messages
    }
}

#[async_trait]
impl<C: IiiClientLike + 'static> LoopRuntime for IiiBridgeRuntime<C> {
    async fn stream_assistant(
        &self,
        session_id: &str,
        messages: &[AgentMessage],
        tools: &[AgentTool],
    ) -> AssistantMessage {
        (self.stream_assistant)(StreamAssistantInput {
            session_id: session_id.to_string(),
            messages: messages.to_vec(),
            tools: tools.to_vec(),
        })
        .await
    }

    async fn resolve_tool(&self, name: &str) -> Option<Arc<dyn ToolHandler>> {
        let function_id = name.to_string();
        match self.client.list_function_ids().await {
            Ok(ids) if ids.iter().any(|id| id == &function_id) => {
                Some(Arc::new(BridgeToolHandler {
                    client: self.client.clone(),
                    function_id,
                }) as Arc<dyn ToolHandler>)
            }
            _ => None,
        }
    }

    async fn before_tool_call(&self, tool_call: &ToolCall) -> HookOutcome {
        let payload = json!({ "tool_call": tool_call });
        let responses = self
            .client
            .publish_collect(state_keys::TOPIC_BEFORE, payload, self.hook_timeout_ms)
            .await
            .unwrap_or_default();
        hooks::merge_before(&responses)
    }

    async fn after_tool_call(&self, tool_call: &ToolCall, result: ToolResult) -> ToolResult {
        let payload = json!({ "tool_call": tool_call, "result": result });
        let responses = self
            .client
            .publish_collect(state_keys::TOPIC_AFTER, payload, self.hook_timeout_ms)
            .await
            .unwrap_or_default();
        hooks::merge_after(result, &responses)
    }

    async fn transform_context(&self, messages: Vec<AgentMessage>) -> Vec<AgentMessage> {
        let payload = json!({ "messages": messages });
        let responses = self
            .client
            .publish_collect(state_keys::TOPIC_TRANSFORM, payload, self.hook_timeout_ms)
            .await
            .unwrap_or_default();
        responses
            .iter()
            .find_map(hooks::decode_transform)
            .unwrap_or(messages)
    }

    async fn drain_steering(&self, session_id: &str) -> Vec<AgentMessage> {
        self.drain_queue(session_id, state_keys::steering).await
    }

    async fn drain_followup(&self, session_id: &str) -> Vec<AgentMessage> {
        self.drain_queue(session_id, state_keys::followup).await
    }

    async fn abort_signal(&self, session_id: &str) -> bool {
        let key = state_keys::abort_signal(session_id);
        match self.client.state_get(state_keys::SCOPE, &key).await {
            Ok(Some(v)) => v.as_bool().unwrap_or(false),
            _ => false,
        }
    }
}

/// `ToolHandler` that dispatches each call to a `tool::<name>` function on
/// the bus. The handler converts iii errors into a non-fatal
/// [`harness_types::ToolResult`] error block so the loop can continue.
pub struct BridgeToolHandler<C: IiiClientLike + 'static> {
    client: Arc<C>,
    function_id: String,
}

impl<C: IiiClientLike + 'static> BridgeToolHandler<C> {
    pub fn new(client: Arc<C>, function_id: String) -> Self {
        Self {
            client,
            function_id,
        }
    }
}

#[async_trait]
impl<C: IiiClientLike + 'static> ToolHandler for BridgeToolHandler<C> {
    async fn execute(&self, tool_call: &ToolCall) -> ToolResult {
        let payload = json!({
            "id": tool_call.id,
            "name": tool_call.name,
            "arguments": tool_call.arguments,
        });
        match self.client.invoke(&self.function_id, payload).await {
            Ok(response) => decode_tool_response(response, &tool_call.name),
            Err(err) => error_result(&format!(
                "tool '{}' invocation failed: {err}",
                tool_call.name
            )),
        }
    }
}

/// Translate the JSON returned by a `tool::<name>` function into a
/// [`ToolResult`]. The function may return either a serialised
/// [`ToolResult`] directly, or a partial object with the same fields.
fn decode_tool_response(response: Value, tool_name: &str) -> ToolResult {
    if let Ok(parsed) = serde_json::from_value::<ToolResult>(response.clone()) {
        return parsed;
    }
    let content = response
        .get("content")
        .and_then(|c| serde_json::from_value::<Vec<ContentBlock>>(c.clone()).ok())
        .unwrap_or_else(|| {
            vec![ContentBlock::Text(TextContent {
                text: response.to_string(),
            })]
        });
    let details = response
        .get("details")
        .cloned()
        .unwrap_or_else(|| json!({}));
    let terminate = response
        .get("terminate")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let _ = tool_name;
    ToolResult {
        content,
        details,
        terminate,
    }
}

fn error_result(message: &str) -> ToolResult {
    ToolResult {
        content: vec![ContentBlock::Text(TextContent {
            text: message.to_string(),
        })],
        details: json!({}),
        terminate: false,
    }
}

/// Build a "transport failed" assistant message. Used by
/// [`register::register_agent_functions`] and any wrapper that needs to
/// short-circuit a stream call.
pub fn error_assistant(reason: &str) -> AssistantMessage {
    AssistantMessage {
        content: Vec::new(),
        stop_reason: StopReason::Error,
        error_message: Some(reason.to_string()),
        error_kind: Some(ErrorKind::Transient),
        usage: None,
        model: "iii-bridge".into(),
        provider: "iii-bridge".into(),
        timestamp: chrono::Utc::now().timestamp_millis(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::FakeClient;
    use harness_types::{ContentBlock, TextContent};
    use serde_json::json;

    fn dummy_stream() -> Arc<StreamAssistantHandler> {
        Arc::new(|_input: StreamAssistantInput| {
            Box::pin(async move {
                AssistantMessage {
                    content: Vec::new(),
                    stop_reason: StopReason::End,
                    error_message: None,
                    error_kind: None,
                    usage: None,
                    model: "stub".into(),
                    provider: "stub".into(),
                    timestamp: 0,
                }
            }) as _
        })
    }

    #[test]
    fn state_keys_match_architecture_spec() {
        assert_eq!(state_keys::messages("abc"), "session/abc/messages");
        assert_eq!(state_keys::state("abc"), "session/abc/state");
        assert_eq!(state_keys::steering("abc"), "session/abc/steering");
        assert_eq!(state_keys::followup("abc"), "session/abc/followup");
        assert_eq!(state_keys::abort_signal("abc"), "session/abc/abort_signal");
        assert_eq!(state_keys::SCOPE, "agent");
        assert_eq!(state_keys::TOPIC_BEFORE, "agent::before_tool_call");
        assert_eq!(state_keys::TOPIC_AFTER, "agent::after_tool_call");
        assert_eq!(state_keys::TOPIC_TRANSFORM, "agent::transform_context");
    }

    #[test]
    fn decode_tool_response_accepts_full_tool_result() {
        let raw = json!({
            "content": [{ "type": "text", "text": "hello" }],
            "details": { "exit": 0 },
            "terminate": false,
        });
        let result = decode_tool_response(raw, "echo");
        assert!(matches!(result.content[0], ContentBlock::Text(_)));
        assert_eq!(result.details["exit"], json!(0));
    }

    #[test]
    fn decode_tool_response_falls_back_to_textual_form() {
        let raw = json!({ "weird": "shape" });
        let result = decode_tool_response(raw, "echo");
        match &result.content[0] {
            ContentBlock::Text(t) => assert!(t.text.contains("weird")),
            _ => panic!("expected text fallback"),
        }
    }

    #[tokio::test]
    async fn abort_signal_reads_from_state() {
        let client = Arc::new(FakeClient::new());
        client
            .preset_state("agent", "session/s1/abort_signal", json!(true))
            .await;
        let rt = IiiBridgeRuntime::new(client, dummy_stream());
        assert!(rt.abort_signal("s1").await);
    }

    #[tokio::test]
    async fn drain_steering_clears_queue_after_read() {
        let client = Arc::new(FakeClient::new());
        let preload = json!([
            { "role": "user", "content": [{ "type": "text", "text": "hi" }], "timestamp": 1 }
        ]);
        client
            .preset_state("agent", "session/s2/steering", preload)
            .await;
        let rt = IiiBridgeRuntime::new(client.clone(), dummy_stream());
        let drained = rt.drain_steering("s2").await;
        assert_eq!(drained.len(), 1);
        // queue was cleared
        let after = client
            .invoke(
                "state::get",
                json!({ "scope": "agent", "key": "session/s2/steering" }),
            )
            .await
            .unwrap();
        assert_eq!(after, json!([]));
    }

    #[tokio::test]
    async fn before_tool_call_uses_collected_block_response() {
        let client = Arc::new(FakeClient::new());
        client
            .preset_topic_response(
                state_keys::TOPIC_BEFORE,
                vec![json!({ "block": true, "reason": "policy" })],
            )
            .await;
        let rt = IiiBridgeRuntime::new(client, dummy_stream());
        let outcome = rt
            .before_tool_call(&ToolCall {
                id: "t1".into(),
                name: "echo".into(),
                arguments: json!({}),
            })
            .await;
        assert!(outcome.block);
        assert_eq!(outcome.reason.as_deref(), Some("policy"));
    }

    #[tokio::test]
    async fn after_tool_call_merges_collected_responses() {
        let client = Arc::new(FakeClient::new());
        client
            .preset_topic_response(
                state_keys::TOPIC_AFTER,
                vec![json!({ "details": { "added": "yes" } })],
            )
            .await;
        let rt = IiiBridgeRuntime::new(client, dummy_stream());
        let initial = ToolResult {
            content: vec![ContentBlock::Text(TextContent { text: "ok".into() })],
            details: json!({ "exit": 0 }),
            terminate: false,
        };
        let merged = rt
            .after_tool_call(
                &ToolCall {
                    id: "t1".into(),
                    name: "echo".into(),
                    arguments: json!({}),
                },
                initial,
            )
            .await;
        assert_eq!(merged.details["added"], json!("yes"));
        assert_eq!(merged.details["exit"], json!(0));
    }

    #[tokio::test]
    async fn resolve_tool_returns_handler_when_function_registered() {
        let client = Arc::new(FakeClient::new());
        client.add_registered_function("echo").await;
        client
            .preset_invoke_response(
                "echo",
                json!({
                    "content": [{ "type": "text", "text": "from-bus" }],
                    "details": {},
                    "terminate": false,
                }),
            )
            .await;
        let rt = IiiBridgeRuntime::new(client, dummy_stream());
        let handler = rt.resolve_tool("echo").await.expect("tool resolves");
        let result = handler
            .execute(&ToolCall {
                id: "t1".into(),
                name: "echo".into(),
                arguments: json!({}),
            })
            .await;
        match &result.content[0] {
            ContentBlock::Text(t) => assert_eq!(t.text, "from-bus"),
            _ => panic!("expected bus response text"),
        }
    }

    #[tokio::test]
    async fn resolve_tool_returns_none_when_not_registered() {
        let client = Arc::new(FakeClient::new());
        let rt = IiiBridgeRuntime::new(client, dummy_stream());
        assert!(rt.resolve_tool("missing").await.is_none());
    }
}
