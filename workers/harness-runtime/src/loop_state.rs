//! The agent loop state machine.
//!
//! `run_loop` drives a session start-to-end, emitting `AgentEvent` variants
//! through the runtime's event sink. All side effects (LLM streaming, tool
//! dispatch, hook fan-out, queue draining) are routed through [`LoopRuntime`].

use harness_types::{
    AgentEvent, AgentMessage, AgentTool, AssistantMessage, ContentBlock, ExecutionMode, StopReason,
    TextContent, ToolCall, ToolResult, ToolResultMessage,
};

use crate::runtime::{BatchOutcome, EventSink, LoopRuntime};

/// Loop configuration. Currently minimal; will grow in P1+ as policy is wired.
#[derive(Debug, Clone)]
pub struct LoopConfig {
    pub session_id: String,
    pub tools: Vec<AgentTool>,
    pub default_execution_mode: ExecutionMode,
    /// Hard cap on assistant turns (one stream_assistant call = one turn).
    /// `None` means unbounded; the caller is expected to cap with the
    /// trigger-level timeout. Most users want a finite cap so a provider
    /// that loops on tool-calls can't burn the budget. Default: 32.
    pub max_turns: Option<u32>,
}

impl LoopConfig {
    pub fn new(session_id: impl Into<String>) -> Self {
        Self {
            session_id: session_id.into(),
            tools: Vec::new(),
            default_execution_mode: ExecutionMode::Parallel,
            max_turns: Some(32),
        }
    }
}

/// Final outcome of running the loop.
#[derive(Debug, Clone)]
pub struct LoopOutcome {
    pub messages: Vec<AgentMessage>,
}

