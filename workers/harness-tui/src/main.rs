//! Interactive terminal UI binary for the harness agent loop.
//!
//! Mirrors the `harness` CLI's flag surface but renders AgentEvents into a
//! ratatui app instead of stdout. The agent loop runs on a background tokio
//! task while the foreground task pumps crossterm input + redraws.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use async_trait::async_trait;
use crossterm::event::{
    DisableMouseCapture, EnableMouseCapture, Event as CtEvent, KeyCode, KeyEvent, KeyEventKind,
    KeyModifiers,
};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use harness_runtime::{
    run_loop, EventSink, HookOutcome, LoopConfig, LoopRuntime, MemoryRuntime, ToolHandler,
};
use harness_types::{
    AgentMessage, AgentTool, AssistantMessage, ContentBlock, ExecutionMode, TextContent, ToolCall,
    ToolResult, UserMessage,
};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;

use harness_tui::app::SlashOutcome;
use harness_tui::fuzzy::FuzzyIndex;
use harness_tui::{App, AppStatus, ChannelSink, RuntimeHandle};

const DEFAULT_PROVIDER: &str = "anthropic";
const DEFAULT_SYSTEM_PROMPT: &str = "You are a coding assistant running on the iii harness. Use the tools provided to inspect and modify files. Keep responses focused and concrete.";

/// Tagged config for the selected provider. Same shape as harness-cli.
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
            "properties": { "path": { "type": "string" } },
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
        description: "Replace one occurrence of old_string with new_string in a file.".into(),
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
        description: "Recursively search files under root for substrings matching pattern.".into(),
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
            "properties": { "command": { "type": "string" } },
            "required": ["command"]
        }),
        label: "bash".into(),
        execution_mode: ExecutionMode::Sequential,
        prepare_arguments_supported: false,
    }
}

#[derive(Debug, Clone)]
struct CliArgs {
    prompt: Option<String>,
    provider: String,
    model: Option<String>,
    max_turns: usize,
    no_bash: bool,
    system_path: Option<String>,
}

fn parse_args_from(raw: &[String]) -> Result<CliArgs> {
    let mut prompt: Option<String> = None;
    let mut provider = DEFAULT_PROVIDER.to_string();
    let mut model: Option<String> = None;
    let mut max_turns = 10usize;
    let mut no_bash = false;
    let mut system_path: Option<String> = None;
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
            "--system" => {
                system_path = Some(
                    raw.get(i + 1)
                        .cloned()
                        .ok_or_else(|| anyhow::anyhow!("--system requires a path"))?,
                );
                i += 2;
            }
            "--help" | "-h" => {
                print_help();
                std::process::exit(0);
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
    Ok(CliArgs {
        prompt,
        provider,
        model,
        max_turns,
        no_bash,
        system_path,
    })
}

fn parse_args() -> Result<CliArgs> {
    let raw: Vec<String> = std::env::args().skip(1).collect();
    parse_args_from(&raw)
}

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
    println!("Usage: harness-tui [options] [<initial prompt>]");
    println!();
    println!("Options:");
    println!("  --provider <name>   provider crate to use (default: {DEFAULT_PROVIDER})");
    println!("  --model <id>        provider model id (default depends on --provider)");
    println!("  --max-turns <n>     stop after n turns (default: 10)");
    println!("  --no-bash           disable bash tool (read-only mode)");
    println!("  --system <path>     read system prompt from file");
    println!();
    println!("If no initial prompt is given, the TUI starts in idle mode awaiting first input.");
    println!();
    println!("Keys:");
    println!("  Enter         submit (start run, or steer if running)");
    println!("  Alt+Enter     submit as follow-up");
    println!("  Esc           clear editor / abort run");
    println!("  Ctrl+C        quit");
    println!("  Ctrl+L        clear scrollback");
    println!("  Ctrl+O        toggle collapsed tool output");
    println!("  Ctrl+T        toggle expanded thinking blocks");
    println!("  PgUp/PgDn     scroll scrollback");
    println!("  Up/Down       browse submitted message history");
}

/// Bridge from TUI's `RuntimeHandle` into the shared `MemoryRuntime` instance
/// owned by the loop runner. Cheap to clone via Arc.
struct MemoryRuntimeHandle {
    inner: MemoryRuntime,
}

impl RuntimeHandle for MemoryRuntimeHandle {
    fn enqueue_steering(&self, session_id: &str, message: AgentMessage) {
        self.inner.enqueue_steering(session_id, vec![message]);
    }
    fn enqueue_followup(&self, session_id: &str, message: AgentMessage) {
        self.inner.enqueue_followup(session_id, vec![message]);
    }
    fn abort(&self, session_id: &str) {
        self.inner.set_abort(session_id, true);
    }
}

fn main() -> Result<()> {
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async_main())
}

