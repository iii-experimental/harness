//! Registration helpers that publish the 10 canonical agent functions plus
//! the 4 HTTP triggers documented in `ARCHITECTURE.md` onto a live engine.
//!
//! ```text
//! agent::run_loop
//! agent::stream_assistant
//! agent::prepare_tool        agent::execute_tool      agent::finalize_tool
//! agent::transform_context   agent::convert_to_llm
//! agent::get_steering        agent::get_followup
//! agent::abort
//!
//! HTTP:
//!   POST /agent/prompt           -> agent::run_loop
//!   POST /agent/<id>/steer       -> push to steering queue
//!   POST /agent/<id>/abort       -> set abort signal
//!   POST /agent/<id>/follow_up   -> push to follow-up queue
//! ```
//!
//! ## iii-sdk dependency
//!
//! Function and trigger registration requires a real [`iii_sdk::III`]
//! handle (the [`IiiClientLike`] abstraction is intentionally read-only:
//! it only exposes the methods the runtime calls during a loop tick).
//! [`register_agent_functions`] therefore takes an [`Arc<III>`] directly
//! rather than the trait object — this is the one place the bridge talks
//! to iii-sdk's mutable surface.

use std::sync::Arc;

use harness_runtime::{run_loop, LoopConfig, LoopRuntime};
use harness_types::{AgentMessage, AgentTool, AssistantMessage, ExecutionMode};
use iii_sdk::{FunctionRef, RegisterFunctionMessage, RegisterTriggerInput, Trigger, III};
use serde_json::{json, Value};

use crate::client::{BridgeError, IiiSdkClient};
use crate::runtime::{state_keys, IiiBridgeRuntime, StreamAssistantHandler, StreamAssistantInput};
use crate::sink::IiiEventSink;

/// Concrete provider plug-in alias for `register_agent_functions`. Wraps
/// the same handler signature the runtime uses.
pub type StreamAssistantFn = Arc<StreamAssistantHandler>;

/// All function and trigger refs returned by [`register_agent_functions`].
/// Drop this to unregister everything in one shot.
pub struct AgentFunctionRefs {
    pub functions: Vec<FunctionRef>,
    pub triggers: Vec<Trigger>,
}

impl AgentFunctionRefs {
    /// Unregister everything this batch installed.
    pub fn unregister_all(self) {
        for t in self.triggers {
            t.unregister();
        }
        for f in self.functions {
            f.unregister();
        }
    }
}

/// Register the canonical agent function set on `client`. Each handler
/// constructs an [`IiiBridgeRuntime`] + [`IiiEventSink`] and calls into the
/// matching `harness_runtime` operation.
pub fn register_agent_functions(
    client: Arc<III>,
    stream_assistant: StreamAssistantFn,
) -> Result<AgentFunctionRefs, BridgeError> {
    let mut functions = Vec::with_capacity(10);
    let mut triggers = Vec::with_capacity(4);

    functions.push(register_run_loop(&client, stream_assistant.clone()));
    functions.push(register_stream_assistant(&client, stream_assistant.clone()));
    functions.push(register_prepare_tool(&client, stream_assistant.clone()));
    functions.push(register_execute_tool(&client, stream_assistant.clone()));
    functions.push(register_finalize_tool(&client, stream_assistant.clone()));
    functions.push(register_transform_context(
        &client,
        stream_assistant.clone(),
    ));
    functions.push(register_convert_to_llm(&client));
    functions.push(register_get_steering(&client));
    functions.push(register_get_followup(&client));
    functions.push(register_abort(&client));

    triggers.push(register_http(&client, "agent/prompt", "agent::run_loop")?);
    triggers.push(register_http(
        &client,
        "agent/{session_id}/steer",
        "agent::push_steering",
    )?);
    triggers.push(register_http(
        &client,
        "agent/{session_id}/abort",
        "agent::abort",
    )?);
    triggers.push(register_http(
        &client,
        "agent/{session_id}/follow_up",
        "agent::push_followup",
    )?);

    // Push-queue helpers backing the steer / follow_up HTTP triggers. Kept
    // out of the public 10 since they are pure plumbing.
    functions.push(register_push_steering(&client));
    functions.push(register_push_followup(&client));

    Ok(AgentFunctionRefs {
        functions,
        triggers,
    })
}

