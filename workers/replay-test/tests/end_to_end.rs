//! End-to-end integration test against a live iii engine.
//!
//! Why this is gated:
//! `iii-sdk` 0.11 has no in-process engine helper (see
//! `docs/SDK-BLOCKED.md`). Running this test needs a real engine. The test
//! is skipped by default and only runs when `IIIX_TEST_ENGINE_URL` is set
//! to a reachable engine WebSocket URL — for example after the user runs
//! `iii --use-default-config &` in a separate terminal.
//!
//! What it asserts:
//! 1. `harness_runtime::register_with_iii` succeeds against a live engine.
//! 2. `provider_faux::register_with_iii` succeeds and a canned response
//!    can be installed.
//! 3. Triggering `agent::run_loop` against the faux provider returns a
//!    non-error transcript with at least one assistant message.
//!
//! When the SDK ships an in-process spawner, this gate flips off and the
//! test becomes part of the default `cargo test` run.

use harness_types::{AgentMessage, ContentBlock, StopReason, TextContent, UserMessage};
use iii_sdk::{register_worker, InitOptions, TriggerRequest};
use serde_json::{json, Value};

#[tokio::test]
async fn faux_round_trip() -> anyhow::Result<()> {
    let Some(engine_url) = std::env::var("IIIX_TEST_ENGINE_URL").ok() else {
        eprintln!(
            "skipping: set IIIX_TEST_ENGINE_URL to a running iii engine (e.g. ws://127.0.0.1:49134)"
        );
        return Ok(());
    };

    let iii = register_worker(&engine_url, InitOptions::default());
    iii.list_functions()
        .await
        .expect("engine reachable for the duration of the test");

    harness_runtime::register_with_iii(&iii).await?;
    provider_faux::register_with_iii(&iii).await?;

    let key = "e2e-canned";
    provider_faux::register_canned(
        key,
        provider_faux::text_only(
            "hello from faux",
            "test-fixture",
            "faux",
            chrono::Utc::now().timestamp_millis(),
        ),
    );

    let session_id = format!("e2e-{}", chrono::Utc::now().timestamp_millis());
    let prompt = vec![AgentMessage::User(UserMessage {
        content: vec![ContentBlock::Text(TextContent {
            text: "say hi".into(),
        })],
        timestamp: chrono::Utc::now().timestamp_millis(),
    })];

    let resp: Value = iii
        .trigger(TriggerRequest {
            function_id: "agent::run_loop".to_string(),
            payload: json!({
                "session_id": session_id,
                "provider": "faux",
                "model": key,
                "system_prompt": "be brief",
                "messages": prompt,
                "tools": [],
                "max_turns": 2,
            }),
            action: None,
            timeout_ms: None,
        })
        .await
        .expect("agent::run_loop succeeds");

    let messages: Vec<AgentMessage> = serde_json::from_value(
        resp.get("messages")
            .cloned()
            .expect("messages field present"),
    )?;
    assert!(!messages.is_empty(), "transcript not empty");
    let assistant_count = messages
        .iter()
        .filter(|m| matches!(m, AgentMessage::Assistant(_)))
        .count();
    assert!(
        assistant_count >= 1,
        "at least one assistant message in transcript"
    );
    let any_error = messages.iter().any(|m| {
        matches!(
            m,
            AgentMessage::Assistant(a) if matches!(a.stop_reason, StopReason::Error)
        )
    });
    assert!(!any_error, "no assistant message with StopReason::Error");

    Ok(())
}
