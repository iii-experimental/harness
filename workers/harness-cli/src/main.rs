//! Thin invoker over the iii bus. Connects to a running iii-engine,
//! registers the harness loop and the configured provider as iii functions,
//! then triggers `agent::run_loop` and prints events from the per-session
//! `agent::events/<session_id>` stream.
//!
//! Usage:
//!   harness [--provider <name>] [--model <id>] [--max-turns <n>]
//!           [--engine-url <url>] "<prompt>"
//!
//! The CLI no longer hosts the loop. Loop, tools, and provider all live as
//! iii functions on the engine bus. Provider crates ship a
//! `register_with_iii(&iii)` of their own; the CLI calls the matching one
//! after [`harness_runtime::register_with_iii`] so that
//! `agent::stream_assistant` has a target to dispatch to.

use std::sync::Arc;

use anyhow::{Context, Result};
use harness_types::{
    AgentEvent, AgentMessage, AgentTool, ContentBlock, ExecutionMode, StopReason, TextContent,
    UserMessage,
};
use iii_sdk::{register_worker, InitOptions, TriggerRequest, III};
use serde_json::{json, Value};

const DEFAULT_PROVIDER: &str = "anthropic";
const DEFAULT_SYSTEM_PROMPT: &str = "You are a coding assistant running on the iii harness. Use the tools provided to inspect and modify files. Keep responses focused and concrete.";
const DEFAULT_ENGINE_URL: &str = "ws://127.0.0.1:49134";

#[derive(Debug, Clone)]
struct CliArgs {
    prompt: String,
    provider: String,
    model: Option<String>,
    max_turns: usize,
    engine_url: String,
}

fn parse_args_from(raw: &[String]) -> Result<CliArgs> {
    let mut prompt: Option<String> = None;
    let mut provider = DEFAULT_PROVIDER.to_string();
    let mut model: Option<String> = None;
    let mut max_turns = 10usize;
    let mut engine_url = DEFAULT_ENGINE_URL.to_string();
    let mut i = 0;
    while i < raw.len() {
        match raw[i].as_str() {
            "--provider" => {
                provider = raw
                    .get(i + 1)
                    .cloned()
                    .ok_or_else(|| anyhow::anyhow!("--provider requires a value"))?;
                i += 2;
            }
            "--model" => {
                model = Some(
                    raw.get(i + 1)
                        .cloned()
                        .ok_or_else(|| anyhow::anyhow!("--model requires a value"))?,
                );
                i += 2;
            }
            "--max-turns" => {
                max_turns = raw
                    .get(i + 1)
                    .and_then(|s| s.parse().ok())
                    .ok_or_else(|| anyhow::anyhow!("--max-turns requires a number"))?;
                i += 2;
            }
            "--engine-url" => {
                engine_url = raw
                    .get(i + 1)
                    .cloned()
                    .ok_or_else(|| anyhow::anyhow!("--engine-url requires a value"))?;
                i += 2;
            }
            "--help" | "-h" => {
                print_help();
                std::process::exit(0);
            }
            other if other.starts_with("--") => {
                anyhow::bail!("unknown flag '{other}'. Run with --help for the supported list.");
            }
            other => {
                if prompt.is_none() {
                    prompt = Some(other.to_string());
                } else if let Some(p) = prompt.as_mut() {
                    p.push(' ');
                    p.push_str(other);
                }
                i += 1;
            }
        }
    }
    let prompt = prompt.ok_or_else(|| anyhow::anyhow!("missing prompt; pass as positional arg"))?;
    Ok(CliArgs {
        prompt,
        provider,
        model,
        max_turns,
        engine_url,
    })
}

fn parse_args() -> Result<CliArgs> {
    let raw: Vec<String> = std::env::args().skip(1).collect();
    parse_args_from(&raw)
}

