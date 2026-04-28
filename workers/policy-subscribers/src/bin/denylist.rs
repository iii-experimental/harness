//! Standalone denylist policy enforcer. Connects to a running iii engine,
//! subscribes to `agent::before_tool_call`, blocks every call whose name
//! matches a comma-separated denylist from `POLICY_DENIED_TOOLS` (or argv).
//!
//! Usage:
//!   policy-denylist [--engine-url <ws>] [--deny <csv>]
//!
//! Defaults: `ws://127.0.0.1:49134`, denylist from `POLICY_DENIED_TOOLS`
//! env var, falling back to a small built-in list (`bash:rm -rf,sudo`).

use anyhow::{Context, Result};
use iii_sdk::{register_worker, InitOptions};

const DEFAULT_ENGINE: &str = "ws://127.0.0.1:49134";
const DEFAULT_DENYLIST: &str = "bash:rm -rf,sudo,curl-pipe-bash";

#[tokio::main]
async fn main() -> Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let (engine_url, denied) = parse_args()?;
    let iii = register_worker(&engine_url, InitOptions::default());
    iii.list_functions()
        .await
        .with_context(|| format!("engine unreachable at {engine_url}"))?;

    let _sub = policy_subscribers::subscribe_denylist(&iii, denied.clone())
        .map_err(|e| anyhow::anyhow!("subscribe failed: {e}"))?;
    log::info!(
        "policy-denylist active on {engine_url}; denylist=[{}]",
        denied.join(", ")
    );
    tokio::signal::ctrl_c().await.ok();
    log::info!("shutdown requested");
    Ok(())
}

fn parse_args() -> Result<(String, Vec<String>)> {
    let raw: Vec<String> = std::env::args().skip(1).collect();
    let mut iter = raw.iter();
    let mut engine_url = DEFAULT_ENGINE.to_string();
    let mut deny: Option<Vec<String>> = None;
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--engine-url" => {
                engine_url = iter
                    .next()
                    .cloned()
                    .ok_or_else(|| anyhow::anyhow!("--engine-url requires a value"))?;
            }
            "--deny" => {
                let csv = iter
                    .next()
                    .cloned()
                    .ok_or_else(|| anyhow::anyhow!("--deny requires a value"))?;
                deny = Some(csv.split(',').map(str::to_string).collect());
            }
            other => anyhow::bail!("unknown flag: {other}"),
        }
    }
    let deny = deny.unwrap_or_else(|| {
        std::env::var("POLICY_DENIED_TOOLS")
            .unwrap_or_else(|_| DEFAULT_DENYLIST.to_string())
            .split(',')
            .map(str::to_string)
            .collect()
    });
    Ok((engine_url, deny))
}
