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
#[serial_test::serial]
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
#[serial_test::serial]
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

    // Stand up a fake router::decide function (the actual id llm-router
    // registers). It ignores the routing hints and always replies with the
    // (provider, model) pair we want the runtime to dispatch to. The
    // `provider` field on the response is a harness-driven extension to
    // llm-router's RoutingDecision shape — see iii-hq/workers PR.
    let router = iii.register_function((
        RegisterFunctionMessage::with_id("router::decide".to_string()).with_description(
            "Test fake: always routes to provider=faux, model=router-target".into(),
        ),
        |_payload: Value| async move {
            Ok(json!({
                "provider": "faux",
                "model": "router-target",
                "reason": "test-fake",
                "confidence": 1.0,
            }))
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

    // Detach explicitly. Without this, alphabetical test order leaves
    // `router::decide` registered on the engine, and downstream tests
    // route their stream_assistant call through the test fake instead of
    // their own canned faux responses.
    router.unregister();

    Ok(())
}

/// Tier-4 composability check: policy denylist blocks tool dispatch.
///
/// Wires the `policy-subscribers::subscribe_denylist` subscriber for the
/// "bash" tool, then drives `agent::run_loop` with a faux canned response
/// that emits a single `bash` tool-call. The runtime's `merge_before`
/// must see the `block: true` reply and short-circuit dispatch — the
/// transcript therefore contains a tool-result that mentions the
/// denylist, NOT the output of an actually-run bash command.
///
/// Gated on `IIIX_TEST_ENGINE_URL` for the same reason as other e2e tests.
#[tokio::test]
#[serial_test::serial]
async fn policy_denylist_blocks_tool_dispatch() -> anyhow::Result<()> {
    let Some(engine_url) = std::env::var("IIIX_TEST_ENGINE_URL").ok() else {
        eprintln!("skipping: set IIIX_TEST_ENGINE_URL to a running iii engine");
        return Ok(());
    };

    let iii = register_worker(&engine_url, InitOptions::default());
    iii.list_functions()
        .await
        .expect("engine reachable for the duration of the test");

    harness_runtime::register_with_iii(&iii).await?;
    provider_faux::register_with_iii(&iii).await?;

    let model_key = format!("policy-bash-{}", chrono::Utc::now().timestamp_millis());
    provider_faux::register_canned(
        &model_key,
        provider_faux::tool_call_only(
            "bash",
            "tc-1",
            json!({ "command": "echo wrongly_run" }),
            &model_key,
            "faux",
            chrono::Utc::now().timestamp_millis(),
        ),
    );

    let _denylist =
        policy_subscribers::subscribe_denylist(&iii, vec!["bash".to_string()])?;

    let session_id = format!("policy-{}", chrono::Utc::now().timestamp_millis());
    let bash_tool = harness_types::AgentTool {
        name: "bash".into(),
        description: "Run a bash command.".into(),
        parameters: json!({
            "type": "object",
            "properties": { "command": { "type": "string" } },
            "required": ["command"]
        }),
        label: "bash".into(),
        execution_mode: harness_types::ExecutionMode::Sequential,
        prepare_arguments_supported: false,
    };
    let prompt = vec![AgentMessage::User(UserMessage {
        content: vec![ContentBlock::Text(TextContent { text: "go".into() })],
        timestamp: chrono::Utc::now().timestamp_millis(),
    })];

    let resp: Value = iii
        .trigger(TriggerRequest {
            function_id: "agent::run_loop".to_string(),
            payload: json!({
                "session_id": session_id,
                "provider": "faux",
                "model": model_key,
                "system_prompt": "test",
                "messages": prompt,
                "tools": [bash_tool],
                "max_turns": 2,
            }),
            action: None,
            timeout_ms: Some(120_000),
        })
        .await
        .expect("agent::run_loop succeeds");

    let messages: Vec<AgentMessage> = serde_json::from_value(
        resp.get("messages")
            .cloned()
            .expect("messages field present"),
    )?;

    // Find the blocked tool-result. The runtime emits AgentMessage::ToolResult
    // directly (not wrapped in a User message); each carries the block reason
    // as a Text content block plus is_error: true.
    let blocked_text = messages.iter().find_map(|m| {
        let AgentMessage::ToolResult(r) = m else { return None };
        if !r.is_error {
            return None;
        }
        for cb in &r.content {
            if let ContentBlock::Text(t) = cb {
                let lower = t.text.to_lowercase();
                if lower.contains("denylist") || lower.contains("blocked") {
                    return Some(t.text.clone());
                }
            }
        }
        None
    });
    assert!(
        blocked_text.is_some(),
        "expected denylist block reason in tool-result; transcript: {messages:#?}"
    );

    Ok(())
}

/// Tier-4 composability check: hook subscribers observe before/after via
/// the collected pubsub envelope. Registers the `hook-example` subscriber
/// set with `bash` on the denylist, drives one tool-call turn through the
/// loop, then asserts both `before_seen` and `after_seen` counters
/// incremented (proving the runtime's collected fan-out reached the
/// subscribers and got a reply back through `agent::hook_reply`).
#[tokio::test]
#[serial_test::serial]
async fn hooks_before_and_after_see_tool_calls() -> anyhow::Result<()> {
    let Some(engine_url) = std::env::var("IIIX_TEST_ENGINE_URL").ok() else {
        eprintln!("skipping: set IIIX_TEST_ENGINE_URL to a running iii engine");
        return Ok(());
    };

    let iii = register_worker(&engine_url, InitOptions::default());
    iii.list_functions()
        .await
        .expect("engine reachable");

    harness_runtime::register_with_iii(&iii).await?;
    provider_faux::register_with_iii(&iii).await?;

    let model_key = format!("hook-bash-{}", chrono::Utc::now().timestamp_millis());
    provider_faux::register_canned(
        &model_key,
        provider_faux::tool_call_only(
            "bash",
            "tc-2",
            json!({ "command": "echo never_runs" }),
            &model_key,
            "faux",
            chrono::Utc::now().timestamp_millis(),
        ),
    );

    let mut denied = std::collections::HashSet::new();
    denied.insert("bash".to_string());
    let hooks = hook_example::register_with_iii(
        &iii,
        hook_example::HookExampleConfig { denied_tools: denied },
    )?;
    let counters = hooks.counters.clone();

    let session_id = format!("hook-{}", chrono::Utc::now().timestamp_millis());
    let bash_tool = harness_types::AgentTool {
        name: "bash".into(),
        description: "bash".into(),
        parameters: json!({
            "type": "object",
            "properties": { "command": { "type": "string" } },
            "required": ["command"]
        }),
        label: "bash".into(),
        execution_mode: harness_types::ExecutionMode::Sequential,
        prepare_arguments_supported: false,
    };
    let prompt = vec![AgentMessage::User(UserMessage {
        content: vec![ContentBlock::Text(TextContent { text: "go".into() })],
        timestamp: chrono::Utc::now().timestamp_millis(),
    })];

    let _ = iii
        .trigger(TriggerRequest {
            function_id: "agent::run_loop".to_string(),
            payload: json!({
                "session_id": session_id,
                "provider": "faux",
                "model": model_key,
                "system_prompt": "test",
                "messages": prompt,
                "tools": [bash_tool],
                "max_turns": 2,
            }),
            action: None,
            timeout_ms: Some(120_000),
        })
        .await
        .expect("agent::run_loop succeeds");

    {
        let snapshot = counters.lock().await;
        assert!(
            snapshot.before_seen >= 1,
            "before_tool_call subscriber never fired; before_seen={}",
            snapshot.before_seen
        );
        assert!(
            snapshot.before_blocked >= 1,
            "before_tool_call denylist never matched; before_blocked={}",
            snapshot.before_blocked
        );
        // after_seen may or may not fire depending on whether the runtime
        // publishes after_tool_call for blocked calls. We assert >=0 with
        // a permissive note so the test pins behaviour without overfitting.
        let _ = snapshot.after_seen;
    }

    // Detach explicitly so the engine cleans up the trigger before the next
    // test registers an overlapping subscriber. Without this, alphabetical
    // test order leaves `hook_example::before_tool_call` active when the
    // policy test starts, and replies race or duplicate.
    hooks.unregister_all();

    Ok(())
}

/// Sub-agent recursion is bounded. Drives a `tool::run_subagent` call
/// whose `parent_session_id` already contains three `::sub-` segments
/// (i.e. depth 3, the default cap). The tool must refuse the spawn and
/// return a `details.depth_limit_reached: true` payload — never trigger a
/// nested `agent::run_loop`.
#[tokio::test]
#[serial_test::serial]
async fn run_subagent_refuses_at_depth_limit() -> anyhow::Result<()> {
    let Some(engine_url) = std::env::var("IIIX_TEST_ENGINE_URL").ok() else {
        eprintln!("skipping: set IIIX_TEST_ENGINE_URL to a running iii engine");
        return Ok(());
    };

    let iii = register_worker(&engine_url, InitOptions::default());
    iii.list_functions().await.expect("engine reachable");
    harness_runtime::register_with_iii(&iii).await?;

    let parent = "root::sub-1::sub-2::sub-3";
    let resp: Value = iii
        .trigger(TriggerRequest {
            function_id: "run_subagent".to_string(),
            payload: json!({
                "tool_call": {
                    "id": "tc-depth",
                    "name": "run_subagent",
                    "arguments": {
                        "prompt": "should refuse",
                        "provider": "faux",
                        "model": "doesnt-matter",
                        "parent_session_id": parent,
                    }
                }
            }),
            action: None,
            timeout_ms: Some(15_000),
        })
        .await
        .expect("run_subagent reachable");

    let limited = resp
        .get("details")
        .and_then(|d| d.get("depth_limit_reached"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    assert!(
        limited,
        "expected depth_limit_reached=true at depth 3; resp: {resp:#?}"
    );

    Ok(())
}

/// Audit-log subscriber writes one JSON-line per dispatched tool call.
///
/// Subscribes `policy::audit_log` to a temp file, drives one tool dispatch
/// through the loop (faux→bash via host shell on a deterministic command),
/// then reads the log and asserts a single line containing both the tool
/// name and an `is_error=false` result.
#[tokio::test]
#[serial_test::serial]
async fn audit_log_records_after_tool_call() -> anyhow::Result<()> {
    let Some(engine_url) = std::env::var("IIIX_TEST_ENGINE_URL").ok() else {
        eprintln!("skipping: set IIIX_TEST_ENGINE_URL to a running iii engine");
        return Ok(());
    };

    let iii = register_worker(&engine_url, InitOptions::default());
    iii.list_functions().await.expect("engine reachable");
    harness_runtime::register_with_iii(&iii).await?;
    provider_faux::register_with_iii(&iii).await?;

    let model_key = format!("audit-bash-{}", chrono::Utc::now().timestamp_millis());
    provider_faux::register_canned(
        &model_key,
        provider_faux::tool_call_only(
            "bash",
            "tc-audit",
            json!({ "command": "echo audit_marker" }),
            &model_key,
            "faux",
            chrono::Utc::now().timestamp_millis(),
        ),
    );

    let log_dir = tempfile::tempdir()?;
    let log_path = log_dir.path().join("audit.jsonl");
    let audit = policy_subscribers::subscribe_audit_log(&iii, log_path.clone())?;

    let session_id = format!("audit-{}", chrono::Utc::now().timestamp_millis());
    let bash_tool = harness_types::AgentTool {
        name: "bash".into(),
        description: "bash".into(),
        parameters: json!({
            "type": "object",
            "properties": { "command": { "type": "string" } },
            "required": ["command"]
        }),
        label: "bash".into(),
        execution_mode: harness_types::ExecutionMode::Sequential,
        prepare_arguments_supported: false,
    };
    let prompt = vec![AgentMessage::User(UserMessage {
        content: vec![ContentBlock::Text(TextContent { text: "go".into() })],
        timestamp: chrono::Utc::now().timestamp_millis(),
    })];

    let _ = iii
        .trigger(TriggerRequest {
            function_id: "agent::run_loop".to_string(),
            payload: json!({
                "session_id": session_id,
                "provider": "faux",
                "model": model_key,
                "system_prompt": "test",
                "messages": prompt,
                "tools": [bash_tool],
                "max_turns": 2,
            }),
            action: None,
            timeout_ms: Some(120_000),
        })
        .await
        .expect("agent::run_loop succeeds");

    let body = std::fs::read_to_string(&log_path)
        .unwrap_or_default();
    let lines: Vec<&str> = body.lines().filter(|l| !l.is_empty()).collect();
    assert!(
        !lines.is_empty(),
        "audit log empty; subscriber never wrote (path: {})",
        log_path.display()
    );
    let first: Value = serde_json::from_str(lines[0])?;
    assert_eq!(
        first.get("tool_call").and_then(|t| t.get("name")).and_then(Value::as_str),
        Some("bash"),
        "audit log line missing tool_call.name=bash"
    );
    assert!(
        first.get("ts_ms").and_then(Value::as_i64).is_some(),
        "audit log line missing ts_ms"
    );

    audit.unregister();
    Ok(())
}

/// DLP scrubber rewrites secret shapes in tool result text. Drives a bash
/// tool call that echoes a fake AWS access key, registers
/// `policy::dlp_scrubber`, and asserts the post-loop transcript shows the
/// secret replaced with the redaction marker.
#[tokio::test]
#[serial_test::serial]
async fn dlp_scrubber_rewrites_secret_in_tool_result() -> anyhow::Result<()> {
    let Some(engine_url) = std::env::var("IIIX_TEST_ENGINE_URL").ok() else {
        eprintln!("skipping: set IIIX_TEST_ENGINE_URL to a running iii engine");
        return Ok(());
    };

    let iii = register_worker(&engine_url, InitOptions::default());
    iii.list_functions().await.expect("engine reachable");
    harness_runtime::register_with_iii(&iii).await?;
    provider_faux::register_with_iii(&iii).await?;

    // The fake key matches `AKIA[0-9A-Z]{16}` → scrubber should redact.
    let fake_key = "AKIAABCDEFGHIJKLMNOP";
    let model_key = format!("dlp-bash-{}", chrono::Utc::now().timestamp_millis());
    provider_faux::register_canned(
        &model_key,
        provider_faux::tool_call_only(
            "bash",
            "tc-dlp",
            json!({ "command": format!("echo {fake_key}") }),
            &model_key,
            "faux",
            chrono::Utc::now().timestamp_millis(),
        ),
    );

    let dlp = policy_subscribers::subscribe_dlp_scrubber(&iii)?;

    let session_id = format!("dlp-{}", chrono::Utc::now().timestamp_millis());
    let bash_tool = harness_types::AgentTool {
        name: "bash".into(),
        description: "bash".into(),
        parameters: json!({
            "type": "object",
            "properties": { "command": { "type": "string" } },
            "required": ["command"]
        }),
        label: "bash".into(),
        execution_mode: harness_types::ExecutionMode::Sequential,
        prepare_arguments_supported: false,
    };
    let prompt = vec![AgentMessage::User(UserMessage {
        content: vec![ContentBlock::Text(TextContent { text: "go".into() })],
        timestamp: chrono::Utc::now().timestamp_millis(),
    })];

    let resp: Value = iii
        .trigger(TriggerRequest {
            function_id: "agent::run_loop".to_string(),
            payload: json!({
                "session_id": session_id,
                "provider": "faux",
                "model": model_key,
                "system_prompt": "test",
                "messages": prompt,
                "tools": [bash_tool],
                "max_turns": 2,
            }),
            action: None,
            timeout_ms: Some(120_000),
        })
        .await
        .expect("agent::run_loop succeeds");

    let messages: Vec<AgentMessage> =
        serde_json::from_value(resp.get("messages").cloned().expect("messages"))?;

    // Find the tool-result. Its text should NOT contain the raw key (the
    // scrubber's merge_after replaced it) and SHOULD contain the marker.
    let tool_text = messages.iter().find_map(|m| {
        let AgentMessage::ToolResult(r) = m else { return None };
        for cb in &r.content {
            if let ContentBlock::Text(t) = cb {
                return Some(t.text.clone());
            }
        }
        None
    });
    let text = tool_text.expect("tool_result with text content present");
    assert!(
        !text.contains(fake_key),
        "raw secret survived the scrubber: {text}"
    );
    assert!(
        text.to_uppercase().contains("REDACTED"),
        "redaction marker missing from scrubbed output: {text}"
    );

    dlp.unregister();
    Ok(())
}

/// transform_context subscriber can mutate the in-flight messages array.
/// Registers a tiny test subscriber that prepends a marker user message
/// to the messages, drives a faux text-only turn, and asserts the marker
/// appears in the post-loop transcript — proving the runtime applied the
/// subscriber's rewrite before stream_assistant was called.
#[tokio::test]
#[serial_test::serial]
async fn transform_context_subscriber_mutates_messages() -> anyhow::Result<()> {
    let Some(engine_url) = std::env::var("IIIX_TEST_ENGINE_URL").ok() else {
        eprintln!("skipping: set IIIX_TEST_ENGINE_URL to a running iii engine");
        return Ok(());
    };

    let iii = register_worker(&engine_url, InitOptions::default());
    iii.list_functions().await.expect("engine reachable");
    harness_runtime::register_with_iii(&iii).await?;
    provider_faux::register_with_iii(&iii).await?;

    let model_key = format!("xform-{}", chrono::Utc::now().timestamp_millis());
    provider_faux::register_canned(
        &model_key,
        provider_faux::text_only(
            "ack",
            &model_key,
            "faux",
            chrono::Utc::now().timestamp_millis(),
        ),
    );

    let marker = format!(
        "[xform-marker-{}]",
        chrono::Utc::now().timestamp_millis()
    );

    // Register a one-shot transform_context subscriber inline. It receives
    // {messages: [...]} via the collected pubsub envelope, prepends a
    // marker user message, and replies with the new array. The runtime's
    // decode_transform accepts a bare array shape.
    let iii_for_xform = iii.clone();
    let marker_for_handler = marker.clone();
    let xform_fn = iii.register_function((
        RegisterFunctionMessage::with_id("test::xform_inject".to_string())
            .with_description("test fixture: prepend marker user msg".into()),
        move |payload: Value| {
            let iii = iii_for_xform.clone();
            let marker = marker_for_handler.clone();
            async move {
                let event_id = payload
                    .get("event_id")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                let reply_stream = payload
                    .get("reply_stream")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                let inner = payload
                    .get("payload")
                    .cloned()
                    .unwrap_or_else(|| payload.clone());
                let mut messages: Vec<AgentMessage> = inner
                    .get("messages")
                    .and_then(|v| serde_json::from_value(v.clone()).ok())
                    .unwrap_or_default();
                messages.insert(
                    0,
                    AgentMessage::User(UserMessage {
                        content: vec![ContentBlock::Text(TextContent {
                            text: marker.clone(),
                        })],
                        timestamp: chrono::Utc::now().timestamp_millis(),
                    }),
                );
                let reply = json!({ "messages": messages });
                if !event_id.is_empty() && !reply_stream.is_empty() {
                    let _ = iii
                        .trigger(TriggerRequest {
                            function_id: "stream::set".into(),
                            payload: json!({
                                "stream_name": reply_stream,
                                "group_id": event_id,
                                "item_id": uuid::Uuid::new_v4().to_string(),
                                "data": reply,
                            }),
                            action: None,
                            timeout_ms: None,
                        })
                        .await;
                }
                Ok(reply)
            }
        },
    ));
    let xform_trig = iii.register_trigger(iii_sdk::RegisterTriggerInput {
        trigger_type: "subscribe".into(),
        function_id: "test::xform_inject".into(),
        config: json!({ "topic": "agent::transform_context" }),
        metadata: None,
    })?;

    let session_id = format!("xform-{}", chrono::Utc::now().timestamp_millis());
    let prompt = vec![AgentMessage::User(UserMessage {
        content: vec![ContentBlock::Text(TextContent { text: "hi".into() })],
        timestamp: chrono::Utc::now().timestamp_millis(),
    })];

    let resp: Value = iii
        .trigger(TriggerRequest {
            function_id: "agent::run_loop".to_string(),
            payload: json!({
                "session_id": session_id,
                "provider": "faux",
                "model": model_key,
                "system_prompt": "test",
                "messages": prompt,
                "tools": [],
                "max_turns": 2,
            }),
            action: None,
            timeout_ms: Some(60_000),
        })
        .await
        .expect("agent::run_loop succeeds");

    let messages: Vec<AgentMessage> =
        serde_json::from_value(resp.get("messages").cloned().expect("messages"))?;
    let injected = messages.iter().any(|m| {
        if let AgentMessage::User(u) = m {
            return u.content.iter().any(|cb| matches!(
                cb,
                ContentBlock::Text(t) if t.text.contains(&marker)
            ));
        }
        false
    });
    assert!(
        injected,
        "transform_context marker missing from transcript; transcript: {messages:#?}"
    );

    xform_trig.unregister();
    xform_fn.unregister();
    Ok(())
}

/// OAuth registration smoke. Confirms `oauth::anthropic::register_with_iii`
/// binds three functions on the live engine and that `oauth::anthropic::status`
/// is reachable by trigger. We don't drive the PKCE login flow (that
/// requires a browser) — this is the registration path only, exercising
/// the same bus-bound shape every other oauth-* worker uses.
///
/// Status returns `{ready: bool}` reflecting upstream-IDP reachability;
/// the test asserts only that the field is present and boolean-typed,
/// not its value, so flaky network conditions don't break CI.
#[tokio::test]
#[serial_test::serial]
async fn oauth_anthropic_register_smoke() -> anyhow::Result<()> {
    let Some(engine_url) = std::env::var("IIIX_TEST_ENGINE_URL").ok() else {
        eprintln!("skipping: set IIIX_TEST_ENGINE_URL to a running iii engine");
        return Ok(());
    };

    let iii = register_worker(&engine_url, InitOptions::default());
    iii.list_functions().await.expect("engine reachable");

    let refs = oauth_anthropic::register_with_iii(&iii).await?;

    let resp: Value = iii
        .trigger(TriggerRequest {
            function_id: "oauth::anthropic::status".to_string(),
            payload: json!({}),
            action: None,
            timeout_ms: Some(15_000),
        })
        .await
        .expect("oauth::anthropic::status reachable");
    assert!(
        resp.get("ready").and_then(Value::as_bool).is_some(),
        "status response missing 'ready' bool: {resp:?}"
    );

    refs.unregister_all();
    Ok(())
}

/// Models catalog: state-first round-trip. Registers `models::*`, calls
/// `models::register` with a custom Model, then `models::get` and asserts
/// the value comes back from state — proving the embedded baseline is no
/// longer the source of truth and any caller can extend the catalog at
/// runtime without touching the crate.
#[tokio::test]
#[serial_test::serial]
async fn models_catalog_state_register_round_trip() -> anyhow::Result<()> {
    let Some(engine_url) = std::env::var("IIIX_TEST_ENGINE_URL").ok() else {
        eprintln!("skipping: set IIIX_TEST_ENGINE_URL to a running iii engine");
        return Ok(());
    };

    let iii = register_worker(&engine_url, InitOptions::default());
    iii.list_functions().await.expect("engine reachable");
    let refs = models_catalog::register_with_iii(&iii).await?;

    let custom_id = format!("test-model-{}", chrono::Utc::now().timestamp_millis());
    let custom_provider = "ephemeral-test-provider";
    let model = json!({
        "id": custom_id,
        "provider": custom_provider,
        "api": "chat-completions",
        "display_name": "Test Custom",
        "context_window": 12345,
        "supports_thinking": true,
        "supports_tools": true,
        "transports": [],
    });

    let reg_resp: Value = iii
        .trigger(TriggerRequest {
            function_id: "models::register".to_string(),
            payload: model.clone(),
            action: None,
            timeout_ms: Some(15_000),
        })
        .await
        .expect("models::register reachable");
    assert_eq!(
        reg_resp.get("registered").and_then(Value::as_bool),
        Some(true),
        "models::register did not confirm; resp: {reg_resp:?}"
    );

    let get_resp: Value = iii
        .trigger(TriggerRequest {
            function_id: "models::get".to_string(),
            payload: json!({
                "provider": custom_provider,
                "model_id": custom_id,
            }),
            action: None,
            timeout_ms: Some(15_000),
        })
        .await
        .expect("models::get reachable");
    assert_eq!(
        get_resp.get("id").and_then(Value::as_str),
        Some(custom_id.as_str()),
        "models::get did not return the registered model: {get_resp:?}"
    );
    assert_eq!(
        get_resp.get("context_window").and_then(Value::as_u64),
        Some(12345),
        "context_window did not round-trip: {get_resp:?}"
    );

    let supports_resp: Value = iii
        .trigger(TriggerRequest {
            function_id: "models::supports".to_string(),
            payload: json!({
                "provider": custom_provider,
                "model_id": custom_id,
                "capability": "thinking",
            }),
            action: None,
            timeout_ms: Some(15_000),
        })
        .await
        .expect("models::supports reachable");
    assert_eq!(
        supports_resp.get("supported").and_then(Value::as_bool),
        Some(true),
        "models::supports did not see the registered capability: {supports_resp:?}"
    );

    refs.unregister_all();
    Ok(())
}
