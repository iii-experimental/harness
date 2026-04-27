//! Live iii subscriber across the three hook topics that the harness loop fans
//! out on:
//!
//! - `agent::before_tool_call`
//! - `agent::after_tool_call`
//! - `agent::transform_context`
//!
//! When the loop publishes a hook event, every subscriber on the same topic
//! receives the payload independently. This crate registers a single example
//! subscriber per topic — a denylist guard for `before_tool_call`, a logger
//! for `after_tool_call`, and a no-op pass-through for `transform_context`.
//!
//! ## Adding a custom hook
//!
//! A custom hook is just another iii subscriber. Connect to the same engine,
//! register your function, and bind a `subscribe` trigger to one of the three
//! topics:
//!
//! ```no_run
//! use iii_sdk::{
//!     register_worker, InitOptions, RegisterFunctionMessage, RegisterTriggerInput,
//! };
//! use serde_json::{json, Value};
//!
//! let iii = register_worker("ws://localhost:49134", InitOptions::default());
//! iii.register_function((
//!     RegisterFunctionMessage::with_id("my_hooks::audit".into()),
//!     |payload: Value| async move {
//!         println!("audit: {payload}");
//!         Ok(json!({}))
//!     },
//! ));
//! iii.register_trigger(RegisterTriggerInput {
//!     trigger_type: "subscribe".into(),
//!     function_id: "my_hooks::audit".into(),
//!     config: json!({ "topic": "agent::before_tool_call" }),
//!     metadata: None,
//! })
//! .expect("register subscriber");
//! ```
//!
//! Subscribers are independent. Multiple workers can subscribe to the same
//! topic and the loop will fan out to each. There is no central registry to
//! update — the engine routes by topic name.

use std::collections::HashSet;
use std::sync::Arc;

use iii_sdk::{FunctionRef, IIIError, RegisterFunctionMessage, RegisterTriggerInput, Trigger, III};
use serde_json::{json, Value};
use tokio::sync::Mutex;

/// Topic constants the loop publishes on.
pub mod topics {
    pub const BEFORE_TOOL_CALL: &str = "agent::before_tool_call";
    pub const AFTER_TOOL_CALL: &str = "agent::after_tool_call";
    pub const TRANSFORM_CONTEXT: &str = "agent::transform_context";
}

/// Function ids used by the example subscribers.
pub mod function_ids {
    pub const BEFORE: &str = "hook_example::before_tool_call";
    pub const AFTER: &str = "hook_example::after_tool_call";
    pub const TRANSFORM: &str = "hook_example::transform_context";
}

/// Configuration for the example subscriber set.
#[derive(Debug, Clone, Default)]
pub struct HookExampleConfig {
    /// Tool names to block in the `before_tool_call` subscriber. A blocked
    /// call returns `{ "block": true, "reason": "..." }`; the loop merges
    /// subscriber responses to decide whether to skip the dispatch.
    pub denied_tools: HashSet<String>,
}

/// Counters for observing what the subscribers saw. Useful for tests and the
/// binary's stdout summary.
#[derive(Debug, Default)]
pub struct HookCounters {
    pub before_seen: u64,
    pub before_blocked: u64,
    pub after_seen: u64,
    pub transform_seen: u64,
}

/// Live subscriber set returned by [`register_with_iii`]. Drop the value or
/// call [`unregister_all`](Self::unregister_all) to detach every binding from
/// the engine.
pub struct HookSubscribers {
    function_refs: Vec<FunctionRef>,
    triggers: Vec<Trigger>,
    pub counters: Arc<Mutex<HookCounters>>,
}

impl HookSubscribers {
    pub fn unregister_all(mut self) {
        for t in self.triggers.drain(..) {
            t.unregister();
        }
        for f in self.function_refs.drain(..) {
            f.unregister();
        }
    }
}

