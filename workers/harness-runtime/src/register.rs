//! iii-engine registration for the agent loop and the seven built-in tools.
//!
//! [`register_with_iii`] turns this crate from a pure state machine into a
//! live worker: every [`run_loop`] step, every built-in tool, and every
//! state read flows through `iii.trigger`. The CLI calls this once at
//! startup; everything else dispatches over the bus.
//!
//! ## Function inventory
//!
//! | function id              | role |
//! |--------------------------|------|
//! | `agent::run_loop`        | Drive a session start to end. |
//! | `agent::stream_assistant`| Provider router; calls `provider::<name>::stream_assistant`. |
//! | `agent::prepare_tool`    | `before_tool_call` pubsub fan-out. |
//! | `agent::execute_tool`    | Dispatch to `tool::<name>`. |
//! | `agent::finalize_tool`   | `after_tool_call` pubsub merge. |
//! | `agent::transform_context` | Pubsub-pipeline context transform. |
//! | `agent::convert_to_llm`  | Pure passthrough; provider crates override. |
//! | `agent::get_steering`    | Drain steering queue. |
//! | `agent::get_followup`    | Drain follow-up queue. |
//! | `agent::abort`           | Set abort signal in iii state. |
//! | `tool::read|write|edit|ls|grep|find|bash` | Built-in tools. |
//!
//! ## State key layout
//!
//! All keys live under scope `agent`:
//!
//! ```text
//! session/<id>/steering       -> Vec<AgentMessage>
//! session/<id>/followup       -> Vec<AgentMessage>
//! session/<id>/abort_signal   -> bool
//! ```
//!
//! ## tool::bash discovery
//!
//! The `tool::bash` handler probes `iii.list_functions()` on each call and
//! dispatches to `sandbox::exec` if the iii-sandbox worker is loaded. If
//! not, it spawns a host-process bash. There is no user flag — the bus is
//! the source of truth.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use harness_types::{
    AgentEvent, AgentMessage, AgentTool, AssistantMessage, ContentBlock, ErrorKind, ExecutionMode,
    StopReason, TextContent, ToolCall, ToolResult,
};
use iii_sdk::{
    IIIError, RegisterFunctionMessage, RegisterTriggerInput, TriggerRequest, Value, III,
};
use serde_json::json;
use tokio::sync::Mutex;
use uuid::Uuid;

use crate::loop_state::{run_loop, LoopConfig};
use crate::runtime::{EventSink, HookOutcome, LoopRuntime, ToolHandler};
use crate::tools::{EditTool, FindTool, GrepTool, LsTool, ReadTool, WriteTool};

/// Default per-hook collection window. Subscribers that don't reply within
/// this many milliseconds are dropped from the merge.
const DEFAULT_HOOK_TIMEOUT_MS: u64 = 5_000;

/// Stream name for agent events.
pub const EVENTS_STREAM: &str = "agent::events";

/// State scope shared by all session keys.
pub const STATE_SCOPE: &str = "agent";

/// Hook topic ids.
pub const TOPIC_BEFORE: &str = "agent::before_tool_call";
pub const TOPIC_AFTER: &str = "agent::after_tool_call";
pub const TOPIC_TRANSFORM: &str = "agent::transform_context";

fn key_steering(session_id: &str) -> String {
    format!("session/{session_id}/steering")
}
fn key_followup(session_id: &str) -> String {
    format!("session/{session_id}/followup")
}
fn key_abort(session_id: &str) -> String {
    format!("session/{session_id}/abort_signal")
}

/// Register the canonical agent and tool functions on `iii`.
///
/// Provider crates must register `provider::<name>::stream_assistant` (or
/// equivalent) separately so `agent::stream_assistant` can route to them.
/// Tools are auto-discovered on the bus by `tool::*` prefix; nothing
/// downstream needs to know about the builtins listed here.
pub async fn register_with_iii(iii: &III) -> anyhow::Result<()> {
    register_run_loop(iii);
    register_stream_assistant(iii);
    register_prepare_tool(iii);
    register_execute_tool(iii);
    register_finalize_tool(iii);
    register_transform_context(iii);
    register_convert_to_llm(iii);
    register_get_steering(iii);
    register_get_followup(iii);
    register_abort(iii);
    register_push_steering(iii);
    register_push_followup(iii);

    register_tool_simple(iii, "tool::read", "Read a file.", Arc::new(ReadTool));
    register_tool_simple(iii, "tool::write", "Write a file.", Arc::new(WriteTool));
    register_tool_simple(
        iii,
        "tool::edit",
        "Edit a file in place.",
        Arc::new(EditTool),
    );
    register_tool_simple(iii, "tool::ls", "List a directory.", Arc::new(LsTool));
    register_tool_simple(iii, "tool::grep", "Substring search.", Arc::new(GrepTool));
    register_tool_simple(
        iii,
        "tool::find",
        "Find files by suffix.",
        Arc::new(FindTool),
    );
    register_tool_bash(iii);
    register_tool_run_subagent(iii);

    register_http(iii, "agent/prompt", "agent::run_loop")?;
    register_http(iii, "agent/{session_id}/steer", "agent::push_steering")?;
    register_http(iii, "agent/{session_id}/abort", "agent::abort")?;
    register_http(iii, "agent/{session_id}/follow_up", "agent::push_followup")?;

    Ok(())
}

/// `LoopRuntime` impl that routes every concern through `iii.trigger`.
struct IiiRuntime {
    iii: III,
    hook_timeout_ms: u64,
    provider: String,
    model: String,
    system_prompt: String,
}