/// Drive the loop start-to-end. Emits AgentEvents via the runtime's event sink.
///
/// Pseudocode mirror of `ARCHITECTURE.md` "The loop":
///
/// ```text
/// outer:
///   loop:
///     while has_more_tool_calls or pending.not_empty():
///       emit TurnStart
///       inject pending into context
///       transform_context -> messages
///       stream_assistant -> AssistantMessage
///       if stop_reason in (Error, Aborted): emit AgentEnd; return
///       if tool_calls: execute_batch; has_more = !batch.terminate
///       emit TurnEnd
///       pending = drain_steering()
///     followups = drain_followup()
///     if followups: pending = followups; continue outer
///     break
///   emit AgentEnd
/// ```
pub async fn run_loop<R: LoopRuntime + ?Sized, S: EventSink + ?Sized>(
    runtime: &R,
    sink: &S,
    config: &LoopConfig,
    initial_messages: Vec<AgentMessage>,
) -> LoopOutcome {
    let mut messages: Vec<AgentMessage> = initial_messages;

    sink.emit(AgentEvent::AgentStart).await;

    for m in &messages {
        sink.emit(AgentEvent::MessageStart { message: m.clone() })
            .await;
        sink.emit(AgentEvent::MessageEnd { message: m.clone() })
            .await;
    }

    let mut pending: Vec<AgentMessage> = runtime.drain_steering(&config.session_id).await;
    let mut turns_taken: u32 = 0;

    'outer: loop {
        let mut has_more_tool_calls = true;

        while has_more_tool_calls || !pending.is_empty() {
            if let Some(cap) = config.max_turns {
                if turns_taken >= cap {
                    // Budget exhausted. Emit a synthetic assistant turn so
                    // callers see a clean termination instead of the loop
                    // appearing to hang on the trigger timeout.
                    let exhausted = AssistantMessage {
                        content: vec![ContentBlock::Text(TextContent {
                            text: format!(
                                "loop stopped: max_turns ({cap}) reached"
                            ),
                        })],
                        stop_reason: StopReason::End,
                        error_message: None,
                        error_kind: None,
                        usage: None,
                        model: String::new(),
                        provider: String::new(),
                        timestamp: chrono::Utc::now().timestamp_millis(),
                    };
                    let exhausted_msg = AgentMessage::Assistant(exhausted.clone());
                    sink.emit(AgentEvent::MessageStart {
                        message: exhausted_msg.clone(),
                    })
                    .await;
                    sink.emit(AgentEvent::MessageEnd {
                        message: exhausted_msg.clone(),
                    })
                    .await;
                    messages.push(exhausted_msg.clone());
                    sink.emit(AgentEvent::TurnEnd {
                        message: exhausted_msg,
                        tool_results: Vec::new(),
                    })
                    .await;
                    break 'outer;
                }
            }
            turns_taken += 1;
            sink.emit(AgentEvent::TurnStart).await;

            for m in std::mem::take(&mut pending) {
                sink.emit(AgentEvent::MessageStart { message: m.clone() })
                    .await;
                sink.emit(AgentEvent::MessageEnd { message: m.clone() })
                    .await;
                messages.push(m);
            }

            messages = runtime.transform_context(messages).await;

            if runtime.abort_signal(&config.session_id).await {
                let aborted = aborted_message();
                messages.push(AgentMessage::Assistant(aborted.clone()));
                sink.emit(AgentEvent::TurnEnd {
                    message: AgentMessage::Assistant(aborted),
                    tool_results: Vec::new(),
                })
                .await;
                break 'outer;
            }

            let assistant: AssistantMessage = runtime
                .stream_assistant(&config.session_id, &messages, &config.tools)
                .await;
            let assistant_msg = AgentMessage::Assistant(assistant.clone());
            sink.emit(AgentEvent::MessageStart {
                message: assistant_msg.clone(),
            })
            .await;
            sink.emit(AgentEvent::MessageEnd {
                message: assistant_msg.clone(),
            })
            .await;
            messages.push(assistant_msg.clone());

            if matches!(
                assistant.stop_reason,
                StopReason::Error | StopReason::Aborted
            ) {
                sink.emit(AgentEvent::TurnEnd {
                    message: assistant_msg,
                    tool_results: Vec::new(),
                })
                .await;
                break 'outer;
            }

            let tool_calls = extract_tool_calls(&assistant);
            has_more_tool_calls = false;
            let mut tool_results: Vec<ToolResultMessage> = Vec::new();

            if !tool_calls.is_empty() {
                let batch = execute_tool_batch(runtime, sink, config, &tool_calls).await;
                for r in &batch.messages {
                    let m = AgentMessage::ToolResult(r.clone());
                    sink.emit(AgentEvent::MessageStart { message: m.clone() })
                        .await;
                    sink.emit(AgentEvent::MessageEnd { message: m.clone() })
                        .await;
                    messages.push(m);
                }
                tool_results = batch.messages;
                has_more_tool_calls = !batch.terminate;
            }

            sink.emit(AgentEvent::TurnEnd {
                message: assistant_msg,
                tool_results,
            })
            .await;

            pending = runtime.drain_steering(&config.session_id).await;
        }

        let followups = runtime.drain_followup(&config.session_id).await;
        if !followups.is_empty() {
            pending = followups;
            continue 'outer;
        }
        break;
    }

    sink.emit(AgentEvent::AgentEnd {
        messages: messages.clone(),
    })
    .await;

    LoopOutcome { messages }
}

