//! Demonstrates the `before_tool_call` hook by wrapping `MemoryRuntime` with a
//! denylist subscriber. The subscriber runs before tool dispatch; if the tool
//! name is on the denylist, the call is blocked and the loop emits an error
//! tool result instead of invoking the handler.
//!
//! Production wires this through iii pubsub fan-out; here we model it with a
//! thin trait override so the example is self-contained.

use std::collections::HashSet;
use std::sync::Arc;

use async_trait::async_trait;
use harness_runtime::{
    run_loop, CapturedEvents, EventSink, HookOutcome, LoopConfig, LoopRuntime, MemoryRuntime,
    ToolHandler,
};
use harness_types::{
    AgentEvent, AgentMessage, AgentTool, AssistantMessage, ContentBlock, ExecutionMode, StopReason,
    TextContent, ToolCall, ToolResult, UserMessage,
};

/// Wraps another runtime and intercepts `before_tool_call` to enforce a
/// denylist. All other concerns delegate to the inner runtime.
struct DenylistRuntime<R: LoopRuntime> {
    inner: R,
    denied_tools: HashSet<String>,
}

#[async_trait]
impl<R: LoopRuntime> LoopRuntime for DenylistRuntime<R> {
    async fn stream_assistant(
        &self,
        session_id: &str,
        messages: &[AgentMessage],
        tools: &[AgentTool],
    ) -> AssistantMessage {
        self.inner
            .stream_assistant(session_id, messages, tools)
            .await
    }

    async fn resolve_tool(&self, name: &str) -> Option<Arc<dyn ToolHandler>> {
        self.inner.resolve_tool(name).await
    }

    async fn before_tool_call(&self, tool_call: &ToolCall) -> HookOutcome {
        if self.denied_tools.contains(&tool_call.name) {
            return HookOutcome {
                block: true,
                reason: Some(format!("denylist blocked: {}", tool_call.name)),
            };
        }
        self.inner.before_tool_call(tool_call).await
    }

    async fn after_tool_call(&self, tool_call: &ToolCall, result: ToolResult) -> ToolResult {
        self.inner.after_tool_call(tool_call, result).await
    }

    async fn transform_context(&self, messages: Vec<AgentMessage>) -> Vec<AgentMessage> {
        self.inner.transform_context(messages).await
    }

    async fn drain_steering(&self, session_id: &str) -> Vec<AgentMessage> {
        self.inner.drain_steering(session_id).await
    }

    async fn drain_followup(&self, session_id: &str) -> Vec<AgentMessage> {
        self.inner.drain_followup(session_id).await
    }

    async fn abort_signal(&self, session_id: &str) -> bool {
        self.inner.abort_signal(session_id).await
    }
}

struct DangerousTool;

#[async_trait]
impl ToolHandler for DangerousTool {
    async fn execute(&self, _tool_call: &ToolCall) -> ToolResult {
        ToolResult {
            content: vec![ContentBlock::Text(TextContent {
                text: "this should never run".into(),
            })],
            details: serde_json::json!({}),
            terminate: false,
        }
    }
}

fn user(text: &str) -> AgentMessage {
    AgentMessage::User(UserMessage {
        content: vec![ContentBlock::Text(TextContent { text: text.into() })],
        timestamp: chrono::Utc::now().timestamp_millis(),
    })
}

fn assistant_calls(tool: &str, args: serde_json::Value) -> AssistantMessage {
    AssistantMessage {
        content: vec![ContentBlock::ToolCall {
            id: "call-1".into(),
            name: tool.into(),
            arguments: args,
        }],
        stop_reason: StopReason::Tool,
        error_message: None,
        error_kind: None,
        usage: None,
        model: "faux".into(),
        provider: "faux".into(),
        timestamp: chrono::Utc::now().timestamp_millis(),
    }
}

fn assistant_text(text: &str) -> AssistantMessage {
    AssistantMessage {
        content: vec![ContentBlock::Text(TextContent { text: text.into() })],
        stop_reason: StopReason::End,
        error_message: None,
        error_kind: None,
        usage: None,
        model: "faux".into(),
        provider: "faux".into(),
        timestamp: chrono::Utc::now().timestamp_millis(),
    }
}

#[tokio::main]
async fn main() {
    let captured = Arc::new(CapturedEvents::new());
    let mut inner = MemoryRuntime::new(captured.clone());
    inner.register_tool("dangerous", Arc::new(DangerousTool));

    inner.queue_assistant(assistant_calls(
        "dangerous",
        serde_json::json!({ "command": "rm -rf /" }),
    ));
    inner.queue_assistant(assistant_text("loop ended after blocked tool"));

    let denylist: HashSet<String> = std::iter::once("dangerous".to_string()).collect();
    let rt = DenylistRuntime {
        inner,
        denied_tools: denylist,
    };

    let cfg = LoopConfig {
        session_id: "hook-demo".into(),
        tools: vec![AgentTool {
            name: "dangerous".into(),
            description: "noop".into(),
            parameters: serde_json::json!({}),
            label: "dangerous".into(),
            execution_mode: ExecutionMode::Parallel,
            prepare_arguments_supported: false,
        }],
        default_execution_mode: ExecutionMode::Parallel,
    };

    let outcome = run_loop(&rt, &*captured, &cfg, vec![user("trigger blocked tool")]).await;

    let blocked = captured.snapshot().into_iter().any(|e| {
        matches!(
            e,
            AgentEvent::ToolExecutionEnd { is_error, result, .. }
                if is_error && result.content.iter().any(|c| matches!(c, ContentBlock::Text(t) if t.text.contains("denylist blocked")))
        )
    });

    println!("messages: {}", outcome.messages.len());
    println!("denylist blocked tool call: {blocked}");
    println!("events emitted: {}", captured.snapshot().len());

    let ev_sink: &dyn EventSink = &*captured;
    let _ = ev_sink; // suppress unused-import warning by referencing trait
}