fn register_run_loop(client: &Arc<III>, stream_assistant: StreamAssistantFn) -> FunctionRef {
    let client_for_handler = client.clone();
    client.register_function((
        RegisterFunctionMessage::with_id("agent::run_loop".to_string())
            .with_description("Drive the agent loop start to end".to_string()),
        move |payload: Value| {
            let client = client_for_handler.clone();
            let stream_assistant = stream_assistant.clone();
            async move {
                let session_id = required_str(&payload, "session_id")?;
                let messages: Vec<AgentMessage> = payload
                    .get("messages")
                    .cloned()
                    .map(serde_json::from_value)
                    .transpose()
                    .map_err(|e| iii_sdk::IIIError::Handler(e.to_string()))?
                    .unwrap_or_default();
                let tools: Vec<AgentTool> = payload
                    .get("tools")
                    .cloned()
                    .map(serde_json::from_value)
                    .transpose()
                    .map_err(|e| iii_sdk::IIIError::Handler(e.to_string()))?
                    .unwrap_or_default();

                let bridge_client = Arc::new(IiiSdkClient::new(client));
                let runtime = IiiBridgeRuntime::new(bridge_client.clone(), stream_assistant);
                let sink = IiiEventSink::new(bridge_client, session_id.clone());
                let cfg = LoopConfig {
                    session_id: session_id.clone(),
                    tools,
                    default_execution_mode: ExecutionMode::Parallel,
                };
                let outcome = run_loop(&runtime, &sink, &cfg, messages).await;
                serde_json::to_value(json!({ "messages": outcome.messages }))
                    .map_err(|e| iii_sdk::IIIError::Handler(e.to_string()))
            }
        },
    ))
}

fn register_stream_assistant(
    client: &Arc<III>,
    stream_assistant: StreamAssistantFn,
) -> FunctionRef {
    client.register_function((
        RegisterFunctionMessage::with_id("agent::stream_assistant".to_string())
            .with_description("One LLM streaming call".to_string()),
        move |payload: Value| {
            let stream_assistant = stream_assistant.clone();
            async move {
                let session_id = required_str(&payload, "session_id")?;
                let messages: Vec<AgentMessage> = payload
                    .get("messages")
                    .cloned()
                    .map(serde_json::from_value)
                    .transpose()
                    .map_err(|e| iii_sdk::IIIError::Handler(e.to_string()))?
                    .unwrap_or_default();
                let tools: Vec<AgentTool> = payload
                    .get("tools")
                    .cloned()
                    .map(serde_json::from_value)
                    .transpose()
                    .map_err(|e| iii_sdk::IIIError::Handler(e.to_string()))?
                    .unwrap_or_default();
                let assistant: AssistantMessage = stream_assistant(StreamAssistantInput {
                    session_id,
                    messages,
                    tools,
                })
                .await;
                serde_json::to_value(assistant)
                    .map_err(|e| iii_sdk::IIIError::Handler(e.to_string()))
            }
        },
    ))
}

fn register_prepare_tool(client: &Arc<III>, stream_assistant: StreamAssistantFn) -> FunctionRef {
    let client_for_handler = client.clone();
    client.register_function((
        RegisterFunctionMessage::with_id("agent::prepare_tool".to_string())
            .with_description("Validate args and run before_tool_call hook".to_string()),
        move |payload: Value| {
            let client = client_for_handler.clone();
            let stream_assistant = stream_assistant.clone();
            async move {
                let tool_call = required_value(&payload, "tool_call")?;
                let tool_call: harness_types::ToolCall = serde_json::from_value(tool_call)
                    .map_err(|e| iii_sdk::IIIError::Handler(e.to_string()))?;
                let bridge_client = Arc::new(IiiSdkClient::new(client));
                let runtime = IiiBridgeRuntime::new(bridge_client, stream_assistant);
                let outcome = runtime.before_tool_call(&tool_call).await;
                Ok(json!({
                    "block": outcome.block,
                    "reason": outcome.reason,
                }))
            }
        },
    ))
}

