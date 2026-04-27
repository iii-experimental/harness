//! Live hook subscriber binary.
//!
//! Connects to an iii engine, registers one subscriber on each of the three
//! hook topics, then idles. Run the harness loop in another process — every
//! tool call and context transform fans out to this binary.
//!
//! Usage:
//!
//! ```text
//! III_URL=ws://localhost:49134 \
//! HOOK_EXAMPLE_DENY=dangerous,rm \
//! cargo run -p hook-example
//! ```
//!
//! Environment:
//! - `III_URL`             — engine WebSocket URL (default: `ws://localhost:49134`)
//! - `HOOK_EXAMPLE_DENY`   — comma-separated tool names to block in
//!   `before_tool_call` (default: `dangerous`)
//! - `HOOK_EXAMPLE_TICK_S` — seconds between counter snapshots (default: `5`)

use std::collections::HashSet;
use std::time::Duration;

use hook_example::{register_with_iii, HookExampleConfig};
use iii_sdk::{register_worker, InitOptions};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .try_init()
        .ok();

    let url = std::env::var("III_URL").unwrap_or_else(|_| "ws://localhost:49134".to_string());
    let denied: HashSet<String> = std::env::var("HOOK_EXAMPLE_DENY")
        .unwrap_or_else(|_| "dangerous".to_string())
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    let tick_s: u64 = std::env::var("HOOK_EXAMPLE_TICK_S")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(5);

    println!("hook-example: connecting to {url}");
    println!("hook-example: denied tools: {denied:?}");
    let iii = register_worker(&url, InitOptions::default());
    let subscribers = register_with_iii(
        &iii,
        HookExampleConfig {
            denied_tools: denied,
        },
    )?;
    println!("hook-example: 3 subscribers registered");

    let counters = subscribers.counters.clone();
    let mut interval = tokio::time::interval(Duration::from_secs(tick_s));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    let shutdown = tokio::signal::ctrl_c();
    tokio::pin!(shutdown);
    loop {
        tokio::select! {
            _ = interval.tick() => {
                let snap = counters.lock().await;
                println!(
                    "before={} (blocked={}) after={} transform={}",
                    snap.before_seen, snap.before_blocked, snap.after_seen, snap.transform_seen,
                );
            }
            _ = &mut shutdown => {
                println!("hook-example: shutting down");
                break;
            }
        }
    }

    subscribers.unregister_all();
    iii.shutdown();
    Ok(())
}
