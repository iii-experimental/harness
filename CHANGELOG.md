# Changelog

All notable changes to this project are documented here. This project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html) and is in 0.x; the API surface may change between minor releases.

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