async fn async_main() -> Result<()> {
    let args = parse_args()?;

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

    let cfg = build_provider_config(&args.provider, model.clone())?;

    let system_prompt = if let Some(path) = &args.system_path {
        std::fs::read_to_string(path).with_context(|| format!("reading system prompt {path}"))?
    } else {
        DEFAULT_SYSTEM_PROMPT.to_string()
    };

    let (sink, rx) = ChannelSink::new();
    let sink_arc: Arc<dyn EventSink> = Arc::new(sink);

    let mut inner = MemoryRuntime::new(sink_arc.clone());
    inner.register_tool("read", Arc::new(harness_runtime::tools::ReadTool));
    inner.register_tool("write", Arc::new(harness_runtime::tools::WriteTool));
    inner.register_tool("edit", Arc::new(harness_runtime::tools::EditTool));
    inner.register_tool("ls", Arc::new(harness_runtime::tools::LsTool));
    inner.register_tool("grep", Arc::new(harness_runtime::tools::GrepTool));
    inner.register_tool("find", Arc::new(harness_runtime::tools::FindTool));
    if !args.no_bash {
        inner.register_tool(
            "bash",
            Arc::new(BashTool {
                cwd: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            }),
        );
    }

    let session_id = format!("tui-{}", chrono::Utc::now().timestamp_millis());
    let cwd = std::env::current_dir().map_or_else(|_| ".".into(), |p| p.display().to_string());

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

    let loop_cfg = LoopConfig {
        session_id: session_id.clone(),
        tools,
        default_execution_mode: ExecutionMode::Parallel,
    };

    let runtime_arc = Arc::new(ProviderRuntime {
        cfg,
        inner: inner.clone(),
        system_prompt,
    });

    let runtime_handle = Arc::new(MemoryRuntimeHandle {
        inner: inner.clone(),
    });

    enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut app = App::new(
        session_id.clone(),
        args.provider.clone(),
        model,
        cwd.clone(),
        rx,
        runtime_handle,
    );
    app.fuzzy_index = Some(FuzzyIndex::index(&PathBuf::from(&cwd)));

    // Optionally kick off a run with the initial prompt argument.
    if let Some(initial) = args.prompt.clone() {
        spawn_run(
            runtime_arc.clone(),
            sink_arc.clone(),
            loop_cfg.clone(),
            initial,
            Vec::new(),
            args.max_turns,
        );
        app.status = AppStatus::Running;
    }

    let result = run_event_loop(
        &mut terminal,
        &mut app,
        runtime_arc,
        sink_arc,
        loop_cfg,
        args.max_turns,
    )
    .await;

    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;
    result
}

/// Foreground event loop: pump crossterm input, drain the event channel, redraw.
async fn run_event_loop<B: ratatui::backend::Backend>(
    terminal: &mut Terminal<B>,
    app: &mut App,
    runtime_arc: Arc<ProviderRuntime>,
    sink_arc: Arc<dyn EventSink>,
    loop_cfg: LoopConfig,
    max_turns: usize,
) -> Result<()> {
    let tick_rate = Duration::from_millis(33); // ~30 fps
    let spinner_rate = Duration::from_millis(100);
    let mut last_tick = Instant::now();
    let mut last_spinner = Instant::now();

    loop {
        app.drain_events();
        terminal.draw(|f| harness_tui::render::draw(f, app))?;

        let timeout = tick_rate
            .checked_sub(last_tick.elapsed())
            .unwrap_or_else(|| Duration::from_millis(0));

        if crossterm::event::poll(timeout)? {
            if let CtEvent::Key(key) = crossterm::event::read()? {
                if key.kind == KeyEventKind::Release {
                    continue;
                }
                handle_key(
                    key,
                    app,
                    runtime_arc.clone(),
                    sink_arc.clone(),
                    &loop_cfg,
                    max_turns,
                );
            }
        }

        if last_tick.elapsed() >= tick_rate {
            last_tick = Instant::now();
        }
        if last_spinner.elapsed() >= spinner_rate {
            app.tick();
            last_spinner = Instant::now();
        }

        if app.should_quit {
            break;
        }
    }
    Ok(())
}