/// Register one subscriber on each of the three hook topics. Returns a
/// [`HookSubscribers`] handle that owns the bindings and exposes counters
/// observers can poll for activity proof.
pub fn register_with_iii(
    iii: &III,
    config: HookExampleConfig,
) -> Result<HookSubscribers, IIIError> {
    let counters = Arc::new(Mutex::new(HookCounters::default()));

    let mut function_refs: Vec<FunctionRef> = Vec::with_capacity(3);
    let mut triggers: Vec<Trigger> = Vec::with_capacity(3);

    let denied = config.denied_tools;
    let counters_for_before = counters.clone();
    function_refs.push(iii.register_function((
        RegisterFunctionMessage::with_id(function_ids::BEFORE.into()).with_description(
            "before_tool_call subscriber: blocks tool calls whose name is on the denylist".into(),
        ),
        move |payload: Value| {
            let denied = denied.clone();
            let counters = counters_for_before.clone();
            async move {
                let tool_name = payload
                    .get("tool_call")
                    .and_then(|tc| tc.get("name"))
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                let mut state = counters.lock().await;
                state.before_seen += 1;
                if denied.contains(&tool_name) {
                    state.before_blocked += 1;
                    return Ok(json!({
                        "block": true,
                        "reason": format!("denylist blocked: {tool_name}"),
                    }));
                }
                Ok(json!({ "block": false }))
            }
        },
    )));
    triggers.push(iii.register_trigger(RegisterTriggerInput {
        trigger_type: "subscribe".into(),
        function_id: function_ids::BEFORE.into(),
        config: json!({ "topic": topics::BEFORE_TOOL_CALL }),
        metadata: None,
    })?);

    let counters_for_after = counters.clone();
    function_refs.push(iii.register_function((
        RegisterFunctionMessage::with_id(function_ids::AFTER.into()).with_description(
            "after_tool_call subscriber: logs the tool name and is_error flag for audit".into(),
        ),
        move |payload: Value| {
            let counters = counters_for_after.clone();
            async move {
                let tool_name = payload
                    .get("tool_call")
                    .and_then(|tc| tc.get("name"))
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                let is_error = payload
                    .get("result")
                    .and_then(|r| r.get("is_error"))
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                let mut state = counters.lock().await;
                state.after_seen += 1;
                tracing::info!(tool = %tool_name, is_error, "after_tool_call");
                Ok(json!({ "ok": true }))
            }
        },
    )));
    triggers.push(iii.register_trigger(RegisterTriggerInput {
        trigger_type: "subscribe".into(),
        function_id: function_ids::AFTER.into(),
        config: json!({ "topic": topics::AFTER_TOOL_CALL }),
        metadata: None,
    })?);

    let counters_for_transform = counters.clone();
    function_refs.push(iii.register_function((
        RegisterFunctionMessage::with_id(function_ids::TRANSFORM.into()).with_description(
            "transform_context subscriber: pass-through that records observation count".into(),
        ),
        move |payload: Value| {
            let counters = counters_for_transform.clone();
            async move {
                let mut state = counters.lock().await;
                state.transform_seen += 1;
                let messages = payload
                    .get("messages")
                    .cloned()
                    .unwrap_or_else(|| json!([]));
                Ok(json!({ "messages": messages }))
            }
        },
    )));
    triggers.push(iii.register_trigger(RegisterTriggerInput {
        trigger_type: "subscribe".into(),
        function_id: function_ids::TRANSFORM.into(),
        config: json!({ "topic": topics::TRANSFORM_CONTEXT }),
        metadata: None,
    })?);

    Ok(HookSubscribers {
        function_refs,
        triggers,
        counters,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_denied_tools_default_empty() {
        let cfg = HookExampleConfig::default();
        assert!(cfg.denied_tools.is_empty());
    }

    #[test]
    fn topics_match_runtime_constants() {
        assert_eq!(topics::BEFORE_TOOL_CALL, "agent::before_tool_call");
        assert_eq!(topics::AFTER_TOOL_CALL, "agent::after_tool_call");
        assert_eq!(topics::TRANSFORM_CONTEXT, "agent::transform_context");
    }

    #[test]
    fn function_ids_are_namespaced() {
        for id in [
            function_ids::BEFORE,
            function_ids::AFTER,
            function_ids::TRANSFORM,
        ] {
            assert!(id.starts_with("hook_example::"), "{id}");
        }
    }
}
