//! Synthesize replay fixtures for harness tests.
//!
//! Writes one JSONL per fixture to `fixtures/agent-sessions/`. Each line is a
//! `SessionEntry`. Companion files in `fixtures/golden-events/` carry the
//! expected `AgentEvent` sequences used by replay tests.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::Result;
use harness_types::{
    AgentMessage, AssistantMessage, ContentBlock, StopReason, TextContent, UserMessage,
};
use session_tree::SessionEntry;

fn user(text: &str, ts: i64) -> AgentMessage {
    AgentMessage::User(UserMessage {
        content: vec![ContentBlock::Text(TextContent { text: text.into() })],
        timestamp: ts,
    })
}

fn assistant_text(text: &str, ts: i64) -> AssistantMessage {
    AssistantMessage {
        content: vec![ContentBlock::Text(TextContent { text: text.into() })],
        stop_reason: StopReason::End,
        error_message: None,
        error_kind: None,
        usage: None,
        model: "faux-model".into(),
        provider: "faux".into(),
        timestamp: ts,
    }
}

fn assistant_tool_call(tool: &str, args: serde_json::Value, ts: i64) -> AssistantMessage {
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
        model: "faux-model".into(),
        provider: "faux".into(),
        timestamp: ts,
    }
}

fn entry_msg(id: &str, parent: Option<&str>, m: AgentMessage, ts: i64) -> SessionEntry {
    SessionEntry::Message {
        id: id.into(),
        parent_id: parent.map(ToString::to_string),
        message: m,
        timestamp: ts,
    }
}

fn write_jsonl(path: &Path, entries: &[SessionEntry]) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut buf = String::new();
    for e in entries {
        buf.push_str(&serde_json::to_string(e)?);
        buf.push('\n');
    }
    fs::write(path, buf)?;
    Ok(())
}

fn fixture_tiny_text() -> Vec<SessionEntry> {
    vec![
        entry_msg("u1", None, user("hello", 1), 1),
        entry_msg(
            "a1",
            Some("u1"),
            AgentMessage::Assistant(assistant_text("hi", 2)),
            2,
        ),
    ]
}

fn fixture_tool_batch_parallel() -> Vec<SessionEntry> {
    vec![
        entry_msg("u1", None, user("read /tmp/x and /tmp/y", 1), 1),
        entry_msg(
            "a1",
            Some("u1"),
            AgentMessage::Assistant(assistant_tool_call(
                "echo",
                serde_json::json!({ "text": "x-result" }),
                2,
            )),
            2,
        ),
        entry_msg(
            "t1",
            Some("a1"),
            AgentMessage::ToolResult(harness_types::ToolResultMessage {
                tool_call_id: "call-1".into(),
                tool_name: "echo".into(),
                content: vec![ContentBlock::Text(TextContent {
                    text: "x-result".into(),
                })],
                details: serde_json::json!({}),
                is_error: false,
                timestamp: 3,
            }),
            3,
        ),
        entry_msg(
            "a2",
            Some("t1"),
            AgentMessage::Assistant(assistant_text("done", 4)),
            4,
        ),
    ]
}

fn fixture_steering_mid_run() -> Vec<SessionEntry> {
    vec![
        entry_msg("u1", None, user("first", 1), 1),
        entry_msg(
            "a1",
            Some("u1"),
            AgentMessage::Assistant(assistant_tool_call(
                "echo",
                serde_json::json!({ "text": "ack" }),
                2,
            )),
            2,
        ),
        entry_msg(
            "t1",
            Some("a1"),
            AgentMessage::ToolResult(harness_types::ToolResultMessage {
                tool_call_id: "call-1".into(),
                tool_name: "echo".into(),
                content: vec![ContentBlock::Text(TextContent { text: "ack".into() })],
                details: serde_json::json!({}),
                is_error: false,
                timestamp: 3,
            }),
            3,
        ),
        entry_msg("u2", Some("t1"), user("steered mid-run", 4), 4),
        entry_msg(
            "a2",
            Some("u2"),
            AgentMessage::Assistant(assistant_text("after-steer", 5)),
            5,
        ),
    ]
}

fn root() -> PathBuf {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest
        .ancestors()
        .nth(2)
        .map(Path::to_path_buf)
        .unwrap_or(manifest)
}

fn main() -> Result<()> {
    let base = root();
    let agent_sessions = base.join("fixtures/agent-sessions");

    let cases: Vec<(&str, Vec<SessionEntry>)> = vec![
        ("tiny-text.jsonl", fixture_tiny_text()),
        ("tool-batch-parallel.jsonl", fixture_tool_batch_parallel()),
        ("steering-mid-run.jsonl", fixture_steering_mid_run()),
    ];

    for (name, entries) in &cases {
        let path = agent_sessions.join(name);
        write_jsonl(&path, entries)?;
        eprintln!("wrote {} ({} entries)", path.display(), entries.len());
    }

    Ok(())
}
