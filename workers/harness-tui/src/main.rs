//! Interactive terminal UI binary for the harness agent loop.
//!
//! Mirrors the `harness` CLI's flag surface but renders AgentEvents into a
//! ratatui app instead of stdout. The agent loop runs on a background tokio
//! task while the foreground task pumps crossterm input + redraws.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use crossterm::event::{
    DisableMouseCapture, EnableMouseCapture, Event as CtEvent, KeyCode, KeyEvent, KeyEventKind,
    KeyModifiers,
};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use harness_runtime::{EventSink, EVENTS_STREAM};
use harness_types::{
    AgentEvent, AgentMessage, AgentTool, ContentBlock, ExecutionMode, TextContent, UserMessage,
};
use iii_sdk::{register_worker, InitOptions, TriggerRequest, III};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use serde_json::{json, Value};

use harness_tui::app::SlashOutcome;
use harness_tui::fuzzy::FuzzyIndex;
use harness_tui::keybindings::{KeyAction, KeybindingsManager};
use harness_tui::theme::Theme;
use harness_tui::watcher::{ConfigReloadEvent, ConfigWatcher};
use harness_tui::{App, AppStatus, ChannelSink, RuntimeHandle};

const DEFAULT_PROVIDER: &str = "anthropic";
const DEFAULT_SYSTEM_PROMPT: &str = "You are a coding assistant running on the iii harness. Use the tools provided to inspect and modify files. Keep responses focused and concrete.";

/// Per-session driver that holds the iii client + the per-run params the
/// loop needs. Replaces the old in-process `ProviderRuntime`. Cheap to clone.
struct IiiAgentDriver {
    iii: Arc<III>,
    provider: String,
    model: String,
    system_prompt: String,
    max_turns: usize,
    tools: Vec<AgentTool>,
}

