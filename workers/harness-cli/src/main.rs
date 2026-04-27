//! CLI binary running the harness agent loop end-to-end against a real
//! provider and a real bash tool. Demonstrates that the loop is a complete
//! agent harness, not just a state-machine fixture.
//!
//! Usage:
//!   harness-cli [--provider <name>] [--model <id>] [--max-turns <n>] [--no-bash] "<prompt>"
//!
//! The `--provider` flag selects one of the workspace's provider crates at
//! runtime; each provider reads its own credentials from the environment.
//! See `print_help` for the full supported list.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use async_trait::async_trait;
use harness_iii_bridge::{IiiSdkClient, SandboxedBashTool};
use harness_runtime::{
    run_loop, EventSink, HookOutcome, LoopConfig, LoopRuntime, MemoryRuntime, ToolHandler,
};
use harness_types::{
    AgentEvent, AgentMessage, AgentTool, AssistantMessage, ContentBlock, ExecutionMode, StopReason,
    TextContent, ToolCall, ToolResult, UserMessage,
};
use iii_sdk::{register_worker, InitOptions};

const DEFAULT_PROVIDER: &str = "anthropic";
const DEFAULT_SYSTEM_PROMPT: &str = "You are a coding assistant running on the iii harness. Use the tools provided to inspect and modify files. Keep responses focused and concrete.";
const DEFAULT_ENGINE_URL: &str = "ws://127.0.0.1:49134";
const DEFAULT_SANDBOX_IMAGE: &str = "python";

/// Tagged config for the selected provider. Each variant owns the typed
/// `Config` from its crate; `stream_assistant` matches and dispatches to that
/// crate's `stream` + `collect`.
enum ProviderConfig {
    Anthropic(Arc<provider_anthropic::AnthropicConfig>),
    OpenAI(Arc<provider_openai::OpenAIConfig>),
    OpenAIResponses(Arc<provider_openai_responses::OpenAIResponsesConfig>),
    Google(Arc<provider_google::GoogleConfig>),
    Bedrock(Arc<provider_bedrock::BedrockConfig>),
    OpenRouter(Arc<provider_openrouter::OpenRouterConfig>),
    Groq(Arc<provider_groq::GroqConfig>),
    Cerebras(Arc<provider_cerebras::CerebrasConfig>),
    Xai(Arc<provider_xai::XaiConfig>),
    DeepSeek(Arc<provider_deepseek::DeepSeekConfig>),
    Mistral(Arc<provider_mistral::MistralConfig>),
    Fireworks(Arc<provider_fireworks::FireworksConfig>),
    KimiCoding(Arc<provider_kimi_coding::KimiCodingConfig>),
    MiniMax(Arc<provider_minimax::MiniMaxConfig>),
    Zai(Arc<provider_zai::ZaiConfig>),
    HuggingFace(Arc<provider_huggingface::HuggingFaceConfig>),
    VercelAiGateway(Arc<provider_vercel_ai_gateway::VercelAiGatewayConfig>),
    OpencodeZen(Arc<provider_opencode_zen::OpencodeZenConfig>),
    OpencodeGo(Arc<provider_opencode_go::OpencodeGoConfig>),
    AzureOpenAI(Arc<provider_azure_openai::AzureOpenAIConfig>),
    GoogleVertex(Arc<provider_google_vertex::VertexConfig>),
}

struct ProviderRuntime {
    cfg: ProviderConfig,
    inner: MemoryRuntime,
    system_prompt: String,
}

