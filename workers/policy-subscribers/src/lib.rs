//! Reference iii hook subscribers for production deployments.
//!
//! Three flavours, each independent — register one or all:
//!
//! - **denylist** (`subscribe_denylist`): subscribes to
//!   `agent::before_tool_call`, blocks calls whose `name` matches a
//!   user-supplied denylist. Replies `{ block: true, reason }`. The runtime's
//!   `merge_before` picks the first blocker.
//! - **audit log** (`subscribe_audit_log`): subscribes to
//!   `agent::after_tool_call`, appends every observed call to a JSON-lines
//!   file at a configurable path. Reply is `{ ok: true }` (informational).
//! - **DLP scrubber** (`subscribe_dlp_scrubber`): subscribes to
//!   `agent::after_tool_call`, scans the result's text content for secrets
//!   (api keys, tokens, AWS access keys), replaces them with redaction
//!   markers, replies `{ content: [<redacted>] }`. The runtime's
//!   `merge_after` overrides the result.
//!
//! All three subscribers follow the v0.10 collected-pubsub envelope contract:
//! receive `{ event_id, reply_stream, payload }`, write the reply via
//! `stream::set` on `(reply_stream, group_id=event_id)` so the publisher
//! (`IiiRuntime::publish_collect`) can collect it.
//!
//! See `harness_runtime::TOPIC_BEFORE` etc. for the topic constants. See
//! `workers/hook-example/src/lib.rs` for a more minimal reference impl.

use std::path::PathBuf;
use std::sync::Arc;

use iii_sdk::{
    FunctionRef, IIIError, RegisterFunctionMessage, RegisterTriggerInput, Trigger, TriggerRequest,
    III,
};
use serde_json::{json, Value};

const FN_DENYLIST: &str = "policy::denylist";
const FN_AUDIT: &str = "policy::audit_log";
const FN_DLP: &str = "policy::dlp_scrubber";

/// Topic the loop publishes on. Re-exported for clarity at the call site.
pub use harness_runtime::{HOOK_REPLY_STREAM, TOPIC_AFTER, TOPIC_BEFORE};

/// Handle owning a single registered subscriber. Drop or call
/// [`unregister`](Self::unregister) to detach the binding from the engine.
pub struct Subscriber {
    function: FunctionRef,
    trigger: Trigger,
}

impl Subscriber {
    /// Detach the subscriber from the bus.
    pub fn unregister(self) {
        self.trigger.unregister();
        self.function.unregister();
    }
}

/// Register a denylist policy subscriber on `agent::before_tool_call`.
///
/// `denied_tools` is matched against the `name` field of each
/// `before_tool_call` payload. A matching call replies
/// `{ block: true, reason: "policy::denylist blocked '<name>'" }`. Anything
/// else replies `{ block: false }`.
pub fn subscribe_denylist(iii: &III, denied_tools: Vec<String>) -> Result<Subscriber, IIIError> {
    let denied: Arc<Vec<String>> = Arc::new(denied_tools);
    let iii_for_handler = iii.clone();
    let function = iii.register_function((
        RegisterFunctionMessage::with_id(FN_DENYLIST.into())
            .with_description("Block tool calls whose name is on a configured denylist.".into()),
        move |payload: Value| {
            let denied = denied.clone();
            let iii = iii_for_handler.clone();
            async move {
                let (event_id, reply_stream, inner) = unwrap_envelope(&payload);
                let tool_name = inner
                    .get("tool_call")
                    .and_then(|tc| tc.get("name"))
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                let reply = if denied.iter().any(|d| d == &tool_name) {
                    json!({
                        "block": true,
                        "reason": format!("policy::denylist blocked '{tool_name}'"),
                    })
                } else {
                    json!({ "block": false })
                };
                write_hook_reply(&iii, &reply_stream, &event_id, &reply).await;
                Ok(reply)
            }
        },
    ));
    let trigger = iii.register_trigger(RegisterTriggerInput {
        trigger_type: "subscribe".into(),
        function_id: FN_DENYLIST.into(),
        config: json!({ "topic": TOPIC_BEFORE }),
        metadata: None,
    })?;
    Ok(Subscriber { function, trigger })
}

