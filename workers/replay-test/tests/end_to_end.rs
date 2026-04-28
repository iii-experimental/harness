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
use iii_sdk::{register_worker, InitOptions, RegisterFunctionMessage, TriggerRequest};
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

/// Tier-3 composability check.
///
/// When `llm-router::route` is registered on the bus, `agent::stream_assistant`
/// must call it before dispatching to the provider, and use the returned
/// `{provider, model}` for the actual dispatch. This test stands up a fake
/// router that always rewrites `provider` to `faux` and `model` to a known
/// canned-response key, then drives `agent::run_loop` with a different
/// (provider, model) pair and asserts the canned response shows up in the
/// transcript — proving the router's swap took effect.
///
/// Gated on `IIIX_TEST_ENGINE_URL` for the same reason as `faux_round_trip`.
#[tokio::test]
async fn llm_router_swaps_provider_and_model() -> anyhow::Result<()> {
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

    // Canned response keyed by the model id the router will substitute.
    let routed_model = "router-target";
    provider_faux::register_canned(
        routed_model,
        provider_faux::text_only(
            "hello via router",
            routed_model,
            "faux",
            chrono::Utc::now().timestamp_millis(),
        ),
    );

    // Stand up a fake llm-router::route function. It ignores the input and
    // always replies with the (provider, model) pair we want the runtime to
    // dispatch to.
    let _router = iii.register_function((
        RegisterFunctionMessage::with_id("llm-router::route".to_string())
            .with_description("Test fake: always routes to provider=faux, model=router-target".into()),
        |_payload: Value| async move {
            Ok(json!({ "provider": "faux", "model": "router-target" }))
        },
    ));

    let session_id = format!("e2e-router-{}", chrono::Utc::now().timestamp_millis());
    let prompt = vec![AgentMessage::User(UserMessage {
        content: vec![ContentBlock::Text(TextContent {
            text: "say hi".into(),
        })],
        timestamp: chrono::Utc::now().timestamp_millis(),
    })];

    // Caller asks for a DIFFERENT provider/model. The router should override.
    let resp: Value = iii
        .trigger(TriggerRequest {
            function_id: "agent::run_loop".to_string(),
            payload: json!({
                "session_id": session_id,
                "provider": "anthropic",
                "model": "claude-something",
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
    let any_error = messages.iter().any(|m| {
        matches!(
            m,
            AgentMessage::Assistant(a) if matches!(a.stop_reason, StopReason::Error)
        )
    });
    assert!(
        !any_error,
        "no error — if the router was bypassed we'd dispatch to provider::anthropic which isn't registered"
    );

    // Find the assistant message and assert it carries the routed canned
    // response. If routing didn't happen the loop would have errored above.
    let routed_text = messages.iter().find_map(|m| {
        if let AgentMessage::Assistant(a) = m {
            for block in &a.content {
                if let ContentBlock::Text(t) = block {
                    if t.text.contains("hello via router") {
                        return Some(t.text.clone());
                    }
                }
            }
        }
        None
    });
    assert!(
        routed_text.is_some(),
        "assistant transcript missing the canned 'hello via router' — router did not swap"
    );

    Ok(())
}
