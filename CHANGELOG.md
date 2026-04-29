# Changelog

All notable changes to this project are documented here. This project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html) and is in 0.x; the API surface may change between minor releases.

## [0.11.10] — unreleased

### Added
- `harness --version` / `-V`, `harness-tui --version`, `harnessd --version`. Prints `<binary> <semver>` and exits 0.
- `harness --json` emits one `AgentEvent` JSON per line on stdout, terminating with a `{"type":"summary", ...}` blob carrying `turns`, `messages`, `stop_reason`, `error_message`, `final_text`. Pipe to jq, build dashboards, write golden-trace tests.
- `HARNESS_ENGINE_URL` honored by the CLI as fallback for `--engine-url` (was TUI-only).
- README provider env-var table (22 providers) so users can pick `--provider groq` and immediately see they need `GROQ_API_KEY`.
- README `harnessd` vs `harness` "when to reach for which" preamble.

### Changed
- README leads with the `curl install.sh` recipe; cargo-from-source moved to a collapsed `<details>` block. TTHW for first-time users drops from ~5 min to under 90 s.
- README Quick Start opens with the zero-config faux smoke (`harness --provider faux --model echo`) so users round-trip the full bus path before touching an API key.
- `install.sh` bootstraps `rustup` automatically when `cargo` is absent (override with `HARNESS_SKIP_RUSTUP=1`). Previous behaviour dead-ended with a "install rustup from URL" message.
- Engine connect-failure error in the CLI prints the exact remediation command (`iii --use-default-config &`) and the env var (`HARNESS_ENGINE_URL`) instead of "start the engine, then retry."
- CLI summary prints the assistant's `error_message` when `stop_reason: Error`. Missing API keys, schema rejects, and provider failures used to surface as a silent `last_stop_reason: Error` with no remediation.

## [0.11.9] — 2026-04-29

### Added
- Real-world Tier 4 (cross-process policy enforcement) verified end-to-end. Anthropic + standalone `policy-denylist` binary; bash blocked, model planned around the block. 33 s end-to-end.
- README per-tier `same-process | cross-process` verification matrix on the composability ladder.
- Composability ladder Tier 5 entry (sub-agent + transform_context).

### Changed
- `DEFAULT_HOOK_TIMEOUT_MS` 5 s → 10 s. iii v0.11.x routes pubsub subscribers asynchronously; the old 5 s window expired before cross-process subscribers ran, so `merge_before` saw zero replies and the loop dispatched anyway.
- `agent::stream_assistant` logs `tracing::warn!` when the `router::decide` call fails (was silently swallowed). Surfaces upstream schema rejects.
- `RouterPresenceCache` probe failure now warns instead of silently treating "router absent."

### Fixed
- README Tier 3 quickstart: `iii worker add llm-router` is broken upstream. Replaced with cargo-from-source recipe.

### Upstream
- `iii-hq/workers#58` filed: `RoutingRequest` must drop `deny_unknown_fields` to tolerate iii-sdk's `_caller_worker_id` injection. Merged 2026-04-29.

## [0.11.8] — 2026-04-29

### Added
- `models::register` iii function — write a `Model` to state under `models:<provider>:<id>`. Any caller can extend the catalog at runtime without touching the crate.
- `models_catalog_state_register_round_trip` e2e test.
- 4 new TUI render snapshots: running with tool call, tool execution end, assistant error, wide 120×30. Total 6.

### Changed
- `models-catalog` flips state-first. `models::list/get/supports` read from state via `state::list/state::get`; embedded `data/models.json` is a one-time seed used only when state is empty. Honors the "narrow worker, no hardcoded catalog" rule.
- `iii-hq/workers#57` (llm-router `provider` field) merged upstream; harness comments + README cite the merge commit instead of the open PR.

## [0.11.7] — 2026-04-28

### Added
- 3 new e2e tests: `audit_log_records_after_tool_call`, `dlp_scrubber_rewrites_secret_in_tool_result`, `transform_context_subscriber_mutates_messages`. All three hook topics now have live coverage.
- `oauth_anthropic_register_smoke` e2e — registers `oauth::anthropic::*`, triggers `status`, asserts shape.
- TUI snapshot tests via `insta` (workers/harness-tui/tests/render_snapshot.rs). 2 snapshots: idle screen, after-message.

### Changed
- `policy-subscribers::append_jsonl` serialises writes per path with a process-wide `tokio::sync::Mutex` map. POSIX `O_APPEND` is atomic only up to PIPE_BUF (4 KB); concurrent `after_tool_call` subscribers writing the same audit log used to interleave on >4 KB tool results.
- `RouterPresenceCache` refactored to atomic fast path + double-checked-lock. Concurrent first-callers no longer queue behind the bus probe.

## [0.11.6] — 2026-04-28

### Added
- Sub-agent depth limit (default 3, override via `max_subagent_depth` arg). Encoded in the session-id chain (`root::sub-...::sub-...`).
- `LoopConfig::max_turns` enforcement — was advertised on `agent::run_loop` but the loop core never read it.
- 3 new e2e tests: `policy_denylist_blocks_tool_dispatch`, `hooks_before_and_after_see_tool_calls`, `run_subagent_refuses_at_depth_limit`.
- `provider_faux::tool_call_only` helper — deterministic single-tool-call canned response for hook/policy e2e tests.
- `harness --experimental-providers` flag. Seven providers with unverified endpoint URLs refuse to register without it.