/// Register an append-only audit-log subscriber on `agent::after_tool_call`.
///
/// Every observed call writes one JSON object per line to `log_path` with
/// `{ ts_ms, tool_call: {...}, result: {...} }`. The reply is informational
/// (`{ ok: true }`); the runtime's `merge_after` ignores it because no
/// recognised field is present.
pub fn subscribe_audit_log(iii: &III, log_path: PathBuf) -> Result<Subscriber, IIIError> {
    let log_path = Arc::new(log_path);
    let iii_for_handler = iii.clone();
    let function = iii.register_function((
        RegisterFunctionMessage::with_id(FN_AUDIT.into())
            .with_description("Append every tool call + result to a JSON-lines audit log.".into()),
        move |payload: Value| {
            let log_path = log_path.clone();
            let iii = iii_for_handler.clone();
            async move {
                let (event_id, reply_stream, inner) = unwrap_envelope(&payload);
                let line = json!({
                    "ts_ms": chrono::Utc::now().timestamp_millis(),
                    "tool_call": inner.get("tool_call").cloned().unwrap_or(Value::Null),
                    "result": inner.get("result").cloned().unwrap_or(Value::Null),
                });
                let _ = append_jsonl(&log_path, &line).await;
                let reply = json!({ "ok": true });
                write_hook_reply(&iii, &reply_stream, &event_id, &reply).await;
                Ok(reply)
            }
        },
    ));
    let trigger = iii.register_trigger(RegisterTriggerInput {
        trigger_type: "subscribe".into(),
        function_id: FN_AUDIT.into(),
        config: json!({ "topic": TOPIC_AFTER }),
        metadata: None,
    })?;
    Ok(Subscriber { function, trigger })
}

/// Register a DLP scrubber on `agent::after_tool_call` that redacts secrets
/// in the result's text content.
///
/// Patterns matched: AWS access keys (`AKIA[0-9A-Z]{16}`), OpenAI keys
/// (`sk-[A-Za-z0-9]{32,}`), GitHub PATs (`ghp_[A-Za-z0-9]{36}`), Stripe
/// live secrets (`sk_live_[A-Za-z0-9]{24,}`), Google API keys
/// (`AIza[0-9A-Za-z_-]{35}`). Each match is replaced with
/// `[REDACTED:<kind>]`.
///
/// The reply embeds the rewritten content in `{ content: [...] }` so the
/// runtime's `merge_after` overrides the original result.
pub fn subscribe_dlp_scrubber(iii: &III) -> Result<Subscriber, IIIError> {
    let iii_for_handler = iii.clone();
    let function = iii.register_function((
        RegisterFunctionMessage::with_id(FN_DLP.into())
            .with_description("Redact common secret shapes in tool result text content.".into()),
        move |payload: Value| {
            let iii = iii_for_handler.clone();
            async move {
                let (event_id, reply_stream, inner) = unwrap_envelope(&payload);
                let original = inner.get("result").cloned().unwrap_or(Value::Null);
                let scrubbed = scrub_result_value(&original);
                let changed = scrubbed.ne(&original);
                let reply = if changed {
                    json!({ "content": scrubbed.get("content").cloned().unwrap_or(Value::Null) })
                } else {
                    json!({ "ok": true, "scrubbed": false })
                };
                write_hook_reply(&iii, &reply_stream, &event_id, &reply).await;
                Ok(reply)
            }
        },
    ));
    let trigger = iii.register_trigger(RegisterTriggerInput {
        trigger_type: "subscribe".into(),
        function_id: FN_DLP.into(),
        config: json!({ "topic": TOPIC_AFTER }),
        metadata: None,
    })?;
    Ok(Subscriber { function, trigger })
}

fn unwrap_envelope(payload: &Value) -> (String, String, Value) {
    let event_id = payload
        .get("event_id")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let reply_stream = payload
        .get("reply_stream")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let inner = payload
        .get("payload")
        .cloned()
        .unwrap_or_else(|| payload.clone());
    (event_id, reply_stream, inner)
}

async fn write_hook_reply(iii: &III, stream_name: &str, event_id: &str, reply: &Value) {
    if event_id.is_empty() || stream_name.is_empty() {
        return;
    }
    // `item_id` is required by iii v0.11.x stream::set; the engine drops or
    // de-dupes writes that don't carry it, which silently broke the
    // collected pubsub reply path before harness v0.11.6.
    let item_id = uuid::Uuid::new_v4().to_string();
    let _ = iii
        .trigger(TriggerRequest {
            function_id: "stream::set".into(),
            payload: json!({
                "stream_name": stream_name,
                "group_id": event_id,
                "item_id": item_id,
                "data": reply,
            }),
            action: None,
            timeout_ms: None,
        })
        .await;
}

/// Per-path mutex map. POSIX `O_APPEND` is atomic only up to `PIPE_BUF`
/// (4096 bytes on most platforms). Tool results routinely exceed that, so
/// concurrent `after_tool_call` subscribers writing the same audit log can
/// interleave bytes. We serialise writes per path with a process-wide
/// mutex map. Different paths still write concurrently.
fn audit_log_locks() -> &'static std::sync::Mutex<
    std::collections::HashMap<PathBuf, Arc<tokio::sync::Mutex<()>>>,