fn print_help() {
    println!("Usage: harness [options] <prompt>");
    println!();
    println!("The CLI connects to a running iii-engine, registers the harness loop");
    println!("and the configured provider as iii functions, then triggers");
    println!("agent::run_loop. Loop, tools, and provider all run on the bus.");
    println!();
    println!("Options:");
    println!("  --provider <name>      provider crate to register (default: {DEFAULT_PROVIDER})");
    println!("  --model <id>           provider model id (default depends on --provider)");
    println!("  --max-turns <n>        stop after n turns (default: 10)");
    println!("  --engine-url <url>     iii-engine WebSocket URL (default: {DEFAULT_ENGINE_URL})");
    println!();
    println!("tool::bash discovery:");
    println!("  the engine's iii-sandbox worker is detected at runtime via");
    println!("  iii.list_functions(). When sandbox::exec is registered the bash");
    println!("  tool routes commands through it; otherwise it falls back to a");
    println!("  host-process bash. There is no flag to override this.");
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = parse_args()?;
    let model = resolve_model(&args)?;

    let iii = connect_iii(&args.engine_url).await?;
    let iii = Arc::new(iii);

    harness_runtime::register_with_iii(iii.as_ref())
        .await
        .context("failed to register harness-runtime functions on iii engine")?;

    register_provider(iii.as_ref(), &args.provider)
        .await
        .with_context(|| format!("failed to register provider '{}'", args.provider))?;

    let session_id = format!("cli-{}", chrono::Utc::now().timestamp_millis());
    let tools = builtin_tool_defs();

    let initial = vec![AgentMessage::User(UserMessage {
        content: vec![ContentBlock::Text(TextContent {
            text: args.prompt.clone(),
        })],
        timestamp: chrono::Utc::now().timestamp_millis(),
    })];

    let payload = json!({
        "session_id": session_id,
        "messages": initial,
        "tools": tools,
        "provider": args.provider,
        "model": model,
        "system_prompt": DEFAULT_SYSTEM_PROMPT,
        "max_turns": args.max_turns,
    });

    let printer = tokio::spawn(stream_events(iii.clone(), session_id.clone()));

    // iii-sdk defaults `None` to a 30s timeout (DEFAULT_TIMEOUT_MS in iii.rs).
    // agent::run_loop drives a multi-turn LLM + tool loop and routinely runs
    // longer than that; without an explicit cap it dies mid-turn with
    // "invocation timed out" while the engine is still healthy. 10 minutes is
    // generous enough for tool-heavy turns but still bounds runaway loops.
    let response = iii
        .trigger(TriggerRequest {
            function_id: "agent::run_loop".to_string(),
            payload,
            action: None,
            timeout_ms: Some(600_000),
        })
        .await
        .context("agent::run_loop failed")?;

    printer.abort();

    let messages: Vec<AgentMessage> = response
        .get("messages")
        .cloned()
        .map(serde_json::from_value)
        .transpose()
        .ok()
        .flatten()
        .unwrap_or_default();

    eprintln!("\n=== summary ===");
    eprintln!("turns: {}", count_turns(&messages));
    eprintln!("messages: {}", messages.len());
    if let Some(AgentMessage::Assistant(a)) = messages.iter().rev().find(|m| matches!(m, AgentMessage::Assistant(_))) {
        eprintln!("last_stop_reason: {:?}", a.stop_reason);
        for block in &a.content {
            if let ContentBlock::Text(t) = block {
                eprintln!("last_text: {}", t.text);
            }
        }
    }

    Ok(())
}

/// Connect to the iii-engine over WebSocket and verify the connection by
/// issuing a `list_functions` round-trip. Exits with code 2 on failure
/// after writing a remediation hint to stderr.
async fn connect_iii(engine_url: &str) -> Result<III> {
    let iii = register_worker(engine_url, InitOptions::default());
    let probe = tokio::time::timeout(std::time::Duration::from_secs(5), iii.list_functions()).await;
    match probe {
        Ok(Ok(_)) => Ok(iii),
        Ok(Err(e)) => {
            eprintln!(
                "failed to connect to iii engine at {engine_url}: {e}\nstart the engine, then retry. Override with --engine-url."
            );
            std::process::exit(2);
        }
        Err(_) => {
            eprintln!(
                "timed out connecting to iii engine at {engine_url}\nstart the engine, then retry. Override with --engine-url."
            );
            std::process::exit(2);
        }
    }
}

