# harness

Single-agent loop runtime on [iii-engine](https://iii.dev).

10 functions, 11 stream-event variants, 3 hook topics, 2 message-pull points. Tools register as iii functions. Sub-agents are nested function calls. Hooks fan out via pubsub. State lives on iii state worker. One trace across loop, tools, LLM, sub-agents.

> Status: 0.5.0, 0.x experimental. API surface unstable until production-proven.

## Why

Modern agent harnesses bundle the loop, the tool sandbox, the provider clients, the session storage, and the UI into a single process. Works at small scale, fails at ecosystem scale: tools have to live in the harness's language, hooks are limited to one process, sub-agents become subprocess shells, sessions are local files.

`harness` keeps the loop and nothing else. Every other concern is a worker on the iii bus:

- Provider streaming → 23 narrow `provider-*` workers
- OAuth subscription auth → 5 narrow `oauth-*` workers
- Sessions + forks + HTML export → `session-tree` worker on iii state
- Auto-compaction on overflow → `context-compaction` async stream subscriber
- Session corpus / redact / publish → `session-corpus` worker
- Document text extraction (PDF, DOCX) → `document-extract` worker
- Models catalog → `models-catalog` worker
- Permission policy / DLP / audit → independent subscribers on `agent::before_tool_call`
- Sandbox isolation → existing iii-sandbox worker
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
- **AssistantMessageEvent** — 14 stream variants for incremental assistant output
- **AgentTool** — schema + execute fn
- **3 hook topics** — `before_tool_call`, `after_tool_call`, `transform_context`
- **2 pull points** — `get_steering`, `get_followup`
- **2 semantic rules** — terminate-batch (all-must-true), sequential-override (any forces all)

That is the entire vocabulary. Implementation details (auth, models, providers, storage, sandbox, sub-agents) are workers consumed through iii functions.

## Workspace layout

43 narrow workers under `workers/`:

- `harness-types`, `harness-runtime` — loop, types, run_loop, built-in tools
- `harness-cli` — reference CLI binary
- `harness-tui` — ratatui interactive TUI binary
- `harness-iii-bridge` — `LoopRuntime` impl over iii-engine + sandboxed bash dispatcher
- `provider-base` — shared HTTP/SSE/error infra; OpenAI Chat Completions generic client
- `provider-anthropic`, `provider-openai`, `provider-openai-responses`, `provider-google`, `provider-google-vertex`, `provider-azure-openai`, `provider-bedrock`, `provider-openrouter`, `provider-groq`, `provider-cerebras`, `provider-xai`, `provider-deepseek`, `provider-mistral`, `provider-fireworks`, `provider-kimi-coding`, `provider-minimax`, `provider-zai`, `provider-huggingface`, `provider-vercel-ai-gateway`, `provider-opencode-zen`, `provider-opencode-go`, `provider-faux`
- `oauth-anthropic`, `oauth-openai-codex`, `oauth-github-copilot`, `oauth-google-gemini-cli`, `oauth-google-antigravity` — PKCE + device-code flows for subscription auth
- `auth-storage` — token persistence
- `session-tree`, `session-corpus`, `context-compaction`, `document-extract` — session lifecycle workers
- `models-catalog` — model registry
- `overflow-classify` — provider context-overflow detector (20 patterns)
- `replay-test`, `fixtures-gen`, `hook-example` — test + dev helpers

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

# read-only mode (skip bash tool)
./target/release/harness --no-bash "what are the workspace crates?"

# dispatch bash through iii-engine sandbox microVM (host filesystem isolated)
./target/release/harness --via-iii --engine-url ws://localhost:49134 --sandbox-image python "run uname -a"
```

Built-in tools the agent can call:
- `read`, `write`, `edit` — file ops with diff-style replace
- `ls`, `find`, `grep` — directory walks and substring search
- `bash` — `bash -lc` on host (default) or `iii-sandbox::exec` in microVM (`--via-iii`)

CLI prints AgentEvents as they stream so you can watch the agent reason, call tools, iterate.

## TUI

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

Apache-2.0. v0.5.0 released — see [release notes](https://github.com/iii-experimental/harness/releases/tag/v0.5.0). Specs in repo: `ARCHITECTURE.md`, `PHASES.md`.

## Contributing

- Apache-2.0 only
- No external agent-harness product names in code, comments, commits, or PR text
- Provider names (Anthropic, OpenAI, Google, etc.) are APIs we authenticate against and may be referenced
- No emojis in any committed text
- Commit per concern, not per file
- No Cargo.lock in workspace root (library workspace)

## License

Apache-2.0. See `LICENSE`.
