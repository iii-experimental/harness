# harness

Single-agent loop runtime on [iii-engine](https://iii.dev).

10 functions, 11 stream-event variants, 3 hook topics, 2 message-pull points. Tools register as iii functions. Sub-agents are nested function calls. Hooks fan out via pubsub. State lives on iii state worker. One trace across loop, tools, LLM, sub-agents.

> Status: 0.x experimental. API surface is unstable until production-proven.

## Why

Modern agent harnesses bundle the loop, the tool sandbox, the provider clients, the session storage, and the UI into a single process. That works at small scale and falls apart at ecosystem scale: tools have to live in the harness's language, hooks are limited to one process, sub-agents become subprocess shells, sessions are local files.

`harness` keeps the loop and nothing else. Every other concern is a worker on the iii bus:

- Provider streaming → 22 narrow `provider-*` workers
- OAuth subscription auth → 5 narrow `oauth-*` workers
- Sessions → `session-tree` worker on iii state
- Models catalog → `models-catalog` worker
- Compaction → `context-compaction` async stream subscriber
- Permission policy / DLP / audit → independent subscribers on `agent::before_tool_call`
- Sandbox isolation → existing iii sandbox worker
- MCP / A2A bridges → existing iii workers
- Sub-agent spawn → nested `agent::run_loop` invocation, parent-child trace

Loop in Rust. Tools in any language. Hot-add capabilities at runtime. One trace through everything.

## Closed vocabulary

- **Worker** — process that registers iii functions
- **Function** — named unit of work
- **Trigger** — what causes a function to run

The loop adds:

- **AgentMessage** — transcript entries (LLM + custom-typed)
- **AgentEvent** — 11 emitted events covering run / turn / message / tool lifecycle
- **AgentTool** — schema + execute fn
- **3 hook topics** — `before_tool_call`, `after_tool_call`, `transform_context`
- **2 pull points** — `get_steering`, `get_followup`
- **2 semantic rules** — terminate-batch (all-must-true), sequential-override (any forces all)

That is the entire vocabulary. Implementation details (auth, models, providers, storage, sandbox, sub-agents) are workers consumed through iii functions.

## Status

Apache-2.0. Not yet published. Specs in repo: `ARCHITECTURE.md`, `PHASES.md`.

## Contributing

Per project conventions:

- Apache-2.0 only
- No external agent-harness product names in code, comments, commits, or PR text
- Provider names (Anthropic, OpenAI, Google, etc.) are APIs we authenticate against and may be referenced
- No emojis in any committed text
- Commit per concern, not per file
- No Cargo.lock in workspace root (library workspace)

## License

Apache-2.0. See `LICENSE`.