fn register_execute_tool(client: &Arc<III>, stream_assistant: StreamAssistantFn) -> FunctionRef {
    let client_for_handler = client.clone();
    client.register_function((
        RegisterFunctionMessage::with_id("agent::execute_tool".to_string())
            .with_description("Dispatch to tool::<name>".to_string()),
        move |payload: Value| {
            let client = client_for_handler.clone();
            let stream_assistant = stream_assistant.clone();
            async move {
                let tool_call: harness_types::ToolCall = required_value(&payload, "tool_call")
                    .and_then(|v| {
                        serde_json::from_value(v)
                            .map_err(|e| iii_sdk::IIIError::Handler(e.to_string()))
                    })?;
                let bridge_client = Arc::new(IiiSdkClient::new(client));
                let runtime = IiiBridgeRuntime::new(bridge_client, stream_assistant);
                let result = match runtime.resolve_tool(&tool_call.name).await {
                    Some(handler) => handler.execute(&tool_call).await,
                    None => harness_types::ToolResult {
                        content: vec![harness_types::ContentBlock::Text(
                            harness_types::TextContent {
                                text: format!("tool not found: {}", tool_call.name),
                            },
                        )],
                        details: json!({}),
                        terminate: false,
                    },
                };
                serde_json::to_value(result).map_err(|e| iii_sdk::IIIError::Handler(e.to_string()))
            }
        },
    ))
}

fn register_finalize_tool(client: &Arc<III>, stream_assistant: StreamAssistantFn) -> FunctionRef {
    let client_for_handler = client.clone();
    client.register_function((
        RegisterFunctionMessage::with_id("agent::finalize_tool".to_string())
            .with_description("Run after_tool_call hook and merge results".to_string()),
        move |payload: Value| {
            let client = client_for_handler.clone();
            let stream_assistant = stream_assistant.clone();
            async move {
                let tool_call: harness_types::ToolCall = required_value(&payload, "tool_call")
                    .and_then(|v| {
                        serde_json::from_value(v)
                            .map_err(|e| iii_sdk::IIIError::Handler(e.to_string()))
                    })?;
                let result: harness_types::ToolResult = required_value(&payload, "result")
                    .and_then(|v| {
                        serde_json::from_value(v)
                            .map_err(|e| iii_sdk::IIIError::Handler(e.to_string()))
                    })?;
                let bridge_client = Arc::new(IiiSdkClient::new(client));
                let runtime = IiiBridgeRuntime::new(bridge_client, stream_assistant);
                let merged = runtime.after_tool_call(&tool_call, result).await;
                serde_json::to_value(merged).map_err(|e| iii_sdk::IIIError::Handler(e.to_string()))
            }
        },
    ))
}

fn register_transform_context(
    client: &Arc<III>,
    stream_assistant: StreamAssistantFn,
) -> FunctionRef {
    let client_for_handler = client.clone();
    client.register_function((
        RegisterFunctionMessage::with_id("agent::transform_context".to_string())
            .with_description("Run transform_context pubsub pipeline".to_string()),
        move |payload: Value| {
            let client = client_for_handler.clone();
            let stream_assistant = stream_assistant.clone();
            async move {
                let messages: Vec<AgentMessage> = payload
                    .get("messages")
                    .cloned()
                    .map(serde_json::from_value)
                    .transpose()
                    .map_err(|e| iii_sdk::IIIError::Handler(e.to_string()))?
                    .unwrap_or_default();
                let bridge_client = Arc::new(IiiSdkClient::new(client));
                let runtime = IiiBridgeRuntime::new(bridge_client, stream_assistant);
                let transformed = runtime.transform_context(messages).await;
                Ok(json!({ "messages": transformed }))
            }
        },
    ))
}