impl IiiRuntime {
    fn new(iii: III) -> Self {
        Self {
            iii,
            hook_timeout_ms: DEFAULT_HOOK_TIMEOUT_MS,
            provider: String::new(),
            model: String::new(),
            system_prompt: String::new(),
        }
    }

    fn with_session(
        iii: III,
        provider: impl Into<String>,
        model: impl Into<String>,
        system_prompt: impl Into<String>,
    ) -> Self {
        Self {
            iii,
            hook_timeout_ms: DEFAULT_HOOK_TIMEOUT_MS,
            provider: provider.into(),
            model: model.into(),
            system_prompt: system_prompt.into(),
        }
    }

    async fn invoke(&self, function_id: &str, payload: Value) -> Result<Value, IIIError> {
        self.iii
            .trigger(TriggerRequest {
                function_id: function_id.to_string(),
                payload,
                action: None,
                timeout_ms: None,
            })
            .await
    }

    async fn state_get(&self, key: &str) -> Option<Value> {
        let resp = self
            .invoke("state::get", json!({ "scope": STATE_SCOPE, "key": key }))
            .await
            .ok()?;
        if resp.is_null() {
            None
        } else {
            Some(resp)
        }
    }

    async fn state_set(&self, key: &str, value: Value) {
        let _ = self
            .invoke(
                "state::set",
                json!({ "scope": STATE_SCOPE, "key": key, "value": value }),
            )
            .await;
    }

    async fn drain_queue(&self, key: &str) -> Vec<AgentMessage> {
        let Some(current) = self.state_get(key).await else {
            return Vec::new();
        };
        let messages: Vec<AgentMessage> = serde_json::from_value(current).unwrap_or_default();
        if !messages.is_empty() {
            self.state_set(key, json!([])).await;
        }
        messages
    }

    async fn list_function_ids(&self) -> Vec<String> {
        self.iii
            .list_functions()
            .await
            .map(|infos| infos.into_iter().map(|f| f.function_id).collect())
            .unwrap_or_default()
    }

