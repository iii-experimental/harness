# SDK-blocked items

Harness components that are degraded today because `iii-sdk` 0.11 lacks a primitive we need. Each section names the gap, the file:line where harness routes around it, and what the real fix looks like once the SDK ships the primitive.

## 1. Collected pubsub (publish + collect replies)

**Need.** `publish_collect(topic, payload, timeout) -> Vec<Reply>` — fan-out to subscribers, gather every reply that arrives within the timeout window, return them as a list.

**Today.** `iii.invoke("publish", ...)` is fire-and-forget. Subscriber replies are dropped. Hook fan-out cannot collect responses.

**Where harness routes around it.**

- `workers/harness-runtime/src/register.rs:230-258` — `before_tool_call`, `after_tool_call`, `transform_context` publish fire-and-forget. Hooks receive the event but the loop ignores any reply.
- `workers/harness-iii-bridge/src/hooks.rs` — pure merge logic ready for collected replies. Will be wired into the runtime when the SDK ships the primitive.

**Impact.** Hook subscribers can observe but cannot block tool calls, modify tool results, or rewrite context. The architecture document promises all three; today only the first works.

## 2. Streaming subscription primitive

**Need.** `iii.subscribe(stream_id, handler)` (or `subscribe_stream`) that pushes new entries to a handler as they arrive — no polling.

**Today.** `iii-sdk` exposes `stream::list` (returns the current snapshot) and `stream::set` (append). No push-style subscribe.

**Where harness routes around it.**

- `workers/harness-cli/src/main.rs` (`stream_events` helper, ~lines 220-250) polls `stream::list` on a 200ms tick, dedupes by sequence, prints new entries. `harness-tui` will follow the same pattern when rewired.

**Impact.** 200ms latency floor on agent event display. Higher CPU than necessary because the polling thread runs even when no events are arriving.

## 3. Per-call streaming response

**Need.** A registered iii function that returns a stream of partial results, not just one terminal `Value`.

**Today.** `register_function` returns one `Value` per call. To stream partial assistant tokens, the producer has to `stream::set` to a side-channel and the consumer has to `stream::list` poll it.

**Where harness routes around it.**

- `workers/provider-base/src/iii_register.rs` — `provider::<name>::stream_assistant` drains the underlying `ReceiverStream<AssistantMessageEvent>` into the final `AssistantMessage`. Caller receives one terminal message per provider call.
- Token-by-token streaming surfaces only via the `agent::events/<sid>` stream maintained separately by the runtime.

**Impact.** TUI cannot stream assistant tokens directly from the provider function call. It has to subscribe to the events stream as a separate consumer.

## 4. In-process engine helper for tests

**Need.** `iii_sdk::test_helpers::spawn_in_process_engine() -> III` that boots an engine inside the test process and returns a connected client. Equivalent to what `register_worker` does but against an embedded engine instance.

**Today.** Tests have to either spawn `iii engine` as a subprocess (CI flake risk) or skip end-to-end tests entirely. The integration test in `workers/replay-test` is `#[ignore]`-gated until this lands.

**Where harness routes around it.**

- `workers/replay-test/tests/end_to_end.rs` (when present) — guarded by an env var so it runs only when `IIIX_TEST_ENGINE` points at a live engine.

**Impact.** No hermetic e2e CI for the iii-bus path. Contract drift between runtime router and provider workers (the bug v0.6.0 review caught) is invisible to `cargo test --workspace`.

## 5. Function unregister batch helper

**Need.** Bulk unregister API — given a registration handle, drop every function/topic/stream registered through it in one call.

**Today.** Each `register_function` returns its own `FunctionRef` with `unregister`. Worker shutdown has to track and drop them one by one.

**Where harness routes around it.**

- `workers/session-tree/src/lib.rs`, `workers/session-corpus/src/lib.rs`, `workers/document-extract/src/lib.rs`, `workers/context-compaction/src/lib.rs`, `workers/hook-example/src/lib.rs` — all return per-function `FunctionRefs` structs whose `unregister_all` walks the list manually.

**Impact.** Boilerplate. Not blocking, but every worker writes the same unregister loop.

## Coordinating with iii-engine team

These are the gaps that surface in the harness. There may be others in adjacent projects (agentmemory, llm-router, etc.). Suggest one consolidated tracking issue against `iii-hq/iii` rather than per-gap PRs.
