//! Replay an agent-session fixture through the harness loop.
//!
//! Usage:
//!   replay <fixture-path> [--write-golden]
//!
//! The fixture is a JSONL of `SessionEntry` records. The loop is driven with a
//! `MemoryRuntime`: assistant entries are queued in order on the faux provider,
//! tool-result entries are handled by an echo tool handler, the first user
//! entry becomes the initial prompt.
//!
//! Without `--write-golden`, the emitted `AgentEvent` stream is compared
//! against the JSON file at `fixtures/golden-events/<basename>.json` and the
//! process exits with a non-zero status on any mismatch.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{anyhow, bail, Context, Result};
use harness_runtime::{run_loop, CapturedEvents, LoopConfig, MemoryRuntime, ToolHandler};
use harness_types::{
    AgentEvent, AgentMessage, AgentTool, AssistantMessage, ContentBlock, ExecutionMode,
    TextContent, ToolCall, ToolResult,
};
use session_tree::SessionEntry;

struct EchoTool;

#[async_trait::async_trait]
impl ToolHandler for EchoTool {
    async fn execute(&self, tool_call: &ToolCall) -> ToolResult {
        let text = tool_call
            .arguments
            .get("text")
            .and_then(|v| v.as_str())
            .map_or_else(|| tool_call.arguments.to_string(), ToString::to_string);
        ToolResult {
            content: vec![ContentBlock::Text(TextContent { text })],
            details: serde_json::json!({}),
            terminate: false,
        }
    }
}

fn read_jsonl(path: &Path) -> Result<Vec<SessionEntry>> {
    let bytes = std::fs::read(path).with_context(|| format!("read {}", path.display()))?;
    let text = String::from_utf8(bytes)?;
    let mut entries = Vec::new();
    for (line_no, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let entry: SessionEntry = serde_json::from_str(line)
            .with_context(|| format!("{}:{}: parse SessionEntry", path.display(), line_no + 1))?;
        entries.push(entry);
    }
    Ok(entries)
}

fn split_inputs(entries: &[SessionEntry]) -> (Vec<AgentMessage>, Vec<AssistantMessage>) {
    let mut initial: Vec<AgentMessage> = Vec::new();
    let mut assistant: Vec<AssistantMessage> = Vec::new();
    for e in entries {
        if let SessionEntry::Message { message, .. } = e {
            match message {
                AgentMessage::User(_) if assistant.is_empty() => initial.push(message.clone()),
                AgentMessage::Assistant(a) => assistant.push(a.clone()),
                _ => {} // tool results recreated by handler; later user messages = steering (not handled here)
            }
        }
    }
    (initial, assistant)
}

fn echo_tool_def() -> AgentTool {
    AgentTool {
        name: "echo".into(),
        description: "echo".into(),
        parameters: serde_json::json!({}),
        label: "echo".into(),
        execution_mode: ExecutionMode::Parallel,
        prepare_arguments_supported: false,
    }
}

/// Zero timestamp fields on a serialized event so replays are deterministic.
fn normalize_timestamps(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(map) => {
            if let Some(ts) = map.get_mut("timestamp") {
                if ts.is_number() {
                    *ts = serde_json::Value::from(0u64);
                }
            }
            for v in map.values_mut() {
                normalize_timestamps(v);
            }
        }
        serde_json::Value::Array(arr) => {
            for v in arr.iter_mut() {
                normalize_timestamps(v);
            }
        }
        _ => {}
    }
}

fn normalize_event_stream(events: &[AgentEvent]) -> Result<Vec<serde_json::Value>> {
    let mut normalized = Vec::with_capacity(events.len());
    for ev in events {
        let mut json = serde_json::to_value(ev)?;
        normalize_timestamps(&mut json);
        normalized.push(json);
    }
    Ok(normalized)
}

async fn replay(fixture: &Path) -> Result<Vec<AgentEvent>> {
    let entries = read_jsonl(fixture)?;
    let (initial, assistants) = split_inputs(&entries);
    if initial.is_empty() {
        bail!(
            "fixture must begin with a user message: {}",
            fixture.display()
        );
    }
    if assistants.is_empty() {
        bail!(
            "fixture must contain at least one assistant message: {}",
            fixture.display()
        );
    }

    let captured = Arc::new(CapturedEvents::new());
    let mut rt = MemoryRuntime::new(captured.clone());
    rt.register_tool("echo", Arc::new(EchoTool));
    for a in assistants {
        rt.queue_assistant(a);
    }

    let mut cfg = LoopConfig::new(format!(
        "replay-{}",
        fixture
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("session")
    ));
    cfg.tools.push(echo_tool_def());

    let _outcome = run_loop(&rt, &*captured, &cfg, initial).await;
    Ok(captured.snapshot())
}

fn golden_path(fixture: &Path) -> PathBuf {
    let stem = fixture
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("fixture");
    let parent_of_agent_sessions = fixture
        .parent()
        .and_then(Path::parent)
        .map_or_else(|| PathBuf::from("."), Path::to_path_buf);
    parent_of_agent_sessions
        .join("golden-events")
        .join(format!("{stem}.json"))
}

fn write_golden(fixture: &Path, events: &[AgentEvent]) -> Result<()> {
    let path = golden_path(fixture);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let normalized = normalize_event_stream(events)?;
    let json = serde_json::to_string_pretty(&normalized)?;
    std::fs::write(&path, json)?;
    eprintln!("wrote golden {} ({} events)", path.display(), events.len());
    Ok(())
}

fn diff_against_golden(fixture: &Path, events: &[AgentEvent]) -> Result<()> {
    let path = golden_path(fixture);
    let bytes = std::fs::read(&path)
        .with_context(|| format!("read golden {} (run with --write-golden)", path.display()))?;
    let expected: Vec<serde_json::Value> = serde_json::from_slice(&bytes)?;
    let observed = normalize_event_stream(events)?;
    if expected.len() != observed.len() {
        bail!(
            "event count differs: expected {} got {}",
            expected.len(),
            observed.len()
        );
    }
    for (i, (a, b)) in expected.iter().zip(observed.iter()).enumerate() {
        if a != b {
            let ja = serde_json::to_string(a)?;
            let jb = serde_json::to_string(b)?;
            bail!("event {i} differs:\n  expected: {ja}\n       got: {jb}");
        }
    }
    eprintln!("ok {} ({} events)", path.display(), observed.len());
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let mut fixture: Option<PathBuf> = None;
    let mut write_mode = false;
    for a in args.iter().skip(1) {
        match a.as_str() {
            "--write-golden" => write_mode = true,
            "--help" | "-h" => {
                eprintln!("Usage: replay <fixture-path> [--write-golden]");
                return Ok(());
            }
            other => fixture = Some(PathBuf::from(other)),
        }
    }
    let fixture = fixture.ok_or_else(|| anyhow!("missing fixture path"))?;

    let events = replay(&fixture).await?;
    if write_mode {
        write_golden(&fixture, &events)
    } else {
        diff_against_golden(&fixture, &events)
    }
}