> {
    static LOCKS: std::sync::OnceLock<
        std::sync::Mutex<std::collections::HashMap<PathBuf, Arc<tokio::sync::Mutex<()>>>>,
    > = std::sync::OnceLock::new();
    LOCKS.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}

async fn append_jsonl(path: &PathBuf, line: &Value) -> std::io::Result<()> {
    use tokio::io::AsyncWriteExt;

    let lock = {
        let mut map = audit_log_locks().lock().expect("audit_log_locks poisoned");
        map.entry(path.clone())
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone()
    };
    let _guard = lock.lock().await;

    if let Some(parent) = path.parent() {
        let _ = tokio::fs::create_dir_all(parent).await;
    }
    let mut f = tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .await?;
    let mut s = serde_json::to_vec(line).unwrap_or_default();
    s.push(b'\n');
    f.write_all(&s).await?;
    Ok(())
}

/// Pure scrubber over the JSON shape `{ content: [{ type, text }, ...] }`.
/// Exposed for unit testing; the live subscriber wraps it.
pub fn scrub_result_value(result: &Value) -> Value {
    let Some(content) = result.get("content").and_then(Value::as_array) else {
        return result.clone();
    };
    let scrubbed: Vec<Value> = content
        .iter()
        .map(|block| {
            let mut block = block.clone();
            if block.get("type").and_then(Value::as_str) == Some("text") {
                if let Some(text) = block.get("text").and_then(Value::as_str) {
                    let redacted = scrub_text(text);
                    if redacted != text {
                        block["text"] = Value::String(redacted);
                    }
                }
            }
            block
        })
        .collect();
    let mut out = result.clone();
    if let Some(obj) = out.as_object_mut() {
        obj.insert("content".into(), Value::Array(scrubbed));
    }
    out
}

/// Pure secret-redaction over a string. Returns the original when no pattern
/// matches, so callers can detect "nothing changed" with equality.
pub fn scrub_text(input: &str) -> String {
    use once_cell::sync::Lazy;
    use regex::Regex;
    static AWS: Lazy<Regex> = Lazy::new(|| Regex::new(r"AKIA[0-9A-Z]{16}").unwrap());
    static OPENAI: Lazy<Regex> = Lazy::new(|| Regex::new(r"sk-[A-Za-z0-9]{32,}").unwrap());
    static GITHUB: Lazy<Regex> = Lazy::new(|| Regex::new(r"ghp_[A-Za-z0-9]{36}").unwrap());
    static STRIPE: Lazy<Regex> = Lazy::new(|| Regex::new(r"sk_live_[A-Za-z0-9]{24,}").unwrap());
    static GOOGLE: Lazy<Regex> = Lazy::new(|| Regex::new(r"AIza[0-9A-Za-z_\-]{35}").unwrap());

    let mut out = AWS.replace_all(input, "[REDACTED:aws]").to_string();
    out = OPENAI.replace_all(&out, "[REDACTED:openai]").to_string();
    out = GITHUB.replace_all(&out, "[REDACTED:github]").to_string();
    out = STRIPE.replace_all(&out, "[REDACTED:stripe]").to_string();
    out = GOOGLE.replace_all(&out, "[REDACTED:google]").to_string();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scrub_text_redacts_aws() {
        let key = format!("AKIA{}", "X".repeat(16));
        let input = format!("found key {key} in log");
        let out = scrub_text(&input);
        assert!(out.contains("[REDACTED:aws]"));
        assert!(!out.contains(&key));
    }

    #[test]
    fn scrub_text_redacts_multiple_kinds() {
        let openai = format!("sk-{}", "0".repeat(40));
        let github = format!("ghp_{}", "1".repeat(36));
        let input = format!("openai={openai} github={github}");
        let out = scrub_text(&input);
        assert!(out.contains("[REDACTED:openai]"));
        assert!(out.contains("[REDACTED:github]"));
    }

    #[test]
    fn scrub_text_passthrough_when_no_secrets() {
        let s = "nothing sensitive here";
        assert_eq!(scrub_text(s), s);
    }

    #[test]
    fn scrub_result_value_rewrites_text_blocks() {
        let aws = format!("AKIA{}", "Z".repeat(16));
        let result = json!({
            "content": [
                { "type": "text", "text": format!("leaked {aws}") },
                { "type": "image", "data": "ignored" },
            ],
            "details": {},
        });
        let out = scrub_result_value(&result);
        let text = out["content"][0]["text"].as_str().expect("text block");
        assert!(text.contains("[REDACTED:aws]"));
        // image block untouched
        assert_eq!(out["content"][1]["data"].as_str(), Some("ignored"));
    }

    #[test]
    fn scrub_result_value_passthrough_when_no_content() {
        let v = json!({ "details": { "exit_code": 0 } });
        assert_eq!(scrub_result_value(&v), v);
    }
}
