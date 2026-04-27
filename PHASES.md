# Phases

## P0 — loop weekend

Foundational crates and runtime; replay synthetic fixtures byte-equivalent against faux provider.

- harness-types: data shapes
- overflow-classify: provider error pattern catalog
- models-catalog: model capabilities knowledge base
- auth-storage: credential store on iii state
- session-tree (subset): create / load / append / active_path / list
- provider-faux: deterministic test fixture provider
- harness-runtime: 10 functions + 6 inlined built-in tools

Acceptance: 3 fixtures (`tiny-bash-ls`, `tool-batch-parallel`, `steering-mid-run`) produce byte-equivalent AgentEvent streams. CI green. 0.1.0 release.

## P1 — providers + oauth

22 provider crates + 5 oauth crates. Shared `provider-base` for HTTP / auth refresh / surrogate sanitization / error classification / SSE parsing.

Acceptance: each provider streams against real API behind env-var gate. OAuth roundtrip for Anthropic Claude Pro/Max. `cross-provider-handoff` example passes. 0.2.0 release.

## P2 — session ops + compaction

- session-tree (complete): fork / clone / compact / export_html / tree
- context-compaction async stream subscriber
- models-catalog regenerate from provider sources

Acceptance: `compaction-rollover` and `multi-branch-tree` fixtures pass. Auto-compaction transparent to outer loop. 0.3.0 release.

## P3 — corpus + extract

- session-corpus (redact + dataset publish)
- document-extract (PDF / Word text)

Acceptance: roundtrip a fixture session through scan / redact / review / publish. Extract sample PDF + DOCX. 0.4.0 release.

## P4 — PRs against existing repos

Filed once relevant harness crates are stable. `overflow-classify` published to crates.io first.

| PR | Target | Scope |
|---|---|---|
| 1 | `iii-hq/workers/llm-router` | handoff_validate, overflow_classify, error_kind, stream-event taxonomy doc, policy schema fields |
| 2 | `iii-hq/iii/engine` (built-in `shell`) | streaming output, tail-keep truncation, BashOperations backend, kill ladder |
| 3 | `iii-hq/workers/guardrails` | subscribe_harness + 4 default policy bundles |
| 4 | `iii-hq/workers/coding` | install_worker_from_git/npm |
| 5 | `iii-hq/workers/agent` | refactor to consume harness (defer until 0.5+) |
| 6 | `iii-hq/workers/autoharness` | edit-surface migration (defer until 0.5+) |
| 7 | `iii-hq/workers/eval` | replay_harness integration |

## P5 — graduation

Crates with 90+ days production use, no breaking releases for 30+ days, real downstream consumers (iiiterm, autoharness, agent worker) graduate per-crate from `iii-experimental/harness` to `iii-hq/workers/`:

- session-tree → workers/session-tree
- auth-storage → workers/auth-storage
- models-catalog → workers/models
- 22 provider-* → workers/provider-*
- 5 oauth-* → workers/oauth-*
- session-corpus → workers/session-corpus
- document-extract → workers/document-extract

`harness-runtime` and `context-compaction` stay in iii-experimental longer.

## Status

- P0 day 1 in progress (workspace scaffolded; harness-types + overflow-classify next)