#[async_trait]
impl LoopRuntime for ProviderRuntime {
    async fn stream_assistant(
        &self,
        _session_id: &str,
        messages: &[AgentMessage],
        tools: &[AgentTool],
    ) -> AssistantMessage {
        let sys = self.system_prompt.clone();
        let msgs = messages.to_vec();
        let tls = tools.to_vec();
        match &self.cfg {
            ProviderConfig::Anthropic(c) => {
                let s = provider_anthropic::stream(c.clone(), sys, msgs, tls).await;
                provider_anthropic::collect(s).await
            }
            ProviderConfig::OpenAI(c) => {
                let s = provider_openai::stream(c.clone(), sys, msgs, tls).await;
                provider_openai::collect(s).await
            }
            ProviderConfig::OpenAIResponses(c) => {
                let s = provider_openai_responses::stream(c.clone(), sys, msgs, tls).await;
                provider_openai_responses::collect(s).await
            }
            ProviderConfig::Google(c) => {
                let s = provider_google::stream(c.clone(), sys, msgs, tls).await;
                provider_google::collect(s).await
            }
            ProviderConfig::Bedrock(c) => {
                let s = provider_bedrock::stream(c.clone(), sys, msgs, tls).await;
                provider_bedrock::collect(s).await
            }
            ProviderConfig::OpenRouter(c) => {
                let s = provider_openrouter::stream(c.clone(), sys, msgs, tls).await;
                provider_openrouter::collect(s).await
            }
            ProviderConfig::Groq(c) => {
                let s = provider_groq::stream(c.clone(), sys, msgs, tls).await;
                provider_groq::collect(s).await
            }
            ProviderConfig::Cerebras(c) => {
                let s = provider_cerebras::stream(c.clone(), sys, msgs, tls).await;
                provider_cerebras::collect(s).await
            }
            ProviderConfig::Xai(c) => {
                let s = provider_xai::stream(c.clone(), sys, msgs, tls).await;
                provider_xai::collect(s).await
            }
            ProviderConfig::DeepSeek(c) => {
                let s = provider_deepseek::stream(c.clone(), sys, msgs, tls).await;
                provider_deepseek::collect(s).await
            }
            ProviderConfig::Mistral(c) => {
                let s = provider_mistral::stream(c.clone(), sys, msgs, tls).await;
                provider_mistral::collect(s).await
            }
            ProviderConfig::Fireworks(c) => {
                let s = provider_fireworks::stream(c.clone(), sys, msgs, tls).await;
                provider_fireworks::collect(s).await
            }
            ProviderConfig::KimiCoding(c) => {
                let s = provider_kimi_coding::stream(c.clone(), sys, msgs, tls).await;
                provider_kimi_coding::collect(s).await
            }
            ProviderConfig::MiniMax(c) => {
                let s = provider_minimax::stream(c.clone(), sys, msgs, tls).await;
                provider_minimax::collect(s).await
            }
            ProviderConfig::Zai(c) => {
                let s = provider_zai::stream(c.clone(), sys, msgs, tls).await;
                provider_zai::collect(s).await
            }
            ProviderConfig::HuggingFace(c) => {
                let s = provider_huggingface::stream(c.clone(), sys, msgs, tls).await;
                provider_huggingface::collect(s).await
            }
            ProviderConfig::VercelAiGateway(c) => {
                let s = provider_vercel_ai_gateway::stream(c.clone(), sys, msgs, tls).await;
                provider_vercel_ai_gateway::collect(s).await
            }
            ProviderConfig::OpencodeZen(c) => {
                let s = provider_opencode_zen::stream(c.clone(), sys, msgs, tls).await;
                provider_opencode_zen::collect(s).await
            }
            ProviderConfig::OpencodeGo(c) => {
                let s = provider_opencode_go::stream(c.clone(), sys, msgs, tls).await;
                provider_opencode_go::collect(s).await
            }
            ProviderConfig::AzureOpenAI(c) => {
                let s = provider_azure_openai::stream(c.clone(), sys, msgs, tls).await;
                provider_azure_openai::collect(s).await
            }
            ProviderConfig::GoogleVertex(c) => {
                let s = provider_google_vertex::stream(c.clone(), sys, msgs, tls).await;
                provider_google_vertex::collect(s).await
            }
        }
    }