fn register_convert_to_llm(client: &Arc<III>) -> FunctionRef {
    client.register_function((
        RegisterFunctionMessage::with_id("agent::convert_to_llm".to_string())
            .with_description("AgentMessage[] -> Message[] (pure)".to_string()),
        move |payload: Value| async move {
            // Pure passthrough at this layer; provider-specific shaping happens
            // inside the provider crates. Preserves ordering and reshapes the
            // envelope so callers can swap this out without changing the wire.
            let messages = payload
                .get("messages")
                .cloned()
                .unwrap_or_else(|| json!([]));
            Ok(json!({ "messages": messages }))
        },
    ))
}

fn register_get_steering(client: &Arc<III>) -> FunctionRef {
    let client_for_handler = client.clone();
    client.register_function((
        RegisterFunctionMessage::with_id("agent::get_steering".to_string())
            .with_description("Drain steering queue".to_string()),
        move |payload: Value| {
            let client = client_for_handler.clone();
            async move {
                let session_id = required_str(&payload, "session_id")?;
                let bridge_client = Arc::new(IiiSdkClient::new(client));
                let runtime = IiiBridgeRuntime::new(
                    bridge_client,
                    Arc::new(no_provider_handler()) as Arc<StreamAssistantHandler>,
                );
                let drained = runtime.drain_steering(&session_id).await;
                Ok(json!({ "messages": drained }))
            }
        },
    ))
}

fn register_get_followup(client: &Arc<III>) -> FunctionRef {
    let client_for_handler = client.clone();
    client.register_function((
        RegisterFunctionMessage::with_id("agent::get_followup".to_string())
            .with_description("Drain follow-up queue".to_string()),
        move |payload: Value| {
            let client = client_for_handler.clone();
            async move {
                let session_id = required_str(&payload, "session_id")?;
                let bridge_client = Arc::new(IiiSdkClient::new(client));
                let runtime = IiiBridgeRuntime::new(
                    bridge_client,
                    Arc::new(no_provider_handler()) as Arc<StreamAssistantHandler>,
                );
                let drained = runtime.drain_followup(&session_id).await;
                Ok(json!({ "messages": drained }))
            }
        },
    ))
}

fn register_abort(client: &Arc<III>) -> FunctionRef {
    let client_for_handler = client.clone();
    client.register_function((
        RegisterFunctionMessage::with_id("agent::abort".to_string())
            .with_description("Set abort signal in state".to_string()),
        move |payload: Value| {
            let client = client_for_handler.clone();
            async move {
                let session_id = required_str(&payload, "session_id")?;
                let bridge_client = IiiSdkClient::new(client);
                use crate::client::IiiClientLike;
                bridge_client
                    .state_set(
                        state_keys::SCOPE,
                        &state_keys::abort_signal(&session_id),
                        json!(true),
                    )
                    .await
                    .map_err(|e| iii_sdk::IIIError::Handler(e.to_string()))?;
                Ok(json!({ "ok": true }))
            }
        },
    ))
}

fn register_push_steering(client: &Arc<III>) -> FunctionRef {
    let client_for_handler = client.clone();
    client.register_function((
        RegisterFunctionMessage::with_id("agent::push_steering".to_string())
            .with_description("Append messages to a session's steering queue".to_string()),
        move |payload: Value| {
            let client = client_for_handler.clone();
            async move { push_queue(client, payload, state_keys::steering).await }
        },
    ))
}

fn register_push_followup(client: &Arc<III>) -> FunctionRef {
    let client_for_handler = client.clone();
    client.register_function((
        RegisterFunctionMessage::with_id("agent::push_followup".to_string())
            .with_description("Append messages to a session's follow-up queue".to_string()),
        move |payload: Value| {
            let client = client_for_handler.clone();
            async move { push_queue(client, payload, state_keys::followup).await }
        },
    ))
}

