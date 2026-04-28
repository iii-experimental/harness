//! All-in-one harness daemon. Connects to a running iii engine and
//! registers every harness worker on the bus in one process — runtime,
//! 22 providers, 5 oauth flows, auth-storage, models-catalog, sessions,
//! corpus, extract, compaction. Replaces the chain of `cargo run -p ...`
//! commands a user would otherwise type for a full demo.
//!
//! Usage:
//!   harnessd serve [--engine-url <ws>] [--providers all|<csv>] [--with-hook-example]
//!   harnessd status [--engine-url <ws>]
//!   harnessd --help
//!
//! Worker shutdown: when the process receives Ctrl-C, it unregisters every
//! function it published before exiting.

use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use iii_sdk::{register_worker, InitOptions, III};

const DEFAULT_ENGINE_URL: &str = "ws://127.0.0.1:49134";

#[tokio::main]
async fn main() -> Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let raw: Vec<String> = std::env::args().skip(1).collect();
    let mut iter = raw.iter();
    let cmd = iter.next().map_or("--help", String::as_str);

    match cmd {
        "serve" => cmd_serve(iter.collect()).await,
        "status" => cmd_status(iter.collect()).await,
        "--help" | "-h" | "help" => {
            print_usage();
            Ok(())
        }
        other => {
            eprintln!("unknown command: {other}");
            print_usage();
            std::process::exit(2);
        }
    }
}

fn print_usage() {
    println!("harnessd — all-in-one iii worker bundle for the harness loop");
    println!();
    println!("USAGE");
    println!("  harnessd serve [--engine-url <ws>] [--providers <csv|all>] [--with-hook-example]");
    println!("  harnessd status [--engine-url <ws>]");
    println!("  harnessd --help");
    println!();
    println!("OPTIONS");
    println!("  --engine-url <ws>      iii engine WebSocket URL (default {DEFAULT_ENGINE_URL})");
    println!("  --providers <csv|all>  provider crates to register (default: all)");
    println!("  --with-hook-example    register the reference hook subscriber that logs traffic");
}

#[derive(Debug)]
struct ServeArgs {
    engine_url: String,
    providers: Vec<String>,
    with_hook_example: bool,
}

fn parse_serve_args(args: Vec<&String>) -> Result<ServeArgs> {
    let mut engine_url = DEFAULT_ENGINE_URL.to_string();
    let mut providers: Option<Vec<String>> = None;
    let mut with_hook_example = false;

    let mut iter = args.into_iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--engine-url" => {
                engine_url.clone_from(
                    iter.next()
                        .ok_or_else(|| anyhow!("--engine-url requires a value"))?,
                );
            }
            "--providers" => {
                let csv = iter
                    .next()
                    .ok_or_else(|| anyhow!("--providers requires a value"))?;
                providers = Some(if csv == "all" {
                    all_provider_names()
                } else {
                    csv.split(',').map(str::to_string).collect()
                });
            }
            "--with-hook-example" => with_hook_example = true,
            other => return Err(anyhow!("unknown serve flag: {other}")),
        }
    }

    Ok(ServeArgs {
        engine_url,
        providers: providers.unwrap_or_else(all_provider_names),
        with_hook_example,
    })
}