fn handle_key(
    key: KeyEvent,
    app: &mut App,
    runtime_arc: Arc<ProviderRuntime>,
    sink_arc: Arc<dyn EventSink>,
    loop_cfg: &LoopConfig,
    max_turns: usize,
) {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    let shift = key.modifiers.contains(KeyModifiers::SHIFT);

    match (key.code, ctrl, alt, shift) {
        (KeyCode::Char('c'), true, _, _) => {
            if matches!(app.status, AppStatus::Running) {
                app.runtime.abort(&app.session_id);
                app.status = AppStatus::Aborted;
            } else {
                app.should_quit = true;
            }
        }
        (KeyCode::Char('l'), true, _, _) => app.clear_scrollback(),
        (KeyCode::Char('o'), true, _, _) => app.toggle_tools_collapsed(),
        (KeyCode::Char('t'), true, _, _) => app.toggle_expand_thinking(),
        (KeyCode::Char('v'), true, _, _) => app.paste_from_clipboard(),
        (KeyCode::Char('w'), true, _, _) => {
            app.editor.delete_word_back();
            app.refresh_command_picker();
            app.refresh_file_picker();
        }
        (KeyCode::BackTab, _, _, _) => app.cycle_thinking_level(),
        (KeyCode::Tab, _, _, true) => app.cycle_thinking_level(),
        (KeyCode::Tab, _, _, _) => {
            if app.command_picker_visible {
                app.complete_slash();
            } else if app.file_picker_visible {
                app.complete_file();
            }
        }
        (KeyCode::Char(c), false, _, _) => {
            app.editor.insert_char(c);
            app.refresh_command_picker();
            app.refresh_file_picker();
        }
        (KeyCode::Char(c), true, _, _) if c.is_ascii_alphabetic() => {
            let _ = c;
        }
        (KeyCode::Backspace, _, _, _) => {
            app.editor.delete_back();
            app.refresh_command_picker();
            app.refresh_file_picker();
        }
        (KeyCode::Delete, _, _, _) => {
            app.editor.delete_forward();
            app.refresh_command_picker();
            app.refresh_file_picker();
        }
        (KeyCode::Left, _, _, _) => {
            app.editor.move_left();
            app.refresh_file_picker();
        }
        (KeyCode::Right, _, _, _) => {
            app.editor.move_right();
            app.refresh_file_picker();
        }
        (KeyCode::Home, _, _, _) => app.editor.home(),
        (KeyCode::End, _, _, _) => app.editor.end(),
        (KeyCode::Up, _, _, _) => {
            if app.command_picker_visible || app.file_picker_visible {
                app.picker_select_prev();
            } else if app.editor.is_multiline() && !app.editor.cursor_at_first_row() {
                app.editor.move_up();
            } else {
                app.history_prev();
            }
        }
        (KeyCode::Down, _, _, _) => {
            if app.command_picker_visible || app.file_picker_visible {
                app.picker_select_next();
            } else if app.editor.is_multiline() && !app.editor.cursor_at_last_row() {
                app.editor.move_down();
            } else {
                app.history_next();
            }
        }
        (KeyCode::PageUp, _, _, _) => app.scroll_up(5),
        (KeyCode::PageDown, _, _, _) => app.scroll_down(5),
        (KeyCode::Esc, _, _, _) => app.handle_escape(),
        (KeyCode::Enter, _, _, true) => {
            // Shift+Enter inserts a newline regardless of picker state.
            app.editor.insert_newline();
            app.refresh_command_picker();
            app.refresh_file_picker();
        }
        (KeyCode::Enter, _, true, false) => {
            // Pickers absorbed elsewhere; Alt+Enter is follow-up.
            let raw = app.editor.text();
            if maybe_handle_inline_bash(
                app,
                &raw,
                runtime_arc.clone(),
                sink_arc.clone(),
                loop_cfg,
                max_turns,
                /* followup */ true,
            ) {
                return;
            }
            if maybe_handle_slash(app, &raw) {
                app.editor.clear();
                return;
            }
            if let Some(text) = app.submit_followup() {
                let attachments = app.drain_attachments_as_blocks();
                spawn_run(
                    runtime_arc,
                    sink_arc,
                    loop_cfg.clone(),
                    text,
                    attachments,
                    max_turns,
                );
                app.status = AppStatus::Running;
            }
        }
        (KeyCode::Enter, _, false, false) => {
            if app.command_picker_visible {
                app.complete_slash();
                app.command_picker_visible = false;
                return;
            }
            if app.file_picker_visible {
                app.complete_file();
                return;
            }
            let raw = app.editor.text();
            if maybe_handle_inline_bash(
                app,
                &raw,
                runtime_arc.clone(),
                sink_arc.clone(),
                loop_cfg,
                max_turns,
                /* followup */ false,
            ) {
                return;
            }
            if maybe_handle_slash(app, &raw) {
                app.editor.clear();
                return;
            }
            if let Some(text) = app.submit_message() {
                let attachments = app.drain_attachments_as_blocks();
                spawn_run(
                    runtime_arc,
                    sink_arc,
                    loop_cfg.clone(),
                    text,
                    attachments,
                    max_turns,
                );
                app.status = AppStatus::Running;
            }
        }
        _ => {}
    }
}