### Fixed
- `policy-subscribers` + `hook-example`: `stream::set` now carries `item_id: uuid::v4()`. iii v0.11.x silently drops writes without `item_id`, which broke before/after/transform reply collection for every cross-process subscriber.
- `IiiRuntime::publish_collect` accepts both bare-array and `{items: [...]}` shapes from `stream::list`.

## [0.11.5] — 2026-04-28

### Added
- CLI: post-loop final `stream::list` drain so tail events between the last poll and the trigger response don't get dropped. `last_index` shared via `Arc<AtomicUsize>` so no double-print.
- `BashTool` host-fallback warning. Single `tracing::warn!` the first time `sandbox::exec` is missing on the bus.

### Fixed
- CLI/TUI `stream::list` shape mismatch. iii v0.11.3 returns a bare array of `data` payloads; the consumer was reading `value.get("items").and_then(Value::as_array)` and silently no-oping. Now accepts both shapes.
- `tool::run_subagent` cap'd at 600 s; `agent::stream_assistant` provider dispatch cap'd at 300 s. Both were inheriting iii-sdk's 30 s default.

## [0.11.4] — 2026-04-28

### Added
- `harness --provider faux --model echo` zero-config smoke path. Auto-installs a canned response on the shared `FauxProvider`; first-time users round-trip the loop without an API key.
- CLI summary prints `last_stop_reason` and `last_text` for the final assistant message. Failures surface immediately at the CLI instead of vanishing into a silent count.

### Fixed
- `agent::run_loop` trigger timeout. iii-sdk 0.11.3 maps `timeout_ms: None` to a 30 s default; multi-turn LLM + tool loops routinely exceed that. CLI and TUI now pass an explicit 600 s.

## [0.11.0] — 2026-04-27

Major cut: full agent surface registered on iii primitives, 3 hook topics, persistent transcripts, 22 providers, 5 OAuth flows, policy subscribers reference workers.

## [0.10.0] — 2026-04-26

`policy-subscribers` reference workers (denylist + audit + DLP). `ARCHITECTURE.md` refreshed for collected pubsub envelope contract.

## [0.9.0] — 2026-04-25

Persistent transcripts via `session::*` iii functions; `harness-cli` and `harness-tui` rewired as thin iii-bus invokers.

## [0.8.0] — 2026-04-24

`harnessd` all-in-one daemon registering every harness worker on the bus in one process.

## [0.7.0] — 2026-04-23

Workspace bumped from 0.6 to 0.7. README refresh; provider crate inventory stabilised at 22.

## [0.1.0] — 2026-04-27

Initial release. Foundational crates and a working agent loop with replay-tested semantics.

### Added

- `harness-types` — shared data shapes (`AgentMessage`, `AgentEvent`, `AgentTool`, `AgentContext`, 11-variant `AssistantMessageEvent`, `ThinkingLevel`, `Transport`, `CacheRetention`, `ErrorKind`, `Usage`).
- `overflow-classify` — 20-pattern provider context-overflow regex catalog plus 3-pattern non-overflow exclusion. `is_overflow()` and `classify_error(text, http_status) -> ErrorKind`.
- `models-catalog` — model capabilities knowledge base. Embedded baseline JSON (10 models across major providers). `list / get / supports`.
- `auth-storage` — pluggable credential store. `CredentialStore` trait, `InMemoryStore`, `resolve_credential` with stored-then-environment priority, per-provider env-var map.
- `session-tree` — parent-id tree of typed entries. `SessionEntry` (Message / CustomMessage / BranchSummary / Compaction), `SessionStore` trait, `InMemoryStore`, `create_session / append_message / active_path / load_messages / load_context`.
- `provider-faux` — deterministic test provider. `StreamProvider` trait, `FauxProvider`, `text_only` canned-response builder.
- `harness-runtime` — agent loop state machine plus six inlined built-in tools (`read`, `write`, `edit`, `ls`, `find`, `grep`) and a bash placeholder. `LoopRuntime` trait abstracts side effects; `MemoryRuntime`, `CapturedEvents`, `EchoTool` back the test suite.
- `fixtures-gen` — synthesises three replay fixtures (`tiny-text`, `tool-batch-parallel`, `steering-mid-run`).
- `replay-test` — replays a fixture through the harness loop, normalises timestamps, and compares the emitted `AgentEvent` stream against a golden JSON.
- `hook-example` — demonstrates `before_tool_call` denylist subscriber blocking a tool call before dispatch.
- Loop semantics: 11 `AgentEvent` variants emitted in spec order; `terminate-batch` rule (all-must-true); `sequential-override` (any forces all); steering injection between batches; follow-up driving a second outer iteration; abort-signal short-circuit; missing-tool yields error result.
- Documentation: `docs/8-functions.md`, `docs/stream-events.md`, `ARCHITECTURE.md`, `PHASES.md`.
- CI: build, format, clippy, test, replay-event verification.

### Notes

- Workers folder layout (`workers/<name>/`) anticipates per-worker graduation to `iii-hq/workers/` once stable.
- iii-engine integration shim is intentionally deferred to 0.2; the runtime exposes `LoopRuntime` so a thin bridge can bind it to engine primitives without touching loop logic.
- Provider crates and OAuth crates land in 0.2.
- Session-tree fork / clone / compact / export-html land in 0.3.
