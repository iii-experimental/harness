//! DLP secret-scrubber subscriber. Connects to a running iii engine,
//! subscribes to `agent::after_tool_call`, redacts matching secret shapes
//! (AWS, OpenAI, GitHub, Stripe, Google) in result text content. Replies
//! with `{ content: [<redacted>] }` so the runtime's `merge_after`
//! overrides the result.
//!
//! Usage:
//!   dlp-scrubber [--engine-url <ws>]
//!
//! Default engine: `ws://127.0.0.1:49134`.

use anyhow::{Context, Result};
use iii_sdk::{register_worker, InitOptions};

const DEFAULT_ENGINE: &str = "ws://127.0.0.1:49134";

#[tokio::main]
async fn main() -> Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let engine_url = parse_args()?;

    let iii = register_worker(&engine_url, InitOptions::default());
    iii.list_functions()
        .await
        .with_context(|| format!("engine unreachable at {engine_url}"))?;

    let _sub = policy_subscribers::subscribe_dlp_scrubber(&iii)
        .map_err(|e| anyhow::anyhow!("subscribe failed: {e}"))?;
    log::info!("dlp-scrubber active on {engine_url}");
    tokio::signal::ctrl_c().await.ok();
    log::info!("shutdown requested");
    Ok(())
}

fn parse_args() -> Result<String> {
    let raw: Vec<String> = std::env::args().skip(1).collect();
    let mut iter = raw.iter();
    let mut engine_url = DEFAULT_ENGINE.to_string();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--engine-url" => {
                engine_url = iter
                    .next()
                    .cloned()
                    .ok_or_else(|| anyhow::anyhow!("--engine-url requires a value"))?;
            }
            other => anyhow::bail!("unknown flag: {other}"),
        }
    }
    Ok(engine_url)
}