    /// Publish a hook event to `topic` and collect subscriber replies.
    ///
    /// `iii-sdk` 0.11 has no `publish_collect` primitive yet (see
    /// `docs/SDK-BLOCKED.md`). The harness-side workaround is:
    ///
    /// 1. Mint a fresh `event_id` (uuid v4).
    /// 2. Publish the payload with `event_id` baked in. Subscribers route
    ///    through `subscribe` triggers, do their work, then `stream::set`
    ///    their reply on the per-event group `agent::hook_reply/<event_id>`.
    /// 3. Wait `timeout_ms`, polling `stream::list` to gather everything
    ///    that arrived before the deadline.
    /// 4. Return the reply list. The contract for empty/unreachable bus is
    ///    "no replies"; merge logic falls through to the default outcome.
    ///
    /// This is end-to-end best-effort. If the bus drops the publish, no
    /// subscriber writes a reply; if a subscriber misbehaves, the timeout
    /// drops it from the merge.
    async fn publish_collect(&self, topic: &str, data: Value, timeout_ms: u64) -> Vec<Value> {
        let event_id = Uuid::new_v4().to_string();
        let stream_name = HOOK_REPLY_STREAM;
        let _ = self
            .invoke(
                "publish",
                json!({
                    "topic": topic,
                    "data": {
                        "event_id": event_id,
                        "reply_stream": stream_name,
                        "payload": data,
                    },
                }),
            )
            .await;

        let deadline =
            std::time::Instant::now() + std::time::Duration::from_millis(timeout_ms.max(50));
        let mut collected: Vec<Value> = Vec::new();
        let mut last_index: usize = 0;
        loop {
            let resp = self
                .invoke(
                    "stream::list",
                    json!({
                        "stream_name": stream_name,
                        "group_id": event_id,
                    }),
                )
                .await;
            if let Ok(value) = resp {
                if let Some(items) = value.get("items").and_then(Value::as_array) {
                    for item in items.iter().skip(last_index) {
                        if let Some(d) = item.get("data") {
                            collected.push(d.clone());
                        }
                    }
                    last_index = items.len();
                }
            }
            if std::time::Instant::now() >= deadline {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
        collected
    }
}

/// Stream name where hook subscribers write their replies.
///
/// Group_id is the per-event uuid. Any custom hook subscriber MUST
/// `stream::set` its reply value here with `group_id = event_id`.
pub const HOOK_REPLY_STREAM: &str = "agent::hook_reply";

#[async_trait]
impl LoopRuntime for IiiRuntime {
    async fn stream_assistant(
        &self,
        session_id: &str,
        messages: &[AgentMessage],
        tools: &[AgentTool],
    ) -> AssistantMessage {
        let payload = json!({
            "session_id": session_id,
            "provider": self.provider,
            "model": self.model,
            "system_prompt": self.system_prompt,
            "messages": messages,
            "tools": tools,
        });
        match self.invoke("agent::stream_assistant", payload).await {
            Ok(value) => serde_json::from_value(value)
                .unwrap_or_else(|e| error_assistant(&format!("decode failed: {e}"))),
            Err(err) => error_assistant(&format!("agent::stream_assistant failed: {err}")),
        }
    }

    async fn resolve_tool(&self, name: &str) -> Option<Arc<dyn ToolHandler>> {
        let function_id = format!("tool::{name}");
        let ids = self.list_function_ids().await;
        if ids.iter().any(|id| id == &function_id) {
            Some(Arc::new(BusToolHandler {
                iii: self.iii.clone(),
                function_id,
            }) as Arc<dyn ToolHandler>)
        } else {
            None
        }
    }

    async fn before_tool_call(&self, tool_call: &ToolCall) -> HookOutcome {
        let replies = self
            .publish_collect(
                TOPIC_BEFORE,
                json!({ "tool_call": tool_call }),
                self.hook_timeout_ms,
            )
            .await;
        crate::hooks::merge_before(&replies)
    }

    async fn after_tool_call(&self, tool_call: &ToolCall, result: ToolResult) -> ToolResult {
        let replies = self
            .publish_collect(
                TOPIC_AFTER,
                json!({ "tool_call": tool_call, "result": result }),
                self.hook_timeout_ms,
            )
            .await;
        crate::hooks::merge_after(result, &replies)
    }

    async fn transform_context(&self, messages: Vec<AgentMessage>) -> Vec<AgentMessage> {
        let replies = self
            .publish_collect(
                TOPIC_TRANSFORM,
                json!({ "messages": messages }),
                self.hook_timeout_ms,
            )
            .await;
        for r in replies.iter().rev() {
            if let Some(decoded) = crate::hooks::decode_transform(r) {
                return decoded;
            }
        }
        messages
    }

    async fn drain_steering(&self, session_id: &str) -> Vec<AgentMessage> {
        self.drain_queue(&key_steering(session_id)).await
    }

    async fn drain_followup(&self, session_id: &str) -> Vec<AgentMessage> {
        self.drain_queue(&key_followup(session_id)).await
    }

    async fn abort_signal(&self, session_id: &str) -> bool {
        self.state_get(&key_abort(session_id))
            .await
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
    }
}

/// `EventSink` that appends each [`AgentEvent`] to `agent::events/<session>`
/// via `stream::set`.
struct IiiSink {
    iii: III,
    session_id: String,
    counter: AtomicU64,
}

impl IiiSink {
    fn new(iii: III, session_id: impl Into<String>) -> Self {
        Self {
            iii,
            session_id: session_id.into(),
            counter: AtomicU64::new(0),
        }
    }

    fn next_item_id(&self) -> String {
        let seq = self.counter.fetch_add(1, Ordering::Relaxed);
        format!("{}-{seq:08}", self.session_id)
    }
}

#[async_trait]
impl EventSink for IiiSink {
    async fn emit(&self, event: AgentEvent) {
        let payload = match serde_json::to_value(&event) {
            Ok(v) => v,
            Err(err) => {
                tracing::warn!(?err, "failed to serialise AgentEvent");
                return;
            }
        };
        let item_id = self.next_item_id();
        let _ = self
            .iii
            .trigger(TriggerRequest {
                function_id: "stream::set".to_string(),
                payload: json!({
                    "stream_name": EVENTS_STREAM,
                    "group_id": self.session_id,
                    "item_id": item_id,
                    "data": payload,
                }),
                action: None,
                timeout_ms: None,
            })
            .await;
    }
}

/// `ToolHandler` that dispatches to a `tool::<name>` function on the bus.
struct BusToolHandler {
    iii: III,
    function_id: String,
}

#[async_trait]
impl ToolHandler for BusToolHandler {
    async fn execute(&self, tool_call: &ToolCall) -> ToolResult {
        let payload = json!({
            "id": tool_call.id,
            "name": tool_call.name,
            "arguments": tool_call.arguments,
        });
        let response = self
            .iii
            .trigger(TriggerRequest {
                function_id: self.function_id.clone(),
                payload,
                action: None,
                timeout_ms: None,
            })
            .await;
        match response {
            Ok(value) => decode_tool_response(value),
            Err(err) => error_result(&format!(
                "tool '{}' invocation failed: {err}",
                tool_call.name
            )),
        }
    }
}

fn decode_tool_response(response: Value) -> ToolResult {
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

fn error_assistant(reason: &str) -> AssistantMessage {
    AssistantMessage {
        content: Vec::new(),
        stop_reason: StopReason::Error,
        error_message: Some(reason.to_string()),
        error_kind: Some(ErrorKind::Transient),
        usage: None,
        model: "harness-runtime".into(),
        provider: "harness-runtime".into(),
        timestamp: chrono::Utc::now().timestamp_millis(),
    }
}

fn register_run_loop(iii: &III) {
    let iii_for_handler = iii.clone();
    iii.register_function((
        RegisterFunctionMessage::with_id("agent::run_loop".to_string())
            .with_description("Drive the agent loop start to end.".to_string()),
        move |payload: Value| {
            let iii = iii_for_handler.clone();
            async move {
                let session_id = required_str(&payload, "session_id")?;
                let provider = required_str(&payload, "provider")?;
                let model = required_str(&payload, "model")?;
                let system_prompt = payload
                    .get("system_prompt")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                let incoming =
                    decode_field::<Vec<AgentMessage>>(&payload, "messages")?.unwrap_or_default();
                let tools = decode_field::<Vec<AgentTool>>(&payload, "tools")?.unwrap_or_default();

                // Persistent-session integration: when `session-tree` is
                // registered on the bus, hydrate the existing transcript via
                // `session::messages`, append the incoming user batch via
                // `session::append`, and persist new turns at exit. When the
                // worker isn't loaded, both calls fail-soft and the loop runs
                // with the in-memory `Vec` it always has.
                let persisted = persisted_load_messages(&iii, &session_id).await;
                let baseline_count = persisted.len();
                let mut messages: Vec<AgentMessage> = if persisted.is_empty() {
                    incoming.clone()
                } else {
                    let mut combined = persisted;
                    combined.extend(incoming.clone());
                    combined
                };
                for m in &incoming {
                    let _ = persisted_append(&iii, &session_id, m).await;
                }

                let runtime = IiiRuntime::with_session(iii.clone(), provider, model, system_prompt);
                let sink = IiiSink::new(iii.clone(), session_id.clone());
                let cfg = LoopConfig {
                    session_id: session_id.clone(),
                    tools,
                    default_execution_mode: ExecutionMode::Parallel,
                };
                let outcome = run_loop(&runtime, &sink, &cfg, std::mem::take(&mut messages)).await;

                // Append every message produced during the loop (post-baseline)
                // back to the session-tree store, so the next call resumes
                // from a complete transcript.
                let total = outcome.messages.len();
                let new_floor = baseline_count + incoming.len();
                if total > new_floor {
                    for m in outcome.messages.iter().skip(new_floor) {
                        let _ = persisted_append(&iii, &session_id, m).await;
                    }
                }

                Ok(json!({ "messages": outcome.messages }))
            }
        },
    ));
}

/// Best-effort: ask `session::messages` for the active path's transcript.
/// Returns `Vec::new()` if the function isn't registered, the call errors,
/// or the response is empty/malformed. Never propagates failure.
async fn persisted_load_messages(iii: &III, session_id: &str) -> Vec<AgentMessage> {
    let resp = iii
        .trigger(TriggerRequest {
            function_id: "session::messages".to_string(),
            payload: json!({ "session_id": session_id }),
            action: None,
            timeout_ms: None,
        })
        .await;
    let value = match resp {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };
    value
        .get("messages")
        .cloned()
        .map(serde_json::from_value::<Vec<AgentMessage>>)
        .transpose()
        .ok()
        .flatten()
        .unwrap_or_default()
}

/// Best-effort: append one message to the session-tree store via
/// `session::append`. Silently swallows missing-function / error responses
/// so the loop can run without `session-tree` registered.
async fn persisted_append(iii: &III, session_id: &str, message: &AgentMessage) {
    let payload = match serde_json::to_value(message) {
        Ok(v) => v,
        Err(_) => return,
    };
    let _ = iii
        .trigger(TriggerRequest {
            function_id: "session::append".to_string(),
            payload: json!({ "session_id": session_id, "message": payload }),
            action: None,
            timeout_ms: None,
        })
        .await;
}

fn register_stream_assistant(iii: &III) {
    let iii_for_handler = iii.clone();
    // One-shot cache for `llm-router::route` presence. Resolved on the first
    // `agent::stream_assistant` invocation — after that, every turn skips the
    // bus list_functions call and reads an atomic. Topology is assumed fixed
    // for the lifetime of the registered handler. If a user adds llm-router
    // mid-session (rare; harnessd / CLI register-then-invoke patterns set
    // topology before the loop runs), restart to pick it up.
    let router_cache: Arc<RouterPresenceCache> = Arc::new(RouterPresenceCache::default());
    iii.register_function((
        RegisterFunctionMessage::with_id("agent::stream_assistant".to_string())
            .with_description("Route a stream call to the configured provider worker, optionally going through llm-router::route when present.".to_string()),
        move |payload: Value| {
            let iii = iii_for_handler.clone();
            let cache = router_cache.clone();
            async move {
                let original_provider = payload
                    .get("provider")
                    .and_then(Value::as_str)
                    .or_else(|| payload.get("provider_name").and_then(Value::as_str))
                    .ok_or_else(|| {
                        IIIError::Handler("missing required field: provider".to_string())
                    })?
                    .to_string();

                // llm-router integration. When `llm-router::route` is
                // registered on the bus (i.e. user ran `iii worker add
                // llm-router`), call it first. The router can swap the
                // payload's `provider` / `model` based on capability rules,
                // model catalog, cost, etc. When absent, the request is
                // dispatched directly to the configured provider.
                let mut routed_payload = payload.clone();
                let mut provider = original_provider.clone();
                if cache.has_router(&iii).await {
                    if let Ok(resolved) = iii
                        .trigger(TriggerRequest {
                            function_id: "llm-router::route".to_string(),
                            payload: payload.clone(),
                            action: None,
                            timeout_ms: None,
                        })
                        .await
                    {
                        if let Some(p) = resolved.get("provider").and_then(Value::as_str) {
                            provider = p.to_string();
                            routed_payload["provider"] = Value::String(p.to_string());
                        }
                        if let Some(m) = resolved.get("model").and_then(Value::as_str) {
                            routed_payload["model"] = Value::String(m.to_string());
                        }
                    }
                }

                let target = format!("provider::{provider}::stream_assistant");
                let assistant = match iii
                    .trigger(TriggerRequest {
                        function_id: target.clone(),
                        payload: routed_payload,
                        action: None,
                        timeout_ms: None,
                    })
                    .await
                {
                    Ok(v) => v,
                    Err(err) => serde_json::to_value(error_assistant(&format!(
                        "{target} not registered or failed: {err}"
                    )))
                    .map_err(|e| IIIError::Handler(e.to_string()))?,
                };
                Ok(assistant)
            }
        },
    ));
}

/// One-shot lookup cache for the `llm-router::route` function. The first
/// caller probes `iii.list_functions()`; later callers read an atomic.
/// Concurrent first-callers are serialised by the `Mutex` so we issue at
/// most one bus probe per process lifetime.
#[derive(Default)]
struct RouterPresenceCache {
    init: tokio::sync::Mutex<bool>,
    present: std::sync::atomic::AtomicBool,
}

impl RouterPresenceCache {
    async fn has_router(&self, iii: &III) -> bool {
        // Fast path: already initialised.
        let mut guard = self.init.lock().await;
        if *guard {
            return self.present.load(std::sync::atomic::Ordering::Acquire);
        }
        // Slow path: probe once, record, release.
        let present = iii
            .list_functions()
            .await
            .map(|infos| infos.iter().any(|f| f.function_id == "llm-router::route"))
            .unwrap_or(false);
        self.present
            .store(present, std::sync::atomic::Ordering::Release);
        *guard = true;
        present
    }
}

fn register_prepare_tool(iii: &III) {
    let iii_for_handler = iii.clone();
    iii.register_function((
        RegisterFunctionMessage::with_id("agent::prepare_tool".to_string())
            .with_description("Run before_tool_call hook fan-out.".to_string()),
        move |payload: Value| {
            let iii = iii_for_handler.clone();
            async move {
                let tool_call = decode_required::<ToolCall>(&payload, "tool_call")?;
                let runtime = IiiRuntime::new(iii);
                let outcome = runtime.before_tool_call(&tool_call).await;
                Ok(json!({ "block": outcome.block, "reason": outcome.reason }))
            }
        },
    ));
}

fn register_execute_tool(iii: &III) {
    let iii_for_handler = iii.clone();
    iii.register_function((
        RegisterFunctionMessage::with_id("agent::execute_tool".to_string())
            .with_description("Dispatch a tool call to tool::<name>.".to_string()),
        move |payload: Value| {
            let iii = iii_for_handler.clone();
            async move {
                let tool_call = decode_required::<ToolCall>(&payload, "tool_call")?;
                let runtime = IiiRuntime::new(iii);
                let result = match runtime.resolve_tool(&tool_call.name).await {
                    Some(handler) => handler.execute(&tool_call).await,
                    None => error_result(&format!("tool not found: {}", tool_call.name)),
                };
                serde_json::to_value(result).map_err(|e| IIIError::Handler(e.to_string()))
            }
        },
    ));
}

fn register_finalize_tool(iii: &III) {
    let iii_for_handler = iii.clone();
    iii.register_function((
        RegisterFunctionMessage::with_id("agent::finalize_tool".to_string())
            .with_description("Run after_tool_call hook merge.".to_string()),
        move |payload: Value| {
            let iii = iii_for_handler.clone();
            async move {
                let tool_call = decode_required::<ToolCall>(&payload, "tool_call")?;
                let result = decode_required::<ToolResult>(&payload, "result")?;
                let runtime = IiiRuntime::new(iii);
                let merged = runtime.after_tool_call(&tool_call, result).await;
                serde_json::to_value(merged).map_err(|e| IIIError::Handler(e.to_string()))
            }
        },
    ));
}

fn register_transform_context(iii: &III) {
    let iii_for_handler = iii.clone();
    iii.register_function((
        RegisterFunctionMessage::with_id("agent::transform_context".to_string())
            .with_description("Run transform_context pubsub pipeline.".to_string()),
        move |payload: Value| {
            let iii = iii_for_handler.clone();
            async move {
                let messages =
                    decode_field::<Vec<AgentMessage>>(&payload, "messages")?.unwrap_or_default();
                let runtime = IiiRuntime::new(iii);
                let transformed = runtime.transform_context(messages).await;
                Ok(json!({ "messages": transformed }))
            }
        },
    ));
}

fn register_convert_to_llm(iii: &III) {
    iii.register_function((
        RegisterFunctionMessage::with_id("agent::convert_to_llm".to_string())
            .with_description("Pure passthrough; provider crates override.".to_string()),
        |payload: Value| async move {
            let messages = payload
                .get("messages")
                .cloned()
                .unwrap_or_else(|| json!([]));
            Ok(json!({ "messages": messages }))
        },
    ));
}

fn register_get_steering(iii: &III) {
    let iii_for_handler = iii.clone();
    iii.register_function((
        RegisterFunctionMessage::with_id("agent::get_steering".to_string())
            .with_description("Drain steering queue.".to_string()),
        move |payload: Value| {
            let iii = iii_for_handler.clone();
            async move {
                let session_id = required_str(&payload, "session_id")?;
                let runtime = IiiRuntime::new(iii);
                let drained = runtime.drain_steering(&session_id).await;
                Ok(json!({ "messages": drained }))
            }
        },
    ));
}

fn register_get_followup(iii: &III) {
    let iii_for_handler = iii.clone();
    iii.register_function((
        RegisterFunctionMessage::with_id("agent::get_followup".to_string())
            .with_description("Drain follow-up queue.".to_string()),
        move |payload: Value| {
            let iii = iii_for_handler.clone();
            async move {
                let session_id = required_str(&payload, "session_id")?;
                let runtime = IiiRuntime::new(iii);
                let drained = runtime.drain_followup(&session_id).await;
                Ok(json!({ "messages": drained }))
            }
        },
    ));
}

fn register_abort(iii: &III) {
    let iii_for_handler = iii.clone();
    iii.register_function((
        RegisterFunctionMessage::with_id("agent::abort".to_string())
            .with_description("Set abort signal in iii state.".to_string()),
        move |payload: Value| {
            let iii = iii_for_handler.clone();
            async move {
                let session_id = required_str(&payload, "session_id")?;
                let runtime = IiiRuntime::new(iii);
                runtime
                    .state_set(&key_abort(&session_id), json!(true))
                    .await;
                Ok(json!({ "ok": true }))
            }
        },
    ));
}

fn register_push_steering(iii: &III) {
    let iii_for_handler = iii.clone();
    iii.register_function((
        RegisterFunctionMessage::with_id("agent::push_steering".to_string())
            .with_description("Append messages to a session's steering queue.".to_string()),
        move |payload: Value| {
            let iii = iii_for_handler.clone();
            async move { push_queue(iii, payload, key_steering as fn(&str) -> String).await }
        },
    ));
}

fn register_push_followup(iii: &III) {
    let iii_for_handler = iii.clone();
    iii.register_function((
        RegisterFunctionMessage::with_id("agent::push_followup".to_string())
            .with_description("Append messages to a session's follow-up queue.".to_string()),
        move |payload: Value| {
            let iii = iii_for_handler.clone();
            async move { push_queue(iii, payload, key_followup as fn(&str) -> String).await }
        },
    ));
}

async fn push_queue(
    iii: III,
    payload: Value,
    key_fn: fn(&str) -> String,
) -> Result<Value, IIIError> {
    let session_id = required_str(&payload, "session_id")?;
    let messages = decode_field::<Vec<AgentMessage>>(&payload, "messages")?.unwrap_or_default();
    let runtime = IiiRuntime::new(iii);
    let key = key_fn(&session_id);
    let mut existing: Vec<AgentMessage> = runtime
        .state_get(&key)
        .await
        .and_then(|v| serde_json::from_value(v).ok())
        .unwrap_or_default();
    existing.extend(messages);
    let count = existing.len();
    let value = serde_json::to_value(&existing).map_err(|e| IIIError::Handler(e.to_string()))?;
    runtime.state_set(&key, value).await;
    Ok(json!({ "ok": true, "queued": count }))
}

fn register_tool_simple(
    iii: &III,
    function_id: &str,
    description: &str,
    handler: Arc<dyn ToolHandler>,
) {
    iii.register_function((
        RegisterFunctionMessage::with_id(function_id.to_string())
            .with_description(description.to_string()),
        move |payload: Value| {
            let handler = handler.clone();
            async move {
                let tool_call = decode_tool_call(&payload)?;
                let result = handler.execute(&tool_call).await;
                serde_json::to_value(result).map_err(|e| IIIError::Handler(e.to_string()))
            }
        },
    ));
}

/// Register `tool::run_subagent` — a tool that triggers a nested
/// `agent::run_loop` with a child session id and returns the child's final
/// assistant text. The agent advertises this tool whenever the sub-agent
/// pattern is wanted; the LLM picks it up like any other tool.
///
/// Tool args:
/// - `prompt` (string, required) — the focused subtask the sub-agent should answer
/// - `provider` (string, required) — provider name registered on the bus
/// - `model` (string, required) — model id passed through to the provider
/// - `system_prompt` (string, optional) — defaults to a small "be concise" prompt
/// - `max_turns` (u32, optional) — overrides the parent's max-turns budget
///
/// Returns: a `ToolResult` with the final assistant text and a `details`
/// payload `{ child_session_id, turns, stop_reason }`.
fn register_tool_run_subagent(iii: &III) {
    let iii_for_handler = iii.clone();
    iii.register_function((
        RegisterFunctionMessage::with_id("tool::run_subagent".to_string()).with_description(
            "Spawn a sub-agent for a focused subtask. Returns the sub-agent's final answer."
                .to_string(),
        ),
        move |payload: Value| {
            let iii = iii_for_handler.clone();
            async move {
                let tool_call = decode_tool_call(&payload)?;
                let args = &tool_call.arguments;
                let prompt = args
                    .get("prompt")
                    .and_then(Value::as_str)
                    .ok_or_else(|| IIIError::Handler("missing required arg: prompt".into()))?;
                let provider = args
                    .get("provider")
                    .and_then(Value::as_str)
                    .ok_or_else(|| IIIError::Handler("missing required arg: provider".into()))?;
                let model = args
                    .get("model")
                    .and_then(Value::as_str)
                    .ok_or_else(|| IIIError::Handler("missing required arg: model".into()))?;
                let system_prompt = args
                    .get("system_prompt")
                    .and_then(Value::as_str)
                    .unwrap_or("You are a focused sub-agent. Answer the parent's subtask concisely and stop.")
                    .to_string();

                let parent_session = args
                    .get("parent_session_id")
                    .and_then(Value::as_str)
                    .unwrap_or("root");
                let child_session_id =
                    format!("{parent_session}::sub-{}", chrono::Utc::now().timestamp_millis());

                let child_payload = json!({
                    "session_id": child_session_id,
                    "parent_session_id": parent_session,
                    "provider": provider,
                    "model": model,
                    "system_prompt": system_prompt,
                    "messages": [{
                        "role": "user",
                        "content": [{"type": "text", "text": prompt}],
                        "timestamp": chrono::Utc::now().timestamp_millis(),
                    }],
                    "tools": [],
                });

                let response = iii
                    .trigger(TriggerRequest {
                        function_id: "agent::run_loop".to_string(),
                        payload: child_payload,
                        action: None,
                        timeout_ms: None,
                    })
                    .await;

                let result = match response {
                    Ok(value) => {
                        let messages = value
                            .get("messages")
                            .and_then(Value::as_array)
                            .cloned()
                            .unwrap_or_default();
                        let final_text = messages
                            .iter()
                            .rev()
                            .find_map(|m| {
                                let role = m.get("role").and_then(Value::as_str)?;
                                if role != "assistant" {
                                    return None;
                                }
                                let content = m.get("content").and_then(Value::as_array)?;
                                let text = content
                                    .iter()
                                    .filter_map(|c| {
                                        if c.get("type").and_then(Value::as_str) == Some("text") {
                                            c.get("text").and_then(Value::as_str)
                                        } else {
                                            None
                                        }
                                    })
                                    .collect::<Vec<_>>()
                                    .join("\n");
                                if text.is_empty() {
                                    None
                                } else {
                                    Some(text)
                                }
                            })
                            .unwrap_or_else(|| "<sub-agent returned no text>".to_string());

                        ToolResult {
                            content: vec![ContentBlock::Text(TextContent { text: final_text })],
                            details: json!({
                                "child_session_id": child_session_id,
                                "turn_count": messages.len(),
                                "via": "tool::run_subagent",
                            }),
                            terminate: false,
                        }
                    }
                    Err(e) => ToolResult {
                        content: vec![ContentBlock::Text(TextContent {
                            text: format!("sub-agent failed: {e}"),
                        })],
                        details: json!({
                            "child_session_id": child_session_id,
                            "via": "tool::run_subagent",
                            "error": e.to_string(),
                        }),
                        terminate: false,
                    },
                };

                serde_json::to_value(result).map_err(|e| IIIError::Handler(e.to_string()))
            }
        },
    ));
}

fn register_tool_bash(iii: &III) {
    let iii_for_handler = iii.clone();
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let bash = Arc::new(BashTool::new(iii.clone(), cwd));
    iii.register_function((
        RegisterFunctionMessage::with_id("tool::bash".to_string()).with_description(
            "Run a bash command. Routes to sandbox::exec when available.".to_string(),
        ),
        move |payload: Value| {
            let bash = bash.clone();
            let _ = iii_for_handler.clone();
            async move {
                let tool_call = decode_tool_call(&payload)?;
                let result = bash.execute(&tool_call).await;
                serde_json::to_value(result).map_err(|e| IIIError::Handler(e.to_string()))
            }
        },
    ));
}

fn register_http(iii: &III, api_path: &str, function_id: &str) -> anyhow::Result<()> {
    iii.register_trigger(RegisterTriggerInput {
        trigger_type: "http".to_string(),
        function_id: function_id.to_string(),
        config: json!({ "api_path": api_path, "http_method": "POST" }),
        metadata: None,
    })
    .map_err(|e| anyhow::anyhow!(e.to_string()))?;
    Ok(())
}

fn decode_tool_call(payload: &Value) -> Result<ToolCall, IIIError> {
    if payload.get("name").is_some() && payload.get("arguments").is_some() {
        return serde_json::from_value(payload.clone())
            .map_err(|e| IIIError::Handler(e.to_string()));
    }
    decode_required::<ToolCall>(payload, "tool_call")
}

fn required_str(payload: &Value, field: &str) -> Result<String, IIIError> {
    payload
        .get(field)
        .and_then(Value::as_str)
        .map(ToString::to_string)
        .ok_or_else(|| IIIError::Handler(format!("missing required field: {field}")))
}

fn decode_field<T: serde::de::DeserializeOwned>(
    payload: &Value,
    field: &str,
) -> Result<Option<T>, IIIError> {
    payload
        .get(field)
        .cloned()
        .map(serde_json::from_value)
        .transpose()
        .map_err(|e| IIIError::Handler(e.to_string()))
}

fn decode_required<T: serde::de::DeserializeOwned>(
    payload: &Value,
    field: &str,
) -> Result<T, IIIError> {
    let raw = payload
        .get(field)
        .cloned()
        .ok_or_else(|| IIIError::Handler(format!("missing required field: {field}")))?;
    serde_json::from_value(raw).map_err(|e| IIIError::Handler(e.to_string()))
}

/// `tool::bash` handler. Probes the bus for `sandbox::exec` on every call;
/// if registered, dispatches the command into the sandbox. Otherwise spawns
/// a host-process bash. The discovery is deliberately per-call so a sandbox
/// worker that registers mid-session takes effect immediately.
struct BashTool {
    iii: III,
    cwd: PathBuf,
    sandbox_id: Mutex<Option<String>>,
}

impl BashTool {
    fn new(iii: III, cwd: PathBuf) -> Self {
        Self {
            iii,
            cwd,
            sandbox_id: Mutex::new(None),
        }
    }

    async fn sandbox_available(&self) -> bool {
        match self.iii.list_functions().await {
            Ok(infos) => infos.iter().any(|f| f.function_id == "sandbox::exec"),
            Err(_) => false,
        }
    }

    async fn ensure_sandbox(&self) -> Result<String, String> {
        let mut guard = self.sandbox_id.lock().await;
        if let Some(id) = guard.as_ref() {
            return Ok(id.clone());
        }
        let response = self
            .iii
            .trigger(TriggerRequest {
                function_id: "sandbox::create".to_string(),
                payload: json!({ "image": "python", "idle_timeout": 600 }),
                action: None,
                timeout_ms: None,
            })
            .await
            .map_err(|e| e.to_string())?;
        let id = response
            .get("sandbox_id")
            .and_then(Value::as_str)
            .ok_or_else(|| "sandbox::create returned no sandbox_id".to_string())?
            .to_string();
        *guard = Some(id.clone());
        Ok(id)
    }

    async fn run_via_sandbox(&self, command: &str) -> ToolResult {
        let sandbox_id = match self.ensure_sandbox().await {
            Ok(id) => id,
            Err(e) => return error_result(&format!("sandbox::create failed: {e}")),
        };
        let payload = json!({
            "sandbox_id": sandbox_id,
            "cmd": "bash",
            "args": ["-lc", command],
        });
        match self
            .iii
            .trigger(TriggerRequest {
                function_id: "sandbox::exec".to_string(),
                payload,
                action: None,
                timeout_ms: None,
            })
            .await
        {
            Ok(value) => render_exec_result(&value, "iii-sandbox"),
            Err(e) => error_result(&format!("sandbox::exec failed: {e}")),
        }
    }

    async fn run_on_host(&self, command: &str) -> ToolResult {
        let output = tokio::process::Command::new("bash")
            .arg("-lc")
            .arg(command)
            .current_dir(&self.cwd)
            .output()
            .await;
        match output {
            Ok(o) => {
                let stdout = String::from_utf8_lossy(&o.stdout).to_string();
                let stderr = String::from_utf8_lossy(&o.stderr).to_string();
                let exit = o.status.code().unwrap_or(-1);
                render_exec_result(
                    &json!({ "stdout": stdout, "stderr": stderr, "exit_code": exit }),
                    "host",
                )
            }
            Err(e) => error_result(&format!("bash spawn failed: {e}")),
        }
    }
}

#[async_trait]
impl ToolHandler for BashTool {
    async fn execute(&self, tool_call: &ToolCall) -> ToolResult {
        let command = tool_call
            .arguments
            .get("command")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        if command.is_empty() {
            return error_result("missing required arg: command");
        }
        if self.sandbox_available().await {
            self.run_via_sandbox(&command).await
        } else {
            self.run_on_host(&command).await
        }
    }
}

fn render_exec_result(value: &Value, via: &str) -> ToolResult {
    use std::fmt::Write as _;
    let stdout = value.get("stdout").and_then(Value::as_str).unwrap_or("");
    let stderr = value.get("stderr").and_then(Value::as_str).unwrap_or("");
    let exit_code = value.get("exit_code").and_then(Value::as_i64).unwrap_or(-1);

    let mut text = String::with_capacity(stdout.len() + stderr.len() + 16);
    let _ = writeln!(text, "exit={exit_code}");
    if !stdout.is_empty() {
        text.push_str(stdout);
    }
    if !stderr.is_empty() {
        if !stdout.is_empty() && !stdout.ends_with('\n') {
            text.push('\n');
        }
        text.push_str(stderr);
    }
    let truncated: String = text.chars().take(30_000).collect();
    ToolResult {
        content: vec![ContentBlock::Text(TextContent { text: truncated })],
        details: json!({ "exit_code": exit_code, "via": via }),
        terminate: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_layout_matches_architecture_spec() {
        assert_eq!(key_steering("abc"), "session/abc/steering");
        assert_eq!(key_followup("abc"), "session/abc/followup");
        assert_eq!(key_abort("abc"), "session/abc/abort_signal");
        assert_eq!(STATE_SCOPE, "agent");
        assert_eq!(TOPIC_BEFORE, "agent::before_tool_call");
        assert_eq!(TOPIC_AFTER, "agent::after_tool_call");
        assert_eq!(TOPIC_TRANSFORM, "agent::transform_context");
        assert_eq!(EVENTS_STREAM, "agent::events");
    }

    #[test]
    fn decode_tool_response_accepts_full_tool_result() {
        let raw = json!({
            "content": [{ "type": "text", "text": "hello" }],
            "details": { "exit": 0 },
            "terminate": false,
        });
        let result = decode_tool_response(raw);
        assert!(matches!(result.content[0], ContentBlock::Text(_)));
        assert_eq!(result.details["exit"], json!(0));
    }

    #[test]
    fn decode_tool_response_falls_back_to_textual_form() {
        let raw = json!({ "weird": "shape" });
        let result = decode_tool_response(raw);
        match &result.content[0] {
            ContentBlock::Text(t) => assert!(t.text.contains("weird")),
            _ => panic!("expected text fallback"),
        }
    }

    #[test]
    fn decode_tool_call_accepts_envelope_or_inline() {
        let inline = json!({ "id": "x", "name": "echo", "arguments": { "text": "hi" } });
        let from_inline = decode_tool_call(&inline).unwrap();
        assert_eq!(from_inline.name, "echo");
        let envelope = json!({ "tool_call": inline });
        let from_envelope = decode_tool_call(&envelope).unwrap();
        assert_eq!(from_envelope.name, "echo");
    }

    #[test]
    fn render_exec_includes_stdout_and_stderr() {
        let value = json!({ "stdout": "out\n", "stderr": "err\n", "exit_code": 1 });
        let result = render_exec_result(&value, "host");
        let text = match result.content.first().unwrap() {
            ContentBlock::Text(t) => &t.text,
            _ => panic!(),
        };
        assert!(text.contains("exit=1"));
        assert!(text.contains("out"));
        assert!(text.contains("err"));
        assert_eq!(result.details["via"].as_str(), Some("host"));
    }

    #[test]
    fn error_assistant_carries_reason() {
        let a = error_assistant("boom");
        assert_eq!(a.error_message.as_deref(), Some("boom"));
        assert!(matches!(a.stop_reason, StopReason::Error));
    }
}