/// Subscribe to the per-session event stream and pretty-print events as
/// they arrive. Best-effort: if the engine doesn't expose `stream::list`
/// or the subscription fails, the loop still runs — the printer simply
/// produces no output.
async fn stream_events(iii: Arc<III>, session_id: String) {
    loop {
        let resp = iii
            .trigger(TriggerRequest {
                function_id: "stream::list".to_string(),
                payload: json!({
                    "stream_name": harness_runtime::EVENTS_STREAM,
                    "group_id": session_id,
                }),
                action: None,
                timeout_ms: None,
            })
            .await;
        if let Ok(value) = resp {
            if let Some(items) = value.get("items").and_then(Value::as_array) {
                for item in items {
                    if let Some(data) = item.get("data") {
                        print_event(data);
                    }
                }
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
}

fn print_event(value: &Value) {
    let Ok(event) = serde_json::from_value::<AgentEvent>(value.clone()) else {
        return;
    };
    match event {
        AgentEvent::AgentStart => println!("[agent_start]"),
        AgentEvent::AgentEnd { messages } => {
            println!("[agent_end] {} messages total", messages.len());
        }
        AgentEvent::TurnStart => println!("\n[turn_start]"),
        AgentEvent::TurnEnd { tool_results, .. } => {
            if tool_results.is_empty() {
                println!("[turn_end]");
            } else {
                println!("[turn_end] {} tool result(s)", tool_results.len());
            }
        }
        AgentEvent::MessageStart { message } => match &message {
            AgentMessage::User(u) => {
                if let Some(ContentBlock::Text(t)) = u.content.first() {
                    println!(">>> user: {}", t.text);
                }
            }
            AgentMessage::Assistant(a) => {
                let text: String = a
                    .content
                    .iter()
                    .filter_map(|c| match c {
                        ContentBlock::Text(t) => Some(t.text.clone()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                if !text.is_empty() {
                    println!("<<< assistant: {text}");
                }
                for tc in a.content.iter().filter_map(|c| match c {
                    ContentBlock::ToolCall {
                        name, arguments, ..
                    } => Some(format!("{name}({arguments})")),
                    _ => None,
                }) {
                    println!("    -> tool call: {tc}");
                }
            }
            _ => {}
        },
        AgentEvent::ToolExecutionEnd {
            tool_name,
            is_error,
            result,
            ..
        } => {
            let preview = result
                .content
                .iter()
                .find_map(|c| match c {
                    ContentBlock::Text(t) => Some(t.text.clone()),
                    _ => None,
                })
                .unwrap_or_default();
            let preview: String = preview.chars().take(160).collect();
            println!(
                "    [{tool_name} result {}] {preview}",
                if is_error { "error" } else { "ok" }
            );
        }
        _ => {}
    }
}

/// Resolve the model id from CLI args, environment, or per-provider default.
fn resolve_model(args: &CliArgs) -> Result<String> {
    if let Some(m) = args.model.clone() {
        return Ok(m);
    }
    default_model(&args.provider)
        .map(ToString::to_string)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "no default model for provider '{}' — pass --model",
                args.provider
            )
        })
}

fn default_model(provider: &str) -> Option<&'static str> {
    match provider {
        "anthropic" => Some("claude-sonnet-4-6"),
        "openai" | "openai-responses" | "vercel-ai-gateway" => Some("gpt-5"),
        "google" | "google-vertex" => Some("gemini-2-5-flash"),
        "bedrock" => Some("anthropic.claude-sonnet-4-6"),
        "openrouter" => Some("anthropic/claude-sonnet-4"),
        "groq" => Some("llama-3.3-70b-versatile"),
        "cerebras" => Some("llama-3.3-70b"),
        "xai" => Some("grok-4"),
        "deepseek" => Some("deepseek-chat"),
        "mistral" => Some("mistral-large-latest"),
        "fireworks" => Some("accounts/fireworks/models/llama-v3p3-70b-instruct"),
        "kimi-coding" => Some("moonshot-v1-32k"),
        "minimax" => Some("abab6.5s-chat"),
        "zai" => Some("glm-4-plus"),
        "huggingface" => Some("meta-llama/Llama-3.3-70B-Instruct"),
        "opencode-zen" | "opencode-go" => Some("claude-sonnet-4-6"),
        _ => None,
    }
}

/// Dispatch to the configured provider's `register_with_iii`. Each provider
/// crate ships its own copy of this function; we pick at runtime by name.
async fn register_provider(iii: &III, provider: &str) -> Result<()> {
    match provider {
        "anthropic" => provider_anthropic::register_with_iii(iii).await,
        "openai" => provider_openai::register_with_iii(iii).await,
        "openai-responses" => provider_openai_responses::register_with_iii(iii).await,
        "google" => provider_google::register_with_iii(iii).await,
        "bedrock" => provider_bedrock::register_with_iii(iii).await,
        "openrouter" => provider_openrouter::register_with_iii(iii).await,
        "groq" => provider_groq::register_with_iii(iii).await,
        "cerebras" => provider_cerebras::register_with_iii(iii).await,
        "xai" => provider_xai::register_with_iii(iii).await,
        "deepseek" => provider_deepseek::register_with_iii(iii).await,
        "mistral" => provider_mistral::register_with_iii(iii).await,
        "fireworks" => provider_fireworks::register_with_iii(iii).await,
        "kimi-coding" => provider_kimi_coding::register_with_iii(iii).await,
        "minimax" => provider_minimax::register_with_iii(iii).await,
        "zai" => provider_zai::register_with_iii(iii).await,
        "huggingface" => provider_huggingface::register_with_iii(iii).await,
        "vercel-ai-gateway" => provider_vercel_ai_gateway::register_with_iii(iii).await,
        "opencode-zen" => provider_opencode_zen::register_with_iii(iii).await,
        "opencode-go" => provider_opencode_go::register_with_iii(iii).await,
        "azure-openai" => provider_azure_openai::register_with_iii(iii).await,
        "google-vertex" => provider_google_vertex::register_with_iii(iii).await,
        "faux" => {
            // Zero-config smoke-test path: register a canned response keyed
            // on a fixed model id so `harness --provider faux --model echo`
            // round-trips without any API key. Real test harnesses install
            // their own canned responses via `provider_faux::register_canned`
            // before invocation; this default only fires when nothing else
            // has registered the key.
            provider_faux::register_canned(
                "echo",
                provider_faux::text_only(
                    "hello from faux — this is the harness zero-config smoke path.",
                    "echo",
                    "faux",
                    chrono::Utc::now().timestamp_millis(),
                ),
            );
            provider_faux::register_with_iii(iii).await
        }
        other => anyhow::bail!("unknown provider '{other}'"),
    }
}

fn builtin_tool_defs() -> Vec<AgentTool> {
    vec![
        tool_def(
            "read",
            "Read a file from disk and return its contents.",
            json!({
                "type": "object",
                "properties": { "path": { "type": "string" } },
                "required": ["path"]
            }),
            ExecutionMode::Parallel,
        ),
        tool_def(
            "write",
            "Write content to a file, creating parent directories as needed.",
            json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "content": { "type": "string" }
                },
                "required": ["path", "content"]
            }),
            ExecutionMode::Parallel,
        ),
        tool_def(
            "edit",
            "Replace one occurrence of old_string with new_string in a file.",
            json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "old_string": { "type": "string" },
                    "new_string": { "type": "string" }
                },
                "required": ["path", "old_string", "new_string"]
            }),
            ExecutionMode::Parallel,
        ),
        tool_def(
            "ls",
            "List entries in a directory.",
            json!({
                "type": "object",
                "properties": { "path": { "type": "string" } },
                "required": ["path"]
            }),
            ExecutionMode::Parallel,
        ),
        tool_def(
            "grep",
            "Recursively search files under root for substrings matching pattern.",
            json!({
                "type": "object",
                "properties": {
                    "root": { "type": "string" },
                    "pattern": { "type": "string" }
                },
                "required": ["root", "pattern"]
            }),
            ExecutionMode::Parallel,
        ),
        tool_def(
            "find",
            "Recursively find files under root whose path ends with suffix.",
            json!({
                "type": "object",
                "properties": {
                    "root": { "type": "string" },
                    "suffix": { "type": "string" }
                },
                "required": ["root", "suffix"]
            }),
            ExecutionMode::Parallel,
        ),
        tool_def(
            "bash",
            "Run a bash command. Routes to sandbox::exec when available, host bash otherwise.",
            json!({
                "type": "object",
                "properties": { "command": { "type": "string" } },
                "required": ["command"]
            }),
            ExecutionMode::Sequential,
        ),
    ]
}