async fn cmd_serve(args: Vec<&String>) -> Result<()> {
    let args = parse_serve_args(args)?;
    log::info!("connecting to iii engine at {}", args.engine_url);
    let iii = register_worker(&args.engine_url, InitOptions::default());
    let iii = Arc::new(iii);

    iii.list_functions()
        .await
        .with_context(|| format!("engine unreachable at {}", args.engine_url))?;
    log::info!("engine connection ok");

    harness_runtime::register_with_iii(&iii)
        .await
        .context("harness-runtime register failed")?;
    log::info!("registered: harness-runtime (agent::* + tool::*)");

    let session_store = Arc::new(session_tree::InMemoryStore::default());
    let _session_refs = session_tree::register_with_iii(&iii, session_store);
    log::info!("registered: session-tree (5 session::* fns)");

    let _compaction_refs = context_compaction::register_with_iii(&iii)
        .context("context-compaction register failed")?;
    log::info!("registered: context-compaction");

    let _corpus_refs = session_corpus::register_with_iii(&iii, None);
    log::info!("registered: session-corpus (4 corpus::* fns)");

    let _extract_refs = document_extract::register_with_iii(&iii);
    log::info!("registered: document-extract");

    let auth_store: Arc<dyn auth_storage::CredentialStore> =
        Arc::new(auth_storage::InMemoryStore::new());
    let _auth_refs = auth_storage::register_with_iii(&iii, auth_store)
        .await
        .context("auth-storage register failed")?;
    log::info!("registered: auth-storage (5 auth::* fns)");

    let _models_refs = models_catalog::register_with_iii(&iii)
        .await
        .context("models-catalog register failed")?;
    log::info!("registered: models-catalog (3 models::* fns)");

    if args.with_hook_example {
        let _hook_refs =
            hook_example::register_with_iii(&iii, hook_example::HookExampleConfig::default())
                .context("hook-example register failed")?;
        log::info!("registered: hook-example (3 hook subscribers)");
    }

    register_oauth(&iii).await?;
    register_providers(&iii, &args.providers).await?;

    log::info!("harnessd ready — waiting for requests (Ctrl-C to exit)");
    tokio::signal::ctrl_c().await.ok();
    log::info!("shutdown requested");
    Ok(())
}

async fn cmd_status(args: Vec<&String>) -> Result<()> {
    let mut engine_url = DEFAULT_ENGINE_URL.to_string();
    let mut iter = args.into_iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--engine-url" => {
                engine_url.clone_from(
                    iter.next()
                        .ok_or_else(|| anyhow!("--engine-url requires a value"))?,
                );
            }
            other => return Err(anyhow!("unknown status flag: {other}")),
        }
    }

    let iii = register_worker(&engine_url, InitOptions::default());
    let infos = iii
        .list_functions()
        .await
        .with_context(|| format!("engine unreachable at {engine_url}"))?;

    let mut by_prefix: std::collections::BTreeMap<String, Vec<String>> = Default::default();
    for f in infos {
        let prefix = f
            .function_id
            .split("::")
            .next()
            .unwrap_or("(root)")
            .to_string();
        by_prefix.entry(prefix).or_default().push(f.function_id);
    }
    println!("functions registered on {engine_url}:");
    for (prefix, ids) in by_prefix {
        println!("  [{}] ({} fns)", prefix, ids.len());
        for id in ids {
            println!("    - {id}");
        }
    }
    Ok(())
}

async fn register_oauth(iii: &III) -> Result<()> {
    let _ = oauth_anthropic::register_with_iii(iii)
        .await
        .context("oauth-anthropic register failed")?;
    let _ = oauth_openai_codex::register_with_iii(iii)
        .await
        .context("oauth-openai-codex register failed")?;
    let _ = oauth_github_copilot::register_with_iii(iii)
        .await
        .context("oauth-github-copilot register failed")?;
    let _ = oauth_google_gemini_cli::register_with_iii(iii)
        .await
        .context("oauth-google-gemini-cli register failed")?;
    let _ = oauth_google_antigravity::register_with_iii(iii)
        .await
        .context("oauth-google-antigravity register failed")?;
    log::info!("registered: 5 oauth::* flows");
    Ok(())
}

async fn register_providers(iii: &III, names: &[String]) -> Result<()> {
    for name in names {
        register_one_provider(iii, name)
            .await
            .with_context(|| format!("provider {name} register failed"))?;
    }
    log::info!("registered: {} provider::* workers", names.len());
    Ok(())
}

async fn register_one_provider(iii: &III, name: &str) -> Result<()> {
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
        "faux" => provider_faux::register_with_iii(iii).await,
        other => Err(anyhow!("unknown provider: {other}")),
    }
}

fn all_provider_names() -> Vec<String> {
    vec![
        "anthropic".into(),
        "openai".into(),
        "openai-responses".into(),
        "google".into(),
        "google-vertex".into(),
        "azure-openai".into(),
        "bedrock".into(),
        "openrouter".into(),
        "groq".into(),
        "cerebras".into(),
        "xai".into(),
        "deepseek".into(),
        "mistral".into(),
        "fireworks".into(),
        "kimi-coding".into(),
        "minimax".into(),
        "zai".into(),
        "huggingface".into(),
        "vercel-ai-gateway".into(),
        "opencode-zen".into(),
        "opencode-go".into(),
        "faux".into(),
    ]
}