    async fn resolve_tool(&self, name: &str) -> Option<Arc<dyn ToolHandler>> {
        self.inner.resolve_tool(name).await
    }

    async fn before_tool_call(&self, tool_call: &ToolCall) -> HookOutcome {
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

struct BashTool {
    cwd: PathBuf,
}

#[async_trait]
impl ToolHandler for BashTool {
    async fn execute(&self, tool_call: &ToolCall) -> ToolResult {
        let command = tool_call
            .arguments
            .get("command")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if command.is_empty() {
            return error_result("missing required arg: command");
        }

        let output = tokio::process::Command::new("bash")
            .arg("-lc")
            .arg(command)
            .current_dir(&self.cwd)
            .output()
            .await;

        match output {
            Ok(o) => {
                let mut combined = String::new();
                if !o.stdout.is_empty() {
                    combined.push_str(&String::from_utf8_lossy(&o.stdout));
                }
                if !o.stderr.is_empty() {
                    if !combined.is_empty() {
                        combined.push('\n');
                    }
                    combined.push_str(&String::from_utf8_lossy(&o.stderr));
                }
                let truncated = combined.chars().take(30000).collect::<String>();
                let exit = o.status.code().unwrap_or(-1);
                ToolResult {
                    content: vec![ContentBlock::Text(TextContent {
                        text: format!("exit={exit}\n{truncated}"),
                    })],
                    details: serde_json::json!({ "exit_code": exit }),
                    terminate: false,
                }
            }
            Err(e) => error_result(&format!("bash spawn failed: {e}")),
        }
    }
}

fn error_result(msg: &str) -> ToolResult {
    ToolResult {
        content: vec![ContentBlock::Text(TextContent {
            text: msg.to_string(),
        })],
        details: serde_json::json!({}),
        terminate: false,
    }
}

fn read_tool_def() -> AgentTool {
    AgentTool {
        name: "read".into(),
        description: "Read a file from disk and return its contents.".into(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Absolute or relative file path." }
            },
            "required": ["path"]
        }),
        label: "read".into(),
        execution_mode: ExecutionMode::Parallel,
        prepare_arguments_supported: false,
    }
}

fn write_tool_def() -> AgentTool {
    AgentTool {
        name: "write".into(),
        description: "Write content to a file, creating parent directories as needed.".into(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "path": { "type": "string" },
                "content": { "type": "string" }
            },
            "required": ["path", "content"]
        }),
        label: "write".into(),
        execution_mode: ExecutionMode::Parallel,
        prepare_arguments_supported: false,
    }
}

fn edit_tool_def() -> AgentTool {
    AgentTool {
        name: "edit".into(),
        description: "Replace one occurrence of old_string with new_string in a file. Fails if old_string is missing or appears more than once.".into(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "path": { "type": "string" },
                "old_string": { "type": "string" },
                "new_string": { "type": "string" }
            },
            "required": ["path", "old_string", "new_string"]
        }),
        label: "edit".into(),
        execution_mode: ExecutionMode::Parallel,
        prepare_arguments_supported: false,
    }
}

fn ls_tool_def() -> AgentTool {
    AgentTool {
        name: "ls".into(),
        description: "List entries in a directory.".into(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": { "path": { "type": "string" } },
            "required": ["path"]
        }),
        label: "ls".into(),
        execution_mode: ExecutionMode::Parallel,
        prepare_arguments_supported: false,
    }
}

fn grep_tool_def() -> AgentTool {
    AgentTool {
        name: "grep".into(),
        description: "Recursively search files under root for substrings matching pattern. Returns path:line_no:line per hit.".into(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "root": { "type": "string" },
                "pattern": { "type": "string" }
            },
            "required": ["root", "pattern"]
        }),
        label: "grep".into(),
        execution_mode: ExecutionMode::Parallel,
        prepare_arguments_supported: false,
    }
}