/// Returns true when the text was a slash command (and was routed). Performs
/// process-level side effects (chdir, quit) the App can't.
fn maybe_handle_slash(app: &mut App, text: &str) -> bool {
    let trimmed = text.trim_end();
    if !trimmed.starts_with('/') {
        return false;
    }
    match app.route_slash(trimmed) {
        SlashOutcome::Handled => true,
        SlashOutcome::Quit => {
            app.should_quit = true;
            true
        }
        SlashOutcome::Chdir(path) => {
            match std::env::set_current_dir(&path) {
                Ok(()) => {
                    app.cwd = path.display().to_string();
                    app.fuzzy_index = Some(FuzzyIndex::index(&path));
                    app.push_notification(format!("[slash] cwd: {}", app.cwd));
                }
                Err(e) => {
                    app.push_notification(format!("[slash] cwd failed: {e}"));
                }
            }
            true
        }
        SlashOutcome::NotFound => true,
    }
}

/// Run an inline `!cmd` or `!!cmd` and either submit the output as a user
/// message or print it to scrollback. Returns true when the text matched the
/// inline-bash prefix and was handled.
fn maybe_handle_inline_bash(
    app: &mut App,
    text: &str,
    runtime_arc: Arc<ProviderRuntime>,
    sink_arc: Arc<dyn EventSink>,
    loop_cfg: &LoopConfig,
    max_turns: usize,
    followup: bool,
) -> bool {
    let parsed = match harness_tui::bash::parse(text) {
        Some(p) => p,
        None => return false,
    };
    app.editor.clear();
    if parsed.command.is_empty() {
        app.push_notification("[bash] empty command".to_string());
        return true;
    }
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let output = std::process::Command::new("bash")
        .arg("-lc")
        .arg(&parsed.command)
        .current_dir(&cwd)
        .output();
    let body = match output {
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
            let exit = o.status.code().unwrap_or(-1);
            format!("exit={exit}\n{combined}")
        }
        Err(e) => format!("bash spawn failed: {e}"),
    };

    let formatted = harness_tui::bash::format_for_submission(&parsed.command, &body);
    if parsed.silent {
        for line in formatted.lines() {
            app.push_notification(line.to_string());
        }
        return true;
    }

    let _ = followup;
    if matches!(app.status, AppStatus::Running) {
        app.runtime
            .enqueue_steering(&app.session_id, harness_tui::app::user_message(&formatted));
        app.push_notification(format!("[bash steered] {}", parsed.command));
    } else if let Some(text) = app.submit_text_as_user(formatted) {
        spawn_run(
            runtime_arc,
            sink_arc,
            loop_cfg.clone(),
            text,
            Vec::new(),
            max_turns,
        );
        app.status = AppStatus::Running;
    }
    true
}

/// Spawn the agent loop for one initial prompt on the tokio runtime. The
/// background task owns its own `Arc` clones; the foreground keeps draining
/// the event channel. `attachments` are content blocks (typically images)
/// prepended before the text block on the first user message.
fn spawn_run(
    runtime: Arc<ProviderRuntime>,
    sink: Arc<dyn EventSink>,
    loop_cfg: LoopConfig,
    prompt: String,
    attachments: Vec<ContentBlock>,
    _max_turns: usize,
) {
    tokio::spawn(async move {
        let mut content: Vec<ContentBlock> = attachments;
        if !prompt.is_empty() {
            content.push(ContentBlock::Text(TextContent { text: prompt }));
        }
        let initial = vec![AgentMessage::User(UserMessage {
            content,
            timestamp: chrono::Utc::now().timestamp_millis(),
        })];
        let _ = run_loop(&*runtime, &*sink, &loop_cfg, initial).await;
    });
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
        assert_eq!(parsed.prompt.as_deref(), Some("hello"));
    }

    #[test]
    fn parse_args_no_prompt_is_ok() {
        let parsed = parse_args_from(&args(&[])).expect("parse ok");
        assert!(parsed.prompt.is_none());
    }

    #[test]
    fn parse_args_threads_through_provider_and_model() {
        let parsed = parse_args_from(&args(&[
            "--provider",
            "openai",
            "--model",
            "gpt-test",
            "hi",
        ]))
        .expect("parse ok");
        assert_eq!(parsed.provider, "openai");
        assert_eq!(parsed.model.as_deref(), Some("gpt-test"));
    }

    #[test]
    fn parse_args_rejects_unknown_provider() {
        let err = parse_args_from(&args(&["--provider", "made-up", "hi"]));
        assert!(err.is_err());
    }

    #[test]
    fn parse_args_accepts_no_bash_and_max_turns() {
        let parsed =
            parse_args_from(&args(&["--no-bash", "--max-turns", "5", "go"])).expect("parse ok");
        assert!(parsed.no_bash);
        assert_eq!(parsed.max_turns, 5);
    }

    #[test]
    fn parse_args_accepts_system_path() {
        let parsed = parse_args_from(&args(&["--system", "/tmp/sys.txt", "go"])).expect("parse ok");
        assert_eq!(parsed.system_path.as_deref(), Some("/tmp/sys.txt"));
    }
}