impl IiiAgentDriver {
    /// Trigger `agent::run_loop` for one initial-prompt batch. Events flow
    /// through the engine's `agent::events/<sid>` stream, which is consumed
    /// separately by the stream subscriber task feeding [`ChannelSink`].
    async fn run(&self, session_id: &str, initial: Vec<AgentMessage>) {
        let payload = json!({
            "session_id": session_id,
            "provider": self.provider,
            "model": self.model,
            "system_prompt": self.system_prompt,
            "messages": initial,
            "tools": self.tools,
            "max_turns": self.max_turns,
        });
        // iii-sdk defaults None to 30s; agent::run_loop is multi-turn and
        // routinely runs longer. Cap at 10 minutes so the loop completes
        // instead of timing out under healthy conditions.
        let _ = self
            .iii
            .trigger(TriggerRequest {
                function_id: "agent::run_loop".to_string(),
                payload,
                action: None,
                timeout_ms: Some(600_000),
            })
            .await;
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
    theme: String,
}

fn parse_args_from(raw: &[String]) -> Result<CliArgs> {
    let mut prompt: Option<String> = None;
    let mut provider = DEFAULT_PROVIDER.to_string();
    let mut model: Option<String> = None;
    let mut max_turns = 10usize;
    let mut no_bash = false;
    let mut system_path: Option<String> = None;
    let mut theme = "dark".to_string();
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
            "--theme" => {
                theme = raw
                    .get(i + 1)
                    .cloned()
                    .ok_or_else(|| anyhow::anyhow!("--theme requires a value"))?;
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
        theme,
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

/// Register the configured provider's `provider::<name>::stream_assistant`
/// iii function on the engine. Mirrors `harness-cli`'s 21-arm dispatcher.
async fn register_provider(iii: &III, name: &str) -> Result<()> {
    match name {
        "anthropic" => provider_anthropic::register_with_iii(iii).await,
        "openai" => provider_openai::register_with_iii(iii).await,
        "openai-responses" => provider_openai_responses::register_with_iii(iii).await,
        "google" => provider_google::register_with_iii(iii).await,
        "google-vertex" => provider_google_vertex::register_with_iii(iii).await,
        "azure-openai" => provider_azure_openai::register_with_iii(iii).await,
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
        other => Err(anyhow::anyhow!("unknown provider '{other}'")),
    }
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
    println!("  --theme <name>      colour theme: dark | light | <user-file> (default: dark)");
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

/// Bridge from TUI's `RuntimeHandle` into iii bus calls. Steering / follow-up
/// / abort all post via `iii.trigger` so the running `agent::run_loop`
/// session sees them on its next pull.
struct IiiRuntimeHandle {
    iii: Arc<III>,
}

impl IiiRuntimeHandle {
    fn fire_and_forget(&self, function_id: &'static str, payload: Value) {
        let iii = self.iii.clone();
        tokio::spawn(async move {
            let _ = iii
                .trigger(TriggerRequest {
                    function_id: function_id.to_string(),
                    payload,
                    action: None,
                    timeout_ms: None,
                })
                .await;
        });
    }
}

impl RuntimeHandle for IiiRuntimeHandle {
    fn enqueue_steering(&self, session_id: &str, message: AgentMessage) {
        self.fire_and_forget(
            "agent::push_steering",
            json!({ "session_id": session_id, "messages": vec![message] }),
        );
    }
    fn enqueue_followup(&self, session_id: &str, message: AgentMessage) {
        self.fire_and_forget(
            "agent::push_followup",
            json!({ "session_id": session_id, "messages": vec![message] }),
        );
    }
    fn abort(&self, session_id: &str) {
        self.fire_and_forget("agent::abort", json!({ "session_id": session_id }));
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

    let system_prompt = if let Some(path) = &args.system_path {
        std::fs::read_to_string(path).with_context(|| format!("reading system prompt {path}"))?
    } else {
        DEFAULT_SYSTEM_PROMPT.to_string()
    };

    let engine_url =
        std::env::var("HARNESS_ENGINE_URL").unwrap_or_else(|_| "ws://127.0.0.1:49134".to_string());
    let iii = connect_iii(&engine_url).await?;
    let iii = Arc::new(iii);

    harness_runtime::register_with_iii(iii.as_ref())
        .await
        .context("failed to register harness-runtime functions on iii engine")?;
    register_provider(iii.as_ref(), &args.provider)
        .await
        .with_context(|| format!("failed to register provider '{}'", args.provider))?;

    let (sink, rx) = ChannelSink::new();
    let sink_arc: Arc<dyn EventSink> = Arc::new(sink);

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

    let driver = Arc::new(IiiAgentDriver {
        iii: iii.clone(),
        provider: args.provider.clone(),
        model: model.clone(),
        system_prompt,
        max_turns: args.max_turns,
        tools,
    });

    let runtime_handle = Arc::new(IiiRuntimeHandle { iii: iii.clone() });

    // Subscribe to the per-session event stream; forward decoded AgentEvents
    // into the sink that the App drains. Best-effort — if the engine doesn't
    // expose `stream::list` the loop still runs and the TUI just shows no
    // intermediate events.
    let stream_task = tokio::spawn(forward_events_from_stream(
        iii.clone(),
        session_id.clone(),
        sink_arc.clone(),
    ));

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
    match Theme::load_named(&args.theme) {
        Ok(t) => app.theme = t,
        Err(e) => {
            app.push_notification(format!(
                "[theme] failed to load '{}' ({e}); using dark default",
                args.theme
            ));
        }
    }

    // Optionally kick off a run with the initial prompt argument.
    if let Some(initial) = args.prompt.clone() {
        spawn_run(driver.clone(), session_id.clone(), initial, Vec::new());
        app.status = AppStatus::Running;
    }

    let result = run_event_loop(&mut terminal, &mut app, driver, session_id).await;
    stream_task.abort();

    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;
    result
}

/// Flush queued native image escapes to stdout. Each job is positioned with
/// crossterm `MoveTo`, then the raw payload bytes are emitted, then the
/// stream is flushed so the terminal commits the image right away.
///
/// Writes happen on a fresh `stdout()` handle — the same file descriptor the
/// ratatui `CrosstermBackend` is using — so the escape bytes interleave
/// after ratatui's own draw flush.
fn write_image_escapes(escapes: &harness_tui::render::PostDrawEscapes) -> Result<()> {
    use crossterm::cursor::MoveTo;
    use crossterm::queue;
    use crossterm::style::Print;
    use std::io::Write;

    let mut out = std::io::stdout().lock();
    for job in &escapes.jobs {
        queue!(out, MoveTo(job.col, job.row), Print(&job.payload))?;
    }
    out.flush()?;
    Ok(())
}

/// Connect to the iii engine. Exits with code 2 on failure after writing a
/// remediation hint to stderr.
async fn connect_iii(engine_url: &str) -> Result<III> {
    let iii = register_worker(engine_url, InitOptions::default());
    let probe = tokio::time::timeout(std::time::Duration::from_secs(5), iii.list_functions()).await;
    match probe {
        Ok(Ok(_)) => Ok(iii),
        Ok(Err(e)) => {
            disable_raw_mode().ok();
            eprintln!(
                "failed to connect to iii engine at {engine_url}: {e}\n\
                 start the engine, then retry. Override with HARNESS_ENGINE_URL."
            );
            std::process::exit(2);
        }
        Err(_) => {
            disable_raw_mode().ok();
            eprintln!(
                "timed out connecting to iii engine at {engine_url}\n\
                 start the engine, then retry. Override with HARNESS_ENGINE_URL."
            );
            std::process::exit(2);
        }
    }
}

/// Subscribe to the per-session event stream and forward each entry into
/// the [`ChannelSink`] the TUI's `App` is draining. Each stream item is
/// expected to be an [`AgentEvent`] in JSON form (the runtime publishes
/// them via `stream::set` on `agent::events/<session_id>`). Items that
/// don't decode are silently dropped.
async fn forward_events_from_stream(iii: Arc<III>, session_id: String, sink: Arc<dyn EventSink>) {
    let mut last_index: usize = 0;
    loop {
        let resp = iii
            .trigger(TriggerRequest {
                function_id: "stream::list".to_string(),
                payload: json!({
                    "stream_name": EVENTS_STREAM,
                    "group_id": session_id,
                }),
                action: None,
                timeout_ms: None,
            })
            .await;
        if let Ok(value) = resp {
            if let Some(items) = value.get("items").and_then(Value::as_array) {
                for item in items.iter().skip(last_index) {
                    if let Some(data) = item.get("data") {
                        if let Ok(event) = serde_json::from_value::<AgentEvent>(data.clone()) {
                            sink.emit(event).await;
                        }
                    }
                }
                last_index = items.len();
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
}

/// Foreground event loop: pump crossterm input, drain the event channel, redraw.
async fn run_event_loop<B: ratatui::backend::Backend>(
    terminal: &mut Terminal<B>,
    app: &mut App,
    driver: Arc<IiiAgentDriver>,
    session_id: String,
) -> Result<()> {
    let tick_rate = Duration::from_millis(33); // ~30 fps
    let spinner_rate = Duration::from_millis(100);
    let mut last_tick = Instant::now();
    let mut last_spinner = Instant::now();

    // Spawn the hot-reload watcher. Failure is non-fatal — the TUI still
    // works, just without live config reloading.
    let watcher = spawn_config_watcher(app);

    loop {
        app.drain_events();
        if let Some(w) = &watcher {
            drain_config_reloads(app, w);
        }
        let mut escapes = harness_tui::render::PostDrawEscapes::default();
        terminal.draw(|f| harness_tui::render::draw(f, app, &mut escapes))?;
        if !escapes.jobs.is_empty() {
            write_image_escapes(&escapes)?;
        }

        let timeout = tick_rate
            .checked_sub(last_tick.elapsed())
            .unwrap_or_else(|| Duration::from_millis(0));

        if crossterm::event::poll(timeout)? {
            if let CtEvent::Key(key) = crossterm::event::read()? {
                if key.kind == KeyEventKind::Release {
                    continue;
                }
                handle_key(key, app, &driver, &session_id);
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

/// Spawn the config-file watcher for the active session. Returns `None` when
/// the user-config directory cannot be resolved (no `$HOME`, etc.). The
/// theme path is `Some` only when the active theme is a user-supplied file
/// — the baked-in `dark` / `light` palettes have no on-disk source.
fn spawn_config_watcher(app: &App) -> Option<ConfigWatcher> {
    let kb_path = KeybindingsManager::watch_path()?;
    let theme_path = user_theme_path(&app.theme.name);
    ConfigWatcher::spawn(kb_path, theme_path)
}

/// Path the active theme would live at when written to disk. Returns `None`
/// for the baked-in defaults (`dark`, `light`) — those have no source file
/// to watch.
fn user_theme_path(name: &str) -> Option<PathBuf> {
    if matches!(name, "dark" | "light") {
        return None;
    }
    let home = directories::UserDirs::new()?;
    Some(
        home.home_dir()
            .join(".harness")
            .join("themes")
            .join(format!("{name}.toml")),
    )
}

/// Drain any pending reload events emitted by the watcher and apply them in
/// place. A single user save can produce both a keybindings and a theme
/// event; we coalesce the status-line message when both fire in the same
/// poll.
fn drain_config_reloads(app: &mut App, watcher: &ConfigWatcher) {
    let mut got_keybindings = false;
    let mut got_theme = false;
    while let Some(ev) = watcher.try_recv() {
        match ev {
            ConfigReloadEvent::Keybindings => got_keybindings = true,
            ConfigReloadEvent::Theme => got_theme = true,
        }
    }
    if got_keybindings {
        app.keybindings = Arc::new(KeybindingsManager::load());
    }
    if got_theme {
        let name = app.theme.name.clone();
        match Theme::load_named(&name) {
            Ok(t) => app.theme = t,
            Err(e) => {
                app.push_notification(format!("[hot-reload] theme '{name}' reload failed: {e}"));
                // Don't return — still emit the combined status line below.
            }
        }
    }
    if got_keybindings && got_theme {
        app.push_notification("[hot-reload] reloaded keybindings + theme".to_string());
    } else if got_keybindings {
        app.push_notification("[hot-reload] reloaded keybindings".to_string());
    } else if got_theme {
        app.push_notification("[hot-reload] reloaded theme".to_string());
    }
}

fn handle_key(key: KeyEvent, app: &mut App, driver: &Arc<IiiAgentDriver>, session_id: &str) {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let shift = key.modifiers.contains(KeyModifiers::SHIFT);

    // Overlays consume input first.
    if app.tree_visible {
        handle_tree_key(key, app, ctrl, shift);
        return;
    }
    if app.hotkeys_visible {
        handle_hotkeys_key(key, app);
        return;
    }

    // Reset double-Esc latch on any non-Esc key so it can't leak across
    // unrelated keystrokes.
    if !matches!(key.code, KeyCode::Esc) {
        app.reset_esc_latch();
    }

    // Step 1: ask the keybinding manager whether this chord is bound to an
    // action. Cloning the Arc keeps us from borrowing `app` immutably across
    // the dispatch arms below.
    let manager = app.keybindings.clone();
    if let Some(action) = manager.resolve(&key) {
        if dispatch_global_action(action, key, app, driver, session_id) {
            return;
        }
    }

    // Step 2: fall through to plain editor input + cursor navigation. These
    // are *not* configurable via keybindings.json — they're the inherent
    // behaviour of an editor buffer.
    dispatch_editor_input(key, app, ctrl);
}

/// Run an action returned by the manager. Returns `true` when the action was
/// recognised and handled (no further dispatch needed); `false` means the
/// caller should fall through to the editor-input layer.
fn dispatch_global_action(
    action: KeyAction,
    key: KeyEvent,
    app: &mut App,
    driver: &Arc<IiiAgentDriver>,
    session_id: &str,
) -> bool {
    match action {
        KeyAction::OpenHotkeys => {
            app.toggle_hotkeys_overlay();
            true
        }
        KeyAction::AbortOrQuit => {
            if matches!(app.status, AppStatus::Running) {
                app.runtime.abort(&app.session_id);
                app.status = AppStatus::Aborted;
            } else {
                app.should_quit = true;
            }
            true
        }
        KeyAction::ClearScrollback => {
            app.clear_scrollback();
            true
        }
        KeyAction::ToggleTools => {
            app.toggle_tools_collapsed();
            true
        }
        KeyAction::ToggleThinking => {
            app.toggle_expand_thinking();
            true
        }
        KeyAction::Paste => {
            app.paste_from_clipboard();
            true
        }
        KeyAction::DeleteWordBack => {
            app.editor.delete_word_back();
            app.refresh_command_picker();
            app.refresh_file_picker();
            true
        }
        KeyAction::CycleThinkingLevel => {
            app.cycle_thinking_level();
            true
        }
        KeyAction::PickerComplete => {
            if app.command_picker_visible {
                app.complete_slash();
            } else if app.file_picker_visible {
                app.complete_file();
            }
            true
        }
        KeyAction::ScrollUp => {
            app.scroll_up(5);
            true
        }
        KeyAction::ScrollDown => {
            app.scroll_down(5);
            true
        }
        KeyAction::HistoryPrev => {
            // Up arrow is overloaded: picker > multiline editor > history.
            if app.command_picker_visible || app.file_picker_visible {
                app.picker_select_prev();
            } else if app.editor.is_multiline() && !app.editor.cursor_at_first_row() {
                app.editor.move_up();
            } else {
                app.history_prev();
            }
            true
        }
        KeyAction::HistoryNext => {
            if app.command_picker_visible || app.file_picker_visible {
                app.picker_select_next();
            } else if app.editor.is_multiline() && !app.editor.cursor_at_last_row() {
                app.editor.move_down();
            } else {
                app.history_next();
            }
            true
        }
        KeyAction::Escape => {
            if app.maybe_open_tree_on_double_esc() {
                return true;
            }
            app.handle_escape();
            true
        }
        KeyAction::Newline => {
            app.editor.insert_newline();
            app.refresh_command_picker();
            app.refresh_file_picker();
            true
        }
        KeyAction::SubmitFollowup => {
            handle_submit(app, driver.clone(), session_id, /* followup */ true);
            true
        }
        KeyAction::Submit => {
            // Vanilla Enter has overloads (picker complete, inline bash,
            // slash, submit). Defer to the dedicated helper.
            if app.command_picker_visible {
                app.complete_slash();
                app.command_picker_visible = false;
                return true;
            }
            if app.file_picker_visible {
                app.complete_file();
                return true;
            }
            handle_submit(app, driver.clone(), session_id, /* followup */ false);
            true
        }
        // Picker openers + tree-overlay actions don't fire from the global
        // dispatcher: `/` and `@` insert into the editor (and the picker
        // refreshes via `refresh_*_picker`); tree actions are handled in
        // `handle_tree_key`. Returning `false` lets editor-input absorb them.
        KeyAction::PickerOpenCommand
        | KeyAction::PickerOpenFile
        | KeyAction::OpenTree
        | KeyAction::TreeClose
        | KeyAction::TreeFilterCycle
        | KeyAction::TreeBookmark
        | KeyAction::TreeToggleTimestamps
        | KeyAction::TreePivot => {
            let _ = key;
            false
        }
    }
}

/// Common Enter-handling: inline bash, slash, then real submit. Used by both
/// `Submit` and `SubmitFollowup`.
fn handle_submit(app: &mut App, driver: Arc<IiiAgentDriver>, session_id: &str, followup: bool) {
    let raw = app.editor.text();
    if maybe_handle_inline_bash(app, &raw, &driver, session_id, followup) {
        return;
    }
    if maybe_handle_slash(app, &raw) {
        app.editor.clear();
        return;
    }
    let submitted = if followup {
        app.submit_followup()
    } else {
        app.submit_message()
    };
    if let Some(text) = submitted {
        let attachments = app.drain_attachments_as_blocks();
        spawn_run(driver, session_id.to_string(), text, attachments);
        app.status = AppStatus::Running;
    }
}

/// Plain editor-input layer. Runs only when the keybinding manager didn't
/// claim the chord. Cursor navigation, character entry, and editing are not
/// configurable via overrides — they're the buffer's inherent behaviour.
fn dispatch_editor_input(key: KeyEvent, app: &mut App, ctrl: bool) {
    match key.code {
        KeyCode::Char(c) if !ctrl => {
            app.editor.insert_char(c);
            app.refresh_command_picker();
            app.refresh_file_picker();
        }
        KeyCode::Backspace => {
            app.editor.delete_back();
            app.refresh_command_picker();
            app.refresh_file_picker();
        }
        KeyCode::Delete => {
            app.editor.delete_forward();
            app.refresh_command_picker();
            app.refresh_file_picker();
        }
        KeyCode::Left => {
            app.editor.move_left();
            app.refresh_file_picker();
        }
        KeyCode::Right => {
            app.editor.move_right();
            app.refresh_file_picker();
        }
        KeyCode::Home => app.editor.home(),
        KeyCode::End => app.editor.end(),
        // Up/Down/PageUp/PageDown/Esc/Enter/Tab and friends should already
        // have been claimed by the manager via their default chords. Ignored
        // here so a user who unbinds them gets a no-op rather than fall-through
        // editor noise.
        _ => {}
    }
}

/// Tree overlay key handler. Owns search input, filter cycle, bookmark, and
/// pivot-to-top. Like `handle_key`, action chords are looked up via the
/// keybinding manager first; arrow keys + plain char entry fall through.
fn handle_tree_key(key: KeyEvent, app: &mut App, ctrl: bool, _shift: bool) {
    let manager = app.keybindings.clone();
    if let Some(action) = manager.resolve(&key) {
        match action {
            KeyAction::TreeClose | KeyAction::Escape => {
                app.tree_visible = false;
                return;
            }
            KeyAction::TreeFilterCycle => {
                app.cycle_tree_filter();
                return;
            }
            KeyAction::TreeBookmark => {
                app.toggle_tree_bookmark();
                return;
            }
            KeyAction::TreeToggleTimestamps => {
                app.toggle_tree_timestamps();
                return;
            }
            KeyAction::TreePivot | KeyAction::Submit => {
                // Pivot: visual highlight only; no real branching for 0.5.
                // TODO: wire up real session branching when /fork is implemented.
                app.tree_visible = false;
                return;
            }
            KeyAction::HistoryPrev => {
                app.tree_cursor_up();
                return;
            }
            KeyAction::HistoryNext => {
                app.tree_cursor_down();
                return;
            }
            _ => {
                // Other actions (paste, scroll, etc.) don't apply inside the
                // tree overlay. Fall through so plain-input editing can fire
                // (e.g. typing into the search box).
            }
        }
    }

    // Search-input fallback: arrows for cursor movement, plain chars for
    // search filter, Backspace to delete.
    match key.code {
        KeyCode::Up => app.tree_cursor_up(),
        KeyCode::Down => app.tree_cursor_down(),
        KeyCode::Backspace => app.tree_search_pop(),
        KeyCode::Char(c) if !ctrl => app.tree_search_push(c),
        _ => {}
    }
}

/// Hotkeys overlay key handler. Esc closes; arrow keys scroll.
fn handle_hotkeys_key(key: KeyEvent, app: &mut App) {
    match key.code {
        KeyCode::Esc => app.hotkeys_visible = false,
        KeyCode::Up => {
            if app.hotkeys_cursor > 0 {
                app.hotkeys_cursor -= 1;
            }
        }
        KeyCode::Down => {
            app.hotkeys_cursor = app.hotkeys_cursor.saturating_add(1);
        }
        KeyCode::PageUp => {
            app.hotkeys_cursor = app.hotkeys_cursor.saturating_sub(8);
        }
        KeyCode::PageDown => {
            app.hotkeys_cursor = app.hotkeys_cursor.saturating_add(8);
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
    driver: &Arc<IiiAgentDriver>,
    session_id: &str,
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
        spawn_run(driver.clone(), session_id.to_string(), text, Vec::new());
        app.status = AppStatus::Running;
    }
    true
}

/// Spawn the agent loop for one initial prompt on the tokio runtime. The
/// background task triggers `agent::run_loop` over the bus and waits for it
/// to settle; events stream through the engine and are forwarded to the
/// TUI's `App` by the stream subscriber. `attachments` are content blocks
/// (typically images) prepended before the text block on the first user
/// message.
fn spawn_run(
    driver: Arc<IiiAgentDriver>,
    session_id: String,
    prompt: String,
    attachments: Vec<ContentBlock>,
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
        driver.run(&session_id, initial).await;
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

    #[test]
    fn parse_args_default_theme_is_dark() {
        let parsed = parse_args_from(&args(&["go"])).expect("parse ok");
        assert_eq!(parsed.theme, "dark");
    }

    #[test]
    fn parse_args_accepts_theme_flag() {
        let parsed = parse_args_from(&args(&["--theme", "light", "go"])).expect("parse ok");
        assert_eq!(parsed.theme, "light");
    }
}