/// Run one assistant message's tool calls. Honors the
/// `terminate-batch` and `sequential-override` rules from the spec.
async fn execute_tool_batch<R: LoopRuntime + ?Sized, S: EventSink + ?Sized>(
    runtime: &R,
    sink: &S,
    config: &LoopConfig,
    tool_calls: &[ToolCall],
) -> BatchOutcome {
    let has_sequential = tool_calls.iter().any(|tc| {
        config
            .tools
            .iter()
            .find(|t| t.name == tc.name)
            .is_some_and(|t| t.execution_mode == ExecutionMode::Sequential)
    });
    let _mode = if has_sequential {
        ExecutionMode::Sequential
    } else {
        config.default_execution_mode
    };

    let mut messages: Vec<ToolResultMessage> = Vec::with_capacity(tool_calls.len());
    let mut terminate_flags: Vec<bool> = Vec::with_capacity(tool_calls.len());

    for tc in tool_calls {
        sink.emit(AgentEvent::ToolExecutionStart {
            tool_call_id: tc.id.clone(),
            tool_name: tc.name.clone(),
            args: tc.arguments.clone(),
        })
        .await;

        let hook = runtime.before_tool_call(tc).await;
        let (result, is_error) = if hook.block {
            let reason = hook.reason.unwrap_or_else(|| "blocked".into());
            (error_result(&reason), true)
        } else {
            match runtime.resolve_tool(&tc.name).await {
                Some(handler) => {
                    let exec = handler.execute(tc).await;
                    let merged = runtime.after_tool_call(tc, exec).await;
                    (merged, false)
                }
                None => (error_result(&format!("tool not found: {}", tc.name)), true),
            }
        };

        sink.emit(AgentEvent::ToolExecutionEnd {
            tool_call_id: tc.id.clone(),
            tool_name: tc.name.clone(),
            result: result.clone(),
            is_error,
        })
        .await;

        terminate_flags.push(result.terminate);
        messages.push(ToolResultMessage {
            tool_call_id: tc.id.clone(),
            tool_name: tc.name.clone(),
            content: result.content,
            details: result.details,
            is_error,
            timestamp: chrono::Utc::now().timestamp_millis(),
        });
    }

    let terminate = !terminate_flags.is_empty() && terminate_flags.iter().all(|t| *t);
    BatchOutcome {
        messages,
        terminate,
    }
}

fn extract_tool_calls(assistant: &AssistantMessage) -> Vec<ToolCall> {
    assistant
        .content
        .iter()
        .filter_map(|c| match c {
            ContentBlock::ToolCall {
                id,
                name,
                arguments,
            } => Some(ToolCall {
                id: id.clone(),
                name: name.clone(),
                arguments: arguments.clone(),
            }),
            _ => None,
        })
        .collect()
}

fn error_result(message: &str) -> ToolResult {
    ToolResult {
        content: vec![ContentBlock::Text(harness_types::TextContent {
            text: message.to_string(),
        })],
        details: serde_json::json!({}),
        terminate: false,
    }
}

