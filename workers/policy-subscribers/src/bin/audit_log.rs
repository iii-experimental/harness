//! Append-only audit-log subscriber. Connects to a running iii engine,
//! subscribes to `agent::after_tool_call`, writes one JSON object per line
//! to a configurable path. The line shape is
//! `{ ts_ms, tool_call, result }`.
//!
//! Usage:
//!   audit-log [--engine-url <ws>] [--log <path>]
//!
//! Defaults: `ws://127.0.0.1:49134`, `~/.harness/audit.jsonl`.

use std::path::PathBuf;

use anyhow::{Context, Result};
use iii_sdk::{register_worker, InitOptions};

const DEFAULT_ENGINE: &str = "ws://127.0.0.1:49134";

#[tokio::main]
async fn main() -> Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let (engine_url, log_path) = parse_args()?;

    let iii = register_worker(&engine_url, InitOptions::default());
    iii.list_functions()
        .await
        .with_context(|| format!("engine unreachable at {engine_url}"))?;

    let _sub = policy_subscribers::subscribe_audit_log(&iii, log_path.clone())
        .map_err(|e| anyhow::anyhow!("subscribe failed: {e}"))?;
    log::info!(
        "audit-log active on {engine_url}; writing to {}",
        log_path.display()
    );
    tokio::signal::ctrl_c().await.ok();
    log::info!("shutdown requested");
    Ok(())
}

fn parse_args() -> Result<(String, PathBuf)> {
    let raw: Vec<String> = std::env::args().skip(1).collect();
    let mut iter = raw.iter();
    let mut engine_url = DEFAULT_ENGINE.to_string();
    let mut log_path: Option<PathBuf> = None;
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--engine-url" => {
                engine_url = iter
                    .next()
                    .cloned()
                    .ok_or_else(|| anyhow::anyhow!("--engine-url requires a value"))?;
            }
            "--log" => {
                log_path = Some(PathBuf::from(
                    iter.next()
                        .cloned()
                        .ok_or_else(|| anyhow::anyhow!("--log requires a path"))?,
                ));
            }
            other => anyhow::bail!("unknown flag: {other}"),
        }
    }
    let log_path = log_path.unwrap_or_else(|| {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
        PathBuf::from(home).join(".harness").join("audit.jsonl")
    });
    Ok((engine_url, log_path))
}
