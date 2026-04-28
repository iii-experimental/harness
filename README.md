# harness

Single-agent loop runtime on [iii-engine](https://iii.dev).

10 loop functions, 11 stream-event variants, 3 hook topics, 2 message-pull points. Tools register as iii functions. Hooks are independent subscribers on `agent::before_tool_call`, `agent::after_tool_call`, and `agent::transform_context`. Sessions, compaction, redaction, and document extraction self-register on the bus.

> Status: 0.7.0, 0.x experimental. API surface unstable until production-proven.

## Why

Modern agent harnesses bundle the loop, the tool sandbox, the provider clients, the session storage, and the UI into a single process. Works at small scale, fails at ecosystem scale: tools have to live in the harness's language, hooks are limited to one process, sub-agents become subprocess shells, sessions are local files.

`harness` keeps the loop and nothing else. Every other concern is a worker on the iii bus:

- Provider streaming → 22 `provider-*` workers (`provider::<name>::stream_assistant`)
- OAuth subscription auth → 5 `oauth-*` workers (`oauth::<name>::{login,refresh,status}`)
- Credential vault → `auth-storage` (`auth::{get,set,delete}_token`, `auth::list_providers`, `auth::status`)
- Models catalog → `models-catalog` (`models::{list,get,supports}`)
- Sessions + forks + HTML export → `session-tree` (5 iii functions: `session::fork`, `session::clone`, `session::compact`, `session::tree`, `session::export_html`)
- Auto-compaction on overflow → `context-compaction` subscribes to `agent::events`, republishes overflow signals to `agent::transform_context`
- Session corpus / redact / publish → `session-corpus` (4 iii functions: `corpus::scan`, `corpus::redact`, `corpus::review`, `corpus::publish`)
- Document text extraction (PDF, DOCX) → `document-extract` (`document::extract`)
- Sub-agents → `tool::run_subagent` invokes `agent::run_loop` recursively with a child session id
- All-in-one bundle → `harnessd serve` registers everything in one process
- Hook subscribers → any worker can `subscribe` on the three hook topics; see `hook-example` for a reference impl
- Sandbox isolation → existing iii-sandbox worker (auto-discovered by `tool::bash`)
- MCP / A2A bridges → existing iii workers

Loop in Rust. Tools in any language. Hot-add capabilities at runtime. One trace through everything.

## Closed vocabulary

- **Worker** — process that registers iii functions
- **Function** — named unit of work
- **Trigger** — what causes a function to run

The loop adds:

- **AgentMessage** — transcript entries (LLM + custom-typed)
- **AgentEvent** — 11 emitted events covering run / turn / message / tool lifecycle
- **AssistantMessageEvent** — 14 stream variants for incremental assistant output
- **AgentTool** — schema + execute fn
- **3 hook topics** — `agent::before_tool_call`, `agent::after_tool_call`, `agent::transform_context`
- **2 pull points** — `get_steering`, `get_followup`
- **2 semantic rules** — terminate-batch (all-must-true), sequential-override (any forces all)

That is the entire vocabulary. Implementation details (auth, models, providers, storage, hooks) are workers consumed through iii functions or pubsub topics.

## Workspace layout

43 narrow workers under `workers/`:

- `harness-types`, `harness-runtime` — loop, types, run_loop, built-in tools
- `harness-cli` — reference CLI binary
- `harness-tui` — ratatui interactive TUI binary
- `harness-runtime::register_with_iii` — registers the 12 `agent::*` functions, 7 `tool::*` functions, and 4 HTTP triggers; `tool::bash` runtime-discovers `sandbox::exec`
- `harness-iii-bridge` — older bridge crate retained for downstream consumers; new wiring lives in `harness-runtime`
- `provider-base` — shared HTTP/SSE/error infra; OpenAI Chat Completions generic client
- `provider-anthropic`, `provider-openai`, `provider-openai-responses`, `provider-google`, `provider-google-vertex`, `provider-azure-openai`, `provider-bedrock`, `provider-openrouter`, `provider-groq`, `provider-cerebras`, `provider-xai`, `provider-deepseek`, `provider-mistral`, `provider-fireworks`, `provider-kimi-coding`, `provider-minimax`, `provider-zai`, `provider-huggingface`, `provider-vercel-ai-gateway`, `provider-opencode-zen`, `provider-opencode-go`, `provider-faux`
- `oauth-anthropic`, `oauth-openai-codex`, `oauth-github-copilot`, `oauth-google-gemini-cli`, `oauth-google-antigravity` — PKCE + device-code flows for subscription auth
- `auth-storage` — credential persistence
- `session-tree` — exposes `register_with_iii(iii, store)` to publish 5 `session::*` iii functions
- `context-compaction` — exposes `register_with_iii(iii)` that subscribes to `agent::events` and republishes overflow signals
- `session-corpus` — exposes `register_with_iii(iii, reviewer)` to publish 4 `corpus::*` iii functions
- `document-extract` — exposes `register_with_iii(iii)` to publish the `document::extract` function
- `hook-example` — live subscriber binary across all 3 hook topics; reference for custom hook authors
- `models-catalog` — model registry
- `overflow-classify` — provider context-overflow detector (20 patterns)
- `replay-test`, `fixtures-gen` — test + dev helpers

## Quick start

```bash
# 1. Boot an iii engine on the default port
iii engine

# 2. Build the harness binaries
cargo build --release --bin harness --bin hook-example

# 3. (Optional) Add the iii-sandbox worker so the agent's bash tool runs in a microVM
iii worker add sandbox

# 4. Run the agent against a real LLM
export ANTHROPIC_API_KEY=sk-ant-...
./target/release/harness "open README.md, summarise it in three sentences, then list every workspace crate using ls."

# 5. (Optional) In a second shell: live-watch hook traffic
III_URL=ws://localhost:49134 ./target/release/hook-example
```

When the iii-sandbox worker is registered, the harness's `bash` tool routes through the sandbox automatically — same tool surface, host filesystem isolated.

## End-to-end demo (real LLM)

```bash
# build
cargo build --release --bin harness

# anthropic (default)
export ANTHROPIC_API_KEY=sk-ant-...
./target/release/harness "open README.md, summarise it in three sentences, then list every workspace crate using ls."

# pick provider + model
./target/release/harness --provider openai --model gpt-4o "say hi"
./target/release/harness --provider groq --model llama-3.3-70b-versatile "say hi"

# add the iii-sandbox worker so bash runs in a microVM (auto-discovered, no flag)
iii worker add sandbox
./target/release/harness "run uname -a"
```

Built-in tools the agent can call:
- `read`, `write`, `edit` — file ops with diff-style replace
- `ls`, `find`, `grep` — directory walks and substring search
- `bash` — `bash -lc` on host, or routed through `iii-sandbox::exec` when the sandbox worker is registered

CLI prints AgentEvents as they stream so you can watch the agent reason, call tools, iterate.

## Hooks

Three pubsub topics. Subscribers are independent workers; the loop fans out and merges responses.

- `agent::before_tool_call` — payload: `{ tool_call }`. Subscribers may return `{ block: bool, reason: str }`.
- `agent::after_tool_call` — payload: `{ tool_call, result }`. Subscribers may return a modified `result`.
- `agent::transform_context` — payload: `{ messages }`. Subscribers may return rewritten `messages`.

Add a custom hook by registering an iii function and binding a `subscribe` trigger to one of the three topics. See `workers/hook-example/src/lib.rs` for a working pattern. Run the binary to log live traffic:

```bash
HOOK_EXAMPLE_DENY=dangerous,rm cargo run --release -p hook-example
```

## TUI

> **0.7.0 caveat**: `harness-tui` still drives the loop in-process (legacy v0.5 path) and has not yet been rewired to run via the iii bus. Use `harness-cli` for an iii-first end-to-end. TUI rewire is tracked for v0.8.

```bash
cargo build --release --bin harness-tui
./target/release/harness-tui --provider anthropic --model claude-sonnet-4-6
```

ratatui interactive UI:
- Multi-line editor with slash commands, `@file` fuzzy attachment, inline bash
- Markdown render with collapsible tool/thinking blocks, queue + spinner indicator
- Native Kitty / iTerm2 inline image render via terminal escape protocols (placeholder fallback elsewhere)
- Clipboard image paste
- `/tree` overlay with parent/child branching glyphs (`├─` `└─` `│`), search, filter, bookmarks
- `/hotkeys` overlay listing every binding
- Themes (dark, light, user-supplied TOML at `~/.harness/themes/<name>.toml`)
- Keybinding overrides at `~/.harness/keybindings.json`
- Hot-reload via `notify` watcher: edit theme or keybindings file, TUI picks up the change live

## Status

Apache-2.0. v0.7.0 — sub-agent tool, oauth/auth/models on the bus, and an `harnessd` all-in-one bundle on top of v0.6.0's iii-first loop. See [release notes](https://github.com/iii-experimental/harness/releases/tag/v0.7.0). Specs in repo: `ARCHITECTURE.md`, `PHASES.md`. Known gaps tracked in [`docs/SDK-BLOCKED.md`](docs/SDK-BLOCKED.md).

`harness-tui` still drives the loop in-process — rewire to the iii bus is the headline item for v0.8. `harness-cli` is the iii-first reference today.

## Contributing

- Apache-2.0 only
- No external agent-harness product names in code, comments, commits, or PR text
- Provider names (Anthropic, OpenAI, Google, etc.) are APIs we authenticate against and may be referenced
- No emojis in any committed text
- Commit per concern, not per file
- No Cargo.lock in workspace root (library workspace)

## License

Apache-2.0. See `LICENSE`.