async fn push_queue(
    client: Arc<III>,
    payload: Value,
    key_fn: fn(&str) -> String,
) -> Result<Value, iii_sdk::IIIError> {
    use crate::client::IiiClientLike;
    let session_id = required_str(&payload, "session_id")?;
    let messages: Vec<AgentMessage> = payload
        .get("messages")
        .cloned()
        .map(serde_json::from_value)
        .transpose()
        .map_err(|e| iii_sdk::IIIError::Handler(e.to_string()))?
        .unwrap_or_default();
    let bridge_client = IiiSdkClient::new(client);
    let key = key_fn(&session_id);
    let current = bridge_client
        .state_get(state_keys::SCOPE, &key)
        .await
        .map_err(|e| iii_sdk::IIIError::Handler(e.to_string()))?;
    let mut existing: Vec<AgentMessage> = current
        .map(serde_json::from_value)
        .transpose()
        .map_err(|e| iii_sdk::IIIError::Handler(e.to_string()))?
        .unwrap_or_default();
    existing.extend(messages);
    bridge_client
        .state_set(
            state_keys::SCOPE,
            &key,
            serde_json::to_value(&existing)
                .map_err(|e| iii_sdk::IIIError::Handler(e.to_string()))?,
        )
        .await
        .map_err(|e| iii_sdk::IIIError::Handler(e.to_string()))?;
    Ok(json!({ "ok": true, "queued": existing.len() }))
}

fn register_http(
    client: &Arc<III>,
    api_path: &str,
    function_id: &str,
) -> Result<Trigger, BridgeError> {
    client
        .register_trigger(RegisterTriggerInput {
            trigger_type: "http".to_string(),
            function_id: function_id.to_string(),
            config: json!({
                "api_path": api_path,
                "http_method": "POST",
            }),
            metadata: None,
        })
        .map_err(|e| BridgeError::Sdk(e.to_string()))
}

fn required_str(payload: &Value, field: &str) -> Result<String, iii_sdk::IIIError> {
    payload
        .get(field)
        .and_then(Value::as_str)
        .map(ToString::to_string)
        .ok_or_else(|| iii_sdk::IIIError::Handler(format!("missing required field: {field}")))
}

fn required_value(payload: &Value, field: &str) -> Result<Value, iii_sdk::IIIError> {
    payload
        .get(field)
        .cloned()
        .ok_or_else(|| iii_sdk::IIIError::Handler(format!("missing required field: {field}")))
}

/// Stand-in `stream_assistant` used by handlers that never trigger a
/// streaming call (`agent::get_steering`, `agent::get_followup`,
/// `agent::abort`). Returns an error assistant if invoked by mistake.
fn no_provider_handler() -> impl Fn(
    StreamAssistantInput,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = AssistantMessage> + Send>>
       + Send
       + Sync
       + 'static {
    |_input: StreamAssistantInput| {
        Box::pin(async move {
            crate::runtime::error_assistant(
                "this handler does not call stream_assistant; provider not wired",
            )
        }) as _
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use harness_types::{AgentMessage, ContentBlock, TextContent, ToolCall, UserMessage};
    use serde_json::json;

    #[test]
    fn run_loop_payload_roundtrips() {
        let messages = vec![AgentMessage::User(UserMessage {
            content: vec![ContentBlock::Text(TextContent { text: "hi".into() })],
            timestamp: 1,
        })];
        let payload = json!({
            "session_id": "s1",
            "messages": messages,
            "tools": [],
        });
        let session_id = required_str(&payload, "session_id").unwrap();
        assert_eq!(session_id, "s1");
        let parsed: Vec<AgentMessage> =
            serde_json::from_value(payload.get("messages").cloned().unwrap()).unwrap();
        assert_eq!(parsed.len(), 1);
    }

    #[test]
    fn prepare_tool_payload_extracts_tool_call() {
        let tool_call = ToolCall {
            id: "x".into(),
            name: "echo".into(),
            arguments: json!({ "text": "hello" }),
        };
        let payload = json!({ "tool_call": tool_call });
        let extracted = required_value(&payload, "tool_call").unwrap();
        let back: ToolCall = serde_json::from_value(extracted).unwrap();
        assert_eq!(back, tool_call);
    }

    #[test]
    fn required_str_reports_missing_field() {
        let err = required_str(&json!({}), "session_id").unwrap_err();
        assert!(err.to_string().contains("session_id"));
    }
}