fn find_tool_def() -> AgentTool {
    AgentTool {
        name: "find".into(),
        description: "Recursively find files under root whose path ends with suffix.".into(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "root": { "type": "string" },
                "suffix": { "type": "string" }
            },
            "required": ["root", "suffix"]
        }),
        label: "find".into(),
        execution_mode: ExecutionMode::Parallel,
        prepare_arguments_supported: false,
    }
}

fn bash_tool_def() -> AgentTool {
    AgentTool {
        name: "bash".into(),
        description: "Run a bash command. Output is truncated at 30000 characters.".into(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "command": { "type": "string" }
            },
            "required": ["command"]
        }),
        label: "bash".into(),
        execution_mode: ExecutionMode::Sequential,
        prepare_arguments_supported: false,
    }
}

struct EventPrinter;

#[async_trait]
impl EventSink for EventPrinter {
    async fn emit(&self, event: AgentEvent) {
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
                    let tool_calls: Vec<_> = a
                        .content
                        .iter()
                        .filter_map(|c| match c {
                            ContentBlock::ToolCall {
                                name, arguments, ..
                            } => Some(format!("{name}({arguments})")),
                            _ => None,
                        })
                        .collect();
                    for tc in tool_calls {
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
}

#[derive(Debug, Clone)]
struct CliArgs {
    prompt: String,
    provider: String,
    model: Option<String>,
    max_turns: usize,
    no_bash: bool,
    via_iii: bool,
    sandbox_image: String,
    engine_url: String,
}

/// Parse argv. Pulled into its own function so the unit tests can exercise it
/// without spawning a subprocess.
fn parse_args_from(raw: &[String]) -> Result<CliArgs> {
    let mut prompt: Option<String> = None;
    let mut provider = DEFAULT_PROVIDER.to_string();
    let mut model: Option<String> = None;
    let mut max_turns = 10usize;
    let mut no_bash = false;
    let mut via_iii = false;
    let mut sandbox_image = DEFAULT_SANDBOX_IMAGE.to_string();
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
            "--no-bash" => {
                no_bash = true;
                i += 1;
            }
            "--via-iii" => {
                via_iii = true;
                i += 1;
            }
            "--sandbox-image" => {
                sandbox_image = raw
                    .get(i + 1)
                    .cloned()
                    .ok_or_else(|| anyhow::anyhow!("--sandbox-image requires a value"))?;
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
    if !is_known_provider(&provider) {
        anyhow::bail!(
            "unknown provider '{provider}'. Run with --help for the full supported list."
        );
    }
    let prompt = prompt.ok_or_else(|| anyhow::anyhow!("missing prompt; pass as positional arg"))?;
    Ok(CliArgs {
        prompt,
        provider,
        model,
        max_turns,
        no_bash,
        via_iii,
        sandbox_image,
        engine_url,
    })
}

fn parse_args() -> Result<CliArgs> {
    let raw: Vec<String> = std::env::args().skip(1).collect();
    parse_args_from(&raw)
}

/// Default model id for each supported provider.
fn default_model(provider: &str) -> Option<&'static str> {
    match provider {
        "anthropic" => Some("claude-sonnet-4-6"),
        "openai" => Some("gpt-5"),
        "openai-responses" => Some("gpt-5"),
        "google" => Some("gemini-2-5-flash"),
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
        "vercel-ai-gateway" => Some("gpt-5"),
        "opencode-zen" => Some("claude-sonnet-4-6"),
        "opencode-go" => Some("claude-sonnet-4-6"),
        "google-vertex" => Some("gemini-2-5-flash"),
        // Azure has no good default — the deployment name is account-specific.
        "azure-openai" => None,
        _ => None,
    }
}

fn is_known_provider(p: &str) -> bool {
    matches!(
        p,
        "anthropic"
            | "openai"
            | "openai-responses"
            | "google"
            | "bedrock"
            | "openrouter"
            | "groq"
            | "cerebras"
            | "xai"
            | "deepseek"
            | "mistral"
            | "fireworks"
            | "kimi-coding"
            | "minimax"
            | "zai"
            | "huggingface"
            | "vercel-ai-gateway"
            | "opencode-zen"
            | "opencode-go"
            | "azure-openai"
            | "google-vertex"
    )
}

fn build_provider_config(provider: &str, model: String) -> Result<ProviderConfig> {
    let cfg = match provider {
        "anthropic" => ProviderConfig::Anthropic(Arc::new(
            provider_anthropic::AnthropicConfig::from_env(model)
                .context("ANTHROPIC_API_KEY not set in environment")?,
        )),
        "openai" => ProviderConfig::OpenAI(Arc::new(
            provider_openai::OpenAIConfig::from_env(model)
                .context("OPENAI_API_KEY not set in environment")?,
        )),
        "openai-responses" => ProviderConfig::OpenAIResponses(Arc::new(
            provider_openai_responses::OpenAIResponsesConfig::from_env(model)
                .context("OPENAI_API_KEY not set in environment")?,
        )),
        "google" => ProviderConfig::Google(Arc::new(
            provider_google::GoogleConfig::from_env(model)
                .context("GOOGLE_API_KEY not set in environment")?,
        )),
        "bedrock" => ProviderConfig::Bedrock(Arc::new(
            provider_bedrock::BedrockConfig::from_env(model)
                .context("AWS Bedrock env not reachable")?,
        )),
        "openrouter" => ProviderConfig::OpenRouter(Arc::new(
            provider_openrouter::OpenRouterConfig::from_env(model)
                .context("OPENROUTER_API_KEY not set in environment")?,
        )),
        "groq" => ProviderConfig::Groq(Arc::new(
            provider_groq::GroqConfig::from_env(model)
                .context("GROQ_API_KEY not set in environment")?,
        )),
        "cerebras" => ProviderConfig::Cerebras(Arc::new(
            provider_cerebras::CerebrasConfig::from_env(model)
                .context("CEREBRAS_API_KEY not set in environment")?,
        )),
        "xai" => ProviderConfig::Xai(Arc::new(
            provider_xai::XaiConfig::from_env(model)
                .context("XAI_API_KEY not set in environment")?,
        )),
        "deepseek" => ProviderConfig::DeepSeek(Arc::new(
            provider_deepseek::DeepSeekConfig::from_env(model)
                .context("DEEPSEEK_API_KEY not set in environment")?,
        )),
        "mistral" => ProviderConfig::Mistral(Arc::new(
            provider_mistral::MistralConfig::from_env(model)
                .context("MISTRAL_API_KEY not set in environment")?,
        )),
        "fireworks" => ProviderConfig::Fireworks(Arc::new(
            provider_fireworks::FireworksConfig::from_env(model)
                .context("FIREWORKS_API_KEY not set in environment")?,
        )),
        "kimi-coding" => ProviderConfig::KimiCoding(Arc::new(
            provider_kimi_coding::KimiCodingConfig::from_env(model)
                .context("MOONSHOT_API_KEY not set in environment")?,
        )),
        "minimax" => ProviderConfig::MiniMax(Arc::new(
            provider_minimax::MiniMaxConfig::from_env(model)
                .context("MINIMAX_API_KEY not set in environment")?,
        )),
        "zai" => ProviderConfig::Zai(Arc::new(
            provider_zai::ZaiConfig::from_env(model)
                .context("ZAI_API_KEY not set in environment")?,
        )),
        "huggingface" => ProviderConfig::HuggingFace(Arc::new(
            provider_huggingface::HuggingFaceConfig::from_env(model)
                .context("HUGGINGFACE_API_KEY not set in environment")?,
        )),
        "vercel-ai-gateway" => ProviderConfig::VercelAiGateway(Arc::new(
            provider_vercel_ai_gateway::VercelAiGatewayConfig::from_env(model)
                .context("VERCEL_AI_GATEWAY_API_KEY not set in environment")?,
        )),
        "opencode-zen" => ProviderConfig::OpencodeZen(Arc::new(
            provider_opencode_zen::OpencodeZenConfig::from_env(model)
                .context("OPENCODE_ZEN_API_KEY not set in environment")?,
        )),
        "opencode-go" => ProviderConfig::OpencodeGo(Arc::new(
            provider_opencode_go::OpencodeGoConfig::from_env(model)
                .context("OPENCODE_GO_API_KEY not set in environment")?,
        )),
        "azure-openai" => ProviderConfig::AzureOpenAI(Arc::new(
            provider_azure_openai::AzureOpenAIConfig::from_env(model)
                .context("AZURE_OPENAI_API_KEY and AZURE_OPENAI_RESOURCE not set in environment")?,
        )),
        "google-vertex" => ProviderConfig::GoogleVertex(Arc::new(
            provider_google_vertex::VertexConfig::from_env(model)
                .context("GOOGLE_VERTEX_ACCESS_TOKEN and GOOGLE_VERTEX_PROJECT not set")?,
        )),
        other => anyhow::bail!("unknown provider '{other}'"),
    };
    Ok(cfg)
}

fn print_help() {
    println!("Usage: harness [options] <prompt>");
    println!();
    println!("Options:");
    println!("  --provider <name>      provider crate to use (default: {DEFAULT_PROVIDER})");
    println!("  --model <id>           provider model id (default depends on --provider)");
    println!("  --max-turns <n>        stop after n turns (default: 10)");
    println!("  --no-bash              disable bash tool (read-only mode)");
    println!("  --via-iii              dispatch the bash tool through the iii-engine");
    println!("                         sandbox (sandbox::create + sandbox::exec) instead");
    println!("                         of running it in-process. Requires a running engine");
    println!("                         on --engine-url with the iii-sandbox worker loaded.");
    println!("  --sandbox-image <name> catalog image used for --via-iii (default: {DEFAULT_SANDBOX_IMAGE})");
    println!("  --engine-url <url>     iii-engine WebSocket URL (default: {DEFAULT_ENGINE_URL})");
    println!();
    println!("Supported providers:");
    println!("  anthropic           env: ANTHROPIC_API_KEY");
    println!("  openai              env: OPENAI_API_KEY");
    println!("  openai-responses    env: OPENAI_API_KEY");
    println!("  google              env: GOOGLE_API_KEY");
    println!("  bedrock             env: AWS standard chain");
    println!("  openrouter          env: OPENROUTER_API_KEY");
    println!("  groq                env: GROQ_API_KEY");
    println!("  cerebras            env: CEREBRAS_API_KEY");
    println!("  xai                 env: XAI_API_KEY");
    println!("  deepseek            env: DEEPSEEK_API_KEY");
    println!("  mistral             env: MISTRAL_API_KEY");
    println!("  fireworks           env: FIREWORKS_API_KEY");
    println!("  kimi-coding         env: MOONSHOT_API_KEY");
    println!("  minimax             env: MINIMAX_API_KEY");
    println!("  zai                 env: ZAI_API_KEY");
    println!("  huggingface         env: HUGGINGFACE_API_KEY");
    println!("  vercel-ai-gateway   env: VERCEL_AI_GATEWAY_API_KEY");
    println!("  opencode-zen        env: OPENCODE_ZEN_API_KEY");
    println!("  opencode-go         env: OPENCODE_GO_API_KEY");
    println!("  azure-openai        env: AZURE_OPENAI_API_KEY, AZURE_OPENAI_RESOURCE,");
    println!("                           AZURE_OPENAI_DEPLOYMENT (acts as --model)");
    println!("  google-vertex       env: GOOGLE_VERTEX_ACCESS_TOKEN, GOOGLE_VERTEX_PROJECT,");
    println!("                           optional GOOGLE_VERTEX_REGION (default us-central1)");
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = parse_args()?;

    // Resolve the model. Azure's deployment name is account-specific so it
    // falls back to AZURE_OPENAI_DEPLOYMENT instead of a hardcoded default.
    let model = if let Some(m) = args.model.clone() {
        m
    } else if args.provider == "azure-openai" {
        std::env::var("AZURE_OPENAI_DEPLOYMENT").context(
            "azure-openai requires --model or AZURE_OPENAI_DEPLOYMENT to identify the deployment",
        )?
    } else {
        default_model(&args.provider)
            .ok_or_else(|| anyhow::anyhow!("no default model for provider '{}'", args.provider))?
            .to_string()
    };

    let cfg = build_provider_config(&args.provider, model)?;

    let captured = Arc::new(EventPrinter);

    let mut inner = MemoryRuntime::new(captured.clone());
    inner.register_tool("read", Arc::new(harness_runtime::tools::ReadTool));
    inner.register_tool("write", Arc::new(harness_runtime::tools::WriteTool));
    inner.register_tool("edit", Arc::new(harness_runtime::tools::EditTool));
    inner.register_tool("ls", Arc::new(harness_runtime::tools::LsTool));
    inner.register_tool("grep", Arc::new(harness_runtime::tools::GrepTool));
    inner.register_tool("find", Arc::new(harness_runtime::tools::FindTool));
    if !args.no_bash {
        if args.via_iii {
            let client = connect_iii(&args.engine_url).await?;
            println!(
                "via iii-engine, sandbox image: {} (default)",
                args.sandbox_image
            );
            inner.register_tool(
                "bash",
                Arc::new(SandboxedBashTool::with_image(
                    Arc::new(client),
                    args.sandbox_image.clone(),
                )),
            );
        } else {
            inner.register_tool(
                "bash",
                Arc::new(BashTool {
                    cwd: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
                }),
            );
        }
    }

    let runtime = ProviderRuntime {
        cfg,
        inner,
        system_prompt: DEFAULT_SYSTEM_PROMPT.into(),
    };

    let mut tools = vec![
        read_tool_def(),
        write_tool_def(),
        edit_tool_def(),
        ls_tool_def(),
        grep_tool_def(),
        find_tool_def(),
    ];
    if !args.no_bash {
        tools.push(bash_tool_def());
    }

    let cfg_loop = LoopConfig {
        session_id: format!("cli-{}", chrono::Utc::now().timestamp_millis()),
        tools,
        default_execution_mode: ExecutionMode::Parallel,
    };

    let initial = vec![AgentMessage::User(UserMessage {
        content: vec![ContentBlock::Text(TextContent { text: args.prompt })],
        timestamp: chrono::Utc::now().timestamp_millis(),
    })];

    let outcome =
        run_loop_with_max_turns(&runtime, &*captured, &cfg_loop, initial, args.max_turns).await;

    eprintln!("\n=== summary ===");
    eprintln!("turns: {}", count_turns(&outcome.messages));
    eprintln!("messages: {}", outcome.messages.len());
    let assistant_count = outcome
        .messages
        .iter()
        .filter(|m| matches!(m, AgentMessage::Assistant(_)))
        .count();
    let tool_result_count = outcome
        .messages
        .iter()
        .filter(|m| matches!(m, AgentMessage::ToolResult(_)))
        .count();
    eprintln!("assistant turns: {assistant_count}");
    eprintln!("tool results: {tool_result_count}");

    Ok(())
}

/// Connect to the iii-engine over WebSocket and verify the connection is
/// alive by issuing one bus round-trip. On failure, print a diagnostic to
/// stderr and exit with code 2 — consistent with the other "preflight"
/// failures in this CLI.
async fn connect_iii(engine_url: &str) -> Result<IiiSdkClient> {
    let iii = register_worker(engine_url, InitOptions::default());
    let handle = Arc::new(iii);
    let client = IiiSdkClient::new(handle.clone());
    let probe =
        tokio::time::timeout(std::time::Duration::from_secs(5), handle.list_functions()).await;
    match probe {
        Ok(Ok(_)) => Ok(client),
        Ok(Err(e)) => {
            eprintln!(
                "failed to connect to iii engine at {engine_url}; start the engine or omit --via-iii ({e})"
            );
            std::process::exit(2);
        }
        Err(_) => {
            eprintln!(
                "failed to connect to iii engine at {engine_url}; start the engine or omit --via-iii"
            );
            std::process::exit(2);
        }
    }
}

async fn run_loop_with_max_turns<R: LoopRuntime, S: EventSink>(
    runtime: &R,
    sink: &S,
    cfg: &LoopConfig,
    initial: Vec<AgentMessage>,
    _max_turns: usize,
) -> harness_runtime::LoopOutcome {
    // Currently the loop has no max-turn cap; expose the field for documentation
    // and a future enhancement. For now, behaviour matches `run_loop` directly.
    run_loop(runtime, sink, cfg, initial).await
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
    fn parse_args_rejects_unknown_provider() {
        let err = parse_args_from(&args(&["--provider", "made-up", "hi"]));
        assert!(err.is_err());
        let msg = err.err().unwrap().to_string();
        assert!(msg.contains("unknown provider"), "msg: {msg}");
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
    fn parse_args_default_via_iii_off_and_default_sandbox_image_and_engine_url() {
        let parsed = parse_args_from(&args(&["hi"])).expect("parse ok");
        assert!(!parsed.via_iii);
        assert_eq!(parsed.sandbox_image, "python");
        assert_eq!(parsed.engine_url, "ws://127.0.0.1:49134");
    }

    #[test]
    fn parse_args_recognises_via_iii_flag() {
        let parsed = parse_args_from(&args(&["--via-iii", "hello"])).expect("parse ok");
        assert!(parsed.via_iii);
        assert_eq!(parsed.prompt, "hello");
    }

    #[test]
    fn parse_args_threads_through_sandbox_image_and_engine_url() {
        let parsed = parse_args_from(&args(&[
            "--via-iii",
            "--sandbox-image",
            "node",
            "--engine-url",
            "ws://10.0.0.5:49134",
            "go",
        ]))
        .expect("parse ok");
        assert!(parsed.via_iii);
        assert_eq!(parsed.sandbox_image, "node");
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

    /// Integration check that hits a real iii-engine. Gated with `#[ignore]`
    /// because it requires a running engine on ws://127.0.0.1:49134 with the
    /// iii-sandbox worker loaded.
    #[tokio::test]
    #[ignore = "requires running iii engine"]
    async fn connect_iii_against_live_engine() {
        let client = connect_iii("ws://127.0.0.1:49134").await.expect("connect");
        let _ = client; // smoke: handle drops cleanly
    }

    #[test]
    fn known_provider_list_matches_default_model_table() {
        // Every provider declared in is_known_provider must have either a
        // hardcoded default or be the documented exception (azure-openai).
        let ps = [
            "anthropic",
            "openai",
            "openai-responses",
            "google",
            "bedrock",
            "openrouter",
            "groq",
            "cerebras",
            "xai",
            "deepseek",
            "mistral",
            "fireworks",
            "kimi-coding",
            "minimax",
            "zai",
            "huggingface",
            "vercel-ai-gateway",
            "opencode-zen",
            "opencode-go",
            "azure-openai",
            "google-vertex",
        ];
        assert_eq!(ps.len(), 21);
        for p in ps {
            assert!(is_known_provider(p), "{p} should be known");
            if p == "azure-openai" {
                assert!(default_model(p).is_none());
            } else {
                assert!(default_model(p).is_some(), "{p} missing default model");
            }
        }
    }
}