fn aborted_message() -> AssistantMessage {
    AssistantMessage {
        content: Vec::new(),
        stop_reason: StopReason::Aborted,
        error_message: Some("aborted".into()),
        error_kind: Some(harness_types::ErrorKind::Transient),
        usage: None,
        model: "harness".into(),
        provider: "harness".into(),
        timestamp: chrono::Utc::now().timestamp_millis(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::{CapturedEvents, EchoTool, MemoryRuntime};
    use harness_types::{AgentMessage, ContentBlock, TextContent, UserMessage};
    use std::sync::Arc;

    fn user(text: &str) -> AgentMessage {
        AgentMessage::User(UserMessage {
            content: vec![ContentBlock::Text(TextContent { text: text.into() })],
            timestamp: 1,
        })
    }

    fn assistant_text(text: &str) -> AssistantMessage {
        AssistantMessage {
            content: vec![ContentBlock::Text(TextContent { text: text.into() })],
            stop_reason: StopReason::End,
            error_message: None,
            error_kind: None,
            usage: None,
            model: "test".into(),
            provider: "test".into(),
            timestamp: 2,
        }
    }

    fn assistant_with_tool_call(tool: &str, args: serde_json::Value) -> AssistantMessage {
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
            model: "test".into(),
            provider: "test".into(),
            timestamp: 3,
        }
    }

    #[tokio::test]
    async fn simple_text_run_emits_full_lifecycle() {
        let captured = Arc::new(CapturedEvents::new());
        let rt = MemoryRuntime::new(captured.clone());
        rt.queue_assistant(assistant_text("hello"));
        let cfg = LoopConfig::new("s1");

        let outcome = run_loop(&rt, &*captured, &cfg, vec![user("hi")]).await;

        assert_eq!(outcome.messages.len(), 2); // user + assistant
        let events = captured.snapshot();
        assert!(events.iter().any(|e| matches!(e, AgentEvent::AgentStart)));
        assert!(events.iter().any(|e| matches!(e, AgentEvent::TurnStart)));
        assert!(events
            .iter()
            .any(|e| matches!(e, AgentEvent::TurnEnd { .. })));
        assert!(events
            .iter()
            .any(|e| matches!(e, AgentEvent::AgentEnd { .. })));
    }

    #[tokio::test]
    async fn tool_call_runs_handler_then_terminates_via_no_more_tools() {
        let captured = Arc::new(CapturedEvents::new());
        let mut rt = MemoryRuntime::new(captured.clone());
        rt.register_tool("echo", Arc::new(EchoTool));
        rt.queue_assistant(assistant_with_tool_call(
            "echo",
            serde_json::json!({ "text": "world" }),
        ));
        rt.queue_assistant(assistant_text("done"));

        let cfg = LoopConfig {
            session_id: "s2".into(),
            tools: vec![AgentTool {
                name: "echo".into(),
                description: "echo".into(),
                parameters: serde_json::json!({}),
                label: "echo".into(),
                execution_mode: ExecutionMode::Parallel,
                prepare_arguments_supported: false,
            }],
            default_execution_mode: ExecutionMode::Parallel,
            max_turns: None,
        };

        let outcome = run_loop(&rt, &*captured, &cfg, vec![user("call echo")]).await;

        // user + assistant1 + tool_result + assistant2 = 4 messages
        assert_eq!(outcome.messages.len(), 4);
        let events = captured.snapshot();
        let starts = events
            .iter()
            .filter(|e| matches!(e, AgentEvent::ToolExecutionStart { .. }))
            .count();
        let ends = events
            .iter()
            .filter(|e| matches!(e, AgentEvent::ToolExecutionEnd { .. }))
            .count();
        assert_eq!(starts, 1);
        assert_eq!(ends, 1);
    }

    #[tokio::test]
    async fn missing_tool_yields_error_result_not_panic() {
        let captured = Arc::new(CapturedEvents::new());
        let rt = MemoryRuntime::new(captured.clone());
        rt.queue_assistant(assistant_with_tool_call("missing", serde_json::json!({})));
        rt.queue_assistant(assistant_text("done"));

        let cfg = LoopConfig::new("s3");
        let outcome = run_loop(&rt, &*captured, &cfg, vec![user("call missing")]).await;
        assert!(outcome.messages.iter().any(|m| matches!(
            m,
            AgentMessage::ToolResult(t) if t.is_error
        )));
    }

    #[tokio::test]
    async fn abort_signal_breaks_outer_loop_immediately() {
        let captured = Arc::new(CapturedEvents::new());
        let rt = MemoryRuntime::new(captured.clone());
        rt.set_abort("s4", true);
        let cfg = LoopConfig::new("s4");
        let _ = run_loop(&rt, &*captured, &cfg, vec![user("hi")]).await;
        let events = captured.snapshot();
        assert!(events
            .iter()
            .any(|e| matches!(e, AgentEvent::AgentEnd { .. })));
    }

    #[tokio::test]
    async fn followup_drives_a_second_outer_iteration() {
        let captured = Arc::new(CapturedEvents::new());
        let rt = MemoryRuntime::new(captured.clone());
        rt.queue_assistant(assistant_text("first"));
        rt.queue_assistant(assistant_text("second"));
        rt.enqueue_followup("s5", vec![user("follow up")]);
        let cfg = LoopConfig::new("s5");

        let outcome = run_loop(&rt, &*captured, &cfg, vec![user("first prompt")]).await;
        // user + assistant + follow-up user + assistant = 4
        assert_eq!(outcome.messages.len(), 4);
    }
}