fn tool_def(
    name: &str,
    description: &str,
    parameters: Value,
    execution_mode: ExecutionMode,
) -> AgentTool {
    AgentTool {
        name: name.into(),
        description: description.into(),
        parameters,
        label: name.into(),
        execution_mode,
        prepare_arguments_supported: false,
    }
}

fn count_turns(messages: &[AgentMessage]) -> usize {
    messages
        .iter()
        .filter(|m| matches!(m, AgentMessage::Assistant(a) if !matches!(a.stop_reason, StopReason::Aborted | StopReason::Error)))
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn parse_args_default_provider_is_anthropic() {
        let parsed = parse_args_from(&args(&["hello"])).expect("parse ok");
        assert_eq!(parsed.provider, "anthropic");
        assert_eq!(parsed.prompt, "hello");
        assert!(parsed.model.is_none());
    }

    #[test]
    fn parse_args_accepts_provider_flag() {
        let parsed = parse_args_from(&args(&["--provider", "openai", "hi"])).expect("parse ok");
        assert_eq!(parsed.provider, "openai");
        assert_eq!(parsed.prompt, "hi");
    }

    #[test]
    fn parse_args_threads_through_model() {
        let parsed = parse_args_from(&args(&[
            "--provider",
            "google",
            "--model",
            "gemini-1.5-pro",
            "hello",
        ]))
        .expect("parse ok");
        assert_eq!(parsed.provider, "google");
        assert_eq!(parsed.model.as_deref(), Some("gemini-1.5-pro"));
    }

    #[test]
    fn parse_args_default_engine_url() {
        let parsed = parse_args_from(&args(&["hi"])).expect("parse ok");
        assert_eq!(parsed.engine_url, "ws://127.0.0.1:49134");
    }

    #[test]
    fn parse_args_threads_engine_url() {
        let parsed =
            parse_args_from(&args(&["--engine-url", "ws://10.0.0.5:49134", "go"])).expect("parse");
        assert_eq!(parsed.engine_url, "ws://10.0.0.5:49134");
        assert_eq!(parsed.prompt, "go");
    }

    #[test]
    fn parse_args_rejects_unknown_flag() {
        let err = parse_args_from(&args(&["--made-up", "hi"]));
        assert!(err.is_err());
        let msg = err.err().unwrap().to_string();
        assert!(msg.contains("unknown flag"), "msg: {msg}");
    }
}
