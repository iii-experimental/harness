//! Pure merge logic for the three hook pubsub topics.
//!
//! Subscriber responses for `agent::before_tool_call`, `agent::after_tool_call`
//! and `agent::transform_context` flow back through the bus as JSON values
//! written to the `agent::hook_reply/<event_id>` stream by each subscriber.
//! These functions compose the final result without any I/O, so they're
//! easy to test in isolation.

use crate::runtime::HookOutcome;
use harness_types::{AgentMessage, ToolResult};
use serde_json::Value;

/// Pick the first response that sets `block: true`. If none do, return the
/// default (no-block) outcome.
///
/// Each response is expected to look like:
///
/// ```json
/// { "block": true, "reason": "policy violation" }
/// ```
///
/// Responses missing `block` or with `block: false` are ignored.
pub fn merge_before(responses: &[Value]) -> HookOutcome {
    for r in responses {
        if r.get("block").and_then(Value::as_bool).unwrap_or(false) {
            return HookOutcome {
                block: true,
                reason: r
                    .get("reason")
                    .and_then(Value::as_str)
                    .map(ToString::to_string),
            };
        }
    }
    HookOutcome::default()
}

/// Field-by-field merge of subscriber responses, applied in order on top of
/// the original tool result. Recognised fields:
///
/// - `content` (Vec<ContentBlock>): replaces `result.content` outright.
/// - `details` (object): shallow-merged into `result.details`.
/// - `terminate` (bool): replaces `result.terminate`.
///
/// Any unrecognised fields are ignored so subscribers can include diagnostic
/// metadata without breaking the merge.
pub fn merge_after(mut result: ToolResult, responses: &[Value]) -> ToolResult {
    for r in responses {
        if let Some(content) = r.get("content") {
            if let Ok(blocks) = serde_json::from_value(content.clone()) {
                result.content = blocks;
            }
        }
        if let Some(details) = r.get("details") {
            if let (Some(existing), Some(incoming)) =
                (result.details.as_object_mut(), details.as_object())
            {
                for (k, v) in incoming {
                    existing.insert(k.clone(), v.clone());
                }
            } else if details.is_object() {
                result.details = details.clone();
            }
        }
        if let Some(t) = r.get("terminate").and_then(Value::as_bool) {
            result.terminate = t;
        }
    }
    result
}

/// Decode a transform-context subscriber response into a `Vec<AgentMessage>`.
/// Tolerates both bare arrays and `{ "messages": [...] }` envelopes.
pub fn decode_transform(response: &Value) -> Option<Vec<AgentMessage>> {
    if response.is_array() {
        return serde_json::from_value(response.clone()).ok();
    }
    if let Some(arr) = response.get("messages") {
        return serde_json::from_value(arr.clone()).ok();
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use harness_types::{ContentBlock, TextContent};
    use serde_json::json;

    fn empty_result() -> ToolResult {
        ToolResult {
            content: vec![ContentBlock::Text(TextContent { text: "ok".into() })],
            details: json!({}),
            terminate: false,
        }
    }

    #[test]
    fn before_first_block_wins() {
        let responses = vec![
            json!({}),
            json!({ "block": false }),
            json!({ "block": true, "reason": "first-blocker" }),
            json!({ "block": true, "reason": "second-blocker" }),
        ];
        let outcome = merge_before(&responses);
        assert!(outcome.block);
        assert_eq!(outcome.reason.as_deref(), Some("first-blocker"));
    }

    #[test]
    fn before_no_blockers_yields_default() {
        let responses = vec![json!({}), json!({ "block": false })];
        let outcome = merge_before(&responses);
        assert!(!outcome.block);
        assert!(outcome.reason.is_none());
    }

    #[test]
    fn after_replaces_content() {
        let result = empty_result();
        let responses = vec![json!({
            "content": [{ "type": "text", "text": "rewritten" }]
        })];
        let merged = merge_after(result, &responses);
        match merged.content.first() {
            Some(ContentBlock::Text(t)) => assert_eq!(t.text, "rewritten"),
            _ => panic!("expected rewritten text block"),
        }
    }

    #[test]
    fn after_shallow_merges_details() {
        let mut r = empty_result();
        r.details = json!({ "a": 1, "b": 2 });
        let responses = vec![json!({ "details": { "b": 99, "c": 3 } })];
        let merged = merge_after(r, &responses);
        assert_eq!(merged.details["a"], 1);
        assert_eq!(merged.details["b"], 99);
        assert_eq!(merged.details["c"], 3);
    }

    #[test]
    fn after_terminate_last_wins() {
        let r = empty_result();
        let responses = vec![json!({ "terminate": false }), json!({ "terminate": true })];
        let merged = merge_after(r, &responses);
        assert!(merged.terminate);
    }

    #[test]
    fn decode_transform_accepts_bare_array() {
        let v = json!([
            { "role": "user", "content": [], "timestamp": 0 }
        ]);
        let decoded = decode_transform(&v).expect("decoded");
        assert_eq!(decoded.len(), 1);
    }

    #[test]
    fn decode_transform_accepts_wrapped() {
        let v = json!({
            "messages": [
                { "role": "user", "content": [], "timestamp": 0 }
            ]
        });
        let decoded = decode_transform(&v).expect("decoded");
        assert_eq!(decoded.len(), 1);
    }

    #[test]
    fn decode_transform_returns_none_for_garbage() {
        let v = json!({ "wat": "" });
        assert!(decode_transform(&v).is_none());
    }
}
