# harness

Single-agent loop runtime on [iii-engine](https://iii.dev). 44 narrow workers, all iii-first.

> Status: 0.11.0, 0.x experimental. API surface unstable until production-proven.

## Install

```bash
# 1. iii engine (prerequisite)
curl -fsSL https://install.iii.dev/iii/main/install.sh | sh

# 2. harness (from source — crates.io publish lands at v1.0)
git clone https://github.com/iii-experimental/harness && cd harness
cargo build --release --bin harness --bin harness-tui --bin harnessd
```

Or, once shipped, use the harness installer that wraps both:

```bash
curl -fsSL https://raw.githubusercontent.com/iii-experimental/harness/main/install.sh | sh
```

## Quick start

```bash
# 1. Boot an iii engine
iii --use-default-config &

# 2. Run the agent
export ANTHROPIC_API_KEY=sk-ant-...
./target/release/harness "summarise this repo and list workspace crates using ls."
```

You should see the agent stream events as it reads the README, calls `ls` on `workers/`, and produces a final summary.

## Composability ladder

The whole point of harness: every capability is one `iii worker add` away. Watch the agent gain power without touching harness code.

```bash
# Tier 1 — baseline
./target/release/harness "run uname -a"
# → tool::bash spawns a host child process. Plain.

# Tier 2 — sandboxed bash (microVM isolation)
iii worker add sandbox
./target/release/harness "run uname -a"
# → tool::bash auto-routes through sandbox::exec. Host filesystem untouched.
# → No flag changed in harness. Bus is the source of truth.

# Tier 3 — smart provider routing (production deployment shape)
iii worker add llm-router
./target/release/harness "say hi"
# → agent::stream_assistant calls llm-router::route first when registered.
# → The router can swap provider/model based on capabilities, cost, fallbacks.
# → When llm-router isn't on the bus, harness dispatches directly to the
#   configured provider. No flag, no config — bus presence decides.

# Tier 4 — policy enforcement
cargo run --release --bin policy-denylist -- --deny "bash:rm -rf,sudo" &
./target/release/harness "delete all logs"
# → before_tool_call fan-out. policy-denylist replies { block: true, reason }.
# → Loop blocks the call. Agent sees the block, plans around it.
```

Each tier = one worker addition. No code change in harness. This is iii primitives + narrow workers in practice.

## Architecture

The agent loop is a state machine. Every concern outside the state machine is a worker on the iii bus. Both reference binaries (`harness-cli`, `harness-tui`) are thin invokers — they connect to a running iii engine, register the runtime + a provider, trigger `agent::run_loop`, and consume the per-session events stream.

```
                                                    +---------------------------+
                                                    |       iii engine          |
                                                    |   (ws://localhost:49134)  |
                                                    +-------------+-------------+
                                                                  |
                                                                  | trigger/publish/subscribe
              +---------------+         +-----------------+       |       +----------------------+
              |  harness-cli  |---+     |  harness-tui    |---+   |   +---|  policy-denylist     |
              |   (thin)      |   |     |   (ratatui)     |   |   |   |   |  audit-log           |
              +-------+-------+   |     +--------+--------+   |   |   |   |  dlp-scrubber        |
                      |           |              |            |   |   |   |  hook-example        |
                      | trigger   |   subscribe  |   trigger  |   |   |   +----------+-----------+
                      | run_loop  |   events     |   run_loop |   |   |              | subscribe
                      v           |              v            |   v   v              v
        +-------------+-----------+--------------+------------+---+---+--+
        |                          iii bus                                |
        +---+--------+--------+--------+--------+--------+--------+-------+
            |        |        |        |        |        |        |
            v        v        v        v        v        v        v
     +-------+ +-------+ +-------+ +-------+ +-------+ +-------+ +-------+
     |runtime| |provider| |oauth | |session| |compact| |corpus | |models |
     |worker | | x22    | | x5   | | tree  | | -ion  | |       | |catalog|
     +-------+ +-------+ +-------+ +-------+ +-------+ +-------+ +-------+
       12 agent::*    provider::    oauth::*  session::  subscriber  corpus::  models::
       8 tool::*      <name>::      login/    fork/      on agent::  scan/     list/
       4 HTTP         stream_       refresh/  clone/     events ->   redact/   get/
       triggers       assistant     status    compact/   transform_  review/   supports
                                              tree/      context     publish
                                              export_html
                                              create/append/messages

                          +-----------------+        +----------------+
                          |  iii-sandbox    |        |  document-     |
                          |  (auto-route    |        |  extract       |
                          |   bash)         |        |                |
                          +-----------------+        +----------------+
                                  ^                          ^
                                  | tool::bash discovers     | document::extract
                                  +--------------------------+

       Streams the runtime publishes:
         agent::events/<sid>        11 AgentEvent variants
         agent::hook_reply/<eid>    collected-pubsub replies

       Pubsub topics the runtime publishes on:
         agent::before_tool_call    collect, first-block-wins
         agent::after_tool_call     collect, field-by-field merge
         agent::transform_context   collect, last-reply-wins

       State (scope `agent`):
         session/<id>/steering | followup | abort_signal
```

## Why

Modern agent harnesses bundle the loop, the tool sandbox, the provider clients, the session storage, and the UI into one process. Works at small scale, fails at ecosystem scale: tools have to live in the harness's language, hooks are limited to one process, sub-agents become subprocess shells, sessions are local files.

`harness` keeps the loop and nothing else. Every other concern is a worker on the iii bus.

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

## CLI

```bash
# anthropic (default)
export ANTHROPIC_API_KEY=sk-ant-...
./target/release/harness "list every workspace crate using ls."

# pick provider + model
./target/release/harness --provider openai --model gpt-4o "say hi"
./target/release/harness --provider groq --model llama-3.3-70b-versatile "say hi"
```

Built-in tools the agent can call:
- `read`, `write`, `edit` — file ops with diff-style replace
- `ls`, `find`, `grep` — directory walks and substring search
- `bash` — `bash -lc` on host, or routed through `iii-sandbox::exec` when the sandbox worker is registered
- `run_subagent` — spawn a focused sub-agent for a subtask, returns its final answer

CLI prints AgentEvents as they stream so you can watch the agent reason, call tools, iterate.

## TUI

```bash
cargo build --release --bin harness-tui
./target/release/harness-tui --provider anthropic --model claude-sonnet-4-6
```

ratatui interactive UI. Same iii-bus shape as the CLI: connects to engine, registers runtime + provider, triggers `agent::run_loop`, renders the events stream.

- Multi-line editor with slash commands, `@file` fuzzy attachment, inline `!bash`
- Markdown render with collapsible tool / thinking blocks, queue + spinner indicator
- Native Kitty / iTerm2 inline image render via terminal escape protocols (placeholder fallback elsewhere)
- Clipboard image paste
- `/tree` overlay with parent / child branching glyphs (`├─` `└─` `│`), search, filter, bookmarks
- `/hotkeys` overlay listing every binding
- Themes (`dark`, `light`, user-supplied TOML at `~/.harness/themes/<name>.toml`)
- Keybinding overrides at `~/.harness/keybindings.json`
- Hot-reload via `notify` watcher: edit theme or keybindings file, TUI picks up the change live
- `HARNESS_ENGINE_URL` env var overrides the engine URL

## harnessd (all-in-one bundle)

```bash
./target/release/harnessd serve --providers all [--with-hook-example]
./target/release/harnessd status
```

Single binary that registers every harness worker on the bus in one process — runtime, 22 providers, 5 oauth flows, auth-storage, models-catalog, sessions, corpus, extract, compaction. Replaces the chain of `cargo run -p ...` calls.

## Functions registered on the bus

### `agent::*` (12)
| Function | Purpose |
|---|---|
| `agent::run_loop` | Orchestrator. Drives the inner state machine. Persists transcripts to `session-tree` when registered. |
| `agent::stream_assistant` | Provider router. Calls `provider::<name>::stream_assistant` based on payload. |
| `agent::prepare_tool` | Validate args, fan out `before_tool_call` via `publish_collect`. |
| `agent::execute_tool` | Resolve `tool::<name>` and dispatch via `iii.trigger`. |
| `agent::finalize_tool` | Fan out `after_tool_call` via `publish_collect`, merge replies. |
| `agent::transform_context` | Fan out `transform_context` via `publish_collect`, decode last reply. |
| `agent::convert_to_llm` | `AgentMessage[]` → provider wire shape. Pass-through default. |
| `agent::get_steering` | Drain mid-run injection queue. |
| `agent::get_followup` | Drain post-stop continuation queue. |
| `agent::abort` | Set abort signal in iii state. |
| `agent::push_steering` | HTTP-callable enqueue helper. |
| `agent::push_followup` | HTTP-callable enqueue helper. |

### `tool::*` (8)
`tool::read`, `tool::write`, `tool::edit`, `tool::ls`, `tool::grep`, `tool::find`, `tool::bash`, `tool::run_subagent`. `tool::bash` runtime-discovers `sandbox::exec`.

### `provider::<name>::stream_assistant` (22)
Each provider crate self-registers via `register_with_iii(iii)`:

```
anthropic, openai, openai-responses, google, google-vertex, azure-openai,
bedrock, openrouter, groq, cerebras, xai, deepseek, mistral, fireworks,
kimi-coding, minimax, zai, huggingface, vercel-ai-gateway, opencode-zen,
opencode-go, faux
```

### `oauth::<name>::{login, refresh, status}` (5)
PKCE / device-code flows registered by:

```
oauth-anthropic, oauth-openai-codex, oauth-github-copilot,
oauth-google-gemini-cli, oauth-google-antigravity
```

### `session::*` (8)
- `session::create`, `session::append`, `session::messages` — persistent transcripts
- `session::fork`, `session::clone` — branch off existing sessions
- `session::compact` — append a Compaction entry summarising the active path
- `session::tree` — return the tree-shape
- `session::export_html` — render the active path as a self-contained HTML doc

When `session-tree` is registered, `agent::run_loop` automatically hydrates from the existing transcript on entry and persists every new turn on exit.

### `auth::*` (5)
- `auth::get_token`, `auth::set_token`, `auth::delete_token` — credential vault
- `auth::list_providers`, `auth::status`

### `models::*` (3)
- `models::list`, `models::get`, `models::supports` — model capability catalog

### `corpus::*` (4)
- `corpus::scan`, `corpus::redact`, `corpus::review`, `corpus::publish` — session corpus pipeline

### `document::extract` (1)
PDF + DOCX text extraction.

### `policy::*` (3, opt-in via `policy-subscribers` binaries)
- `policy::denylist` — block tool calls by name
- `policy::audit_log` — JSONL append of every call
- `policy::dlp_scrubber` — redact AWS / OpenAI / GitHub / Stripe / Google secret shapes

## Hooks

Three pubsub topics. Subscribers are independent workers; the loop fans out via `publish_collect` (write event with `event_id` to topic, poll `agent::hook_reply/<event_id>` for replies, merge).

| Topic | Subscriber reply shape | Merge rule |
|---|---|---|
| `agent::before_tool_call` | `{ block: bool, reason: str }` | first-blocker-wins |
| `agent::after_tool_call` | partial `ToolResult` (`content`, `details`, `terminate`) | field-by-field |
| `agent::transform_context` | `{ messages: [...] }` or bare `[...]` | last reply wins |

Add a custom hook by registering an iii function and binding a `subscribe` trigger. See `workers/hook-example/src/lib.rs` for the minimal pattern and `workers/policy-subscribers/src/lib.rs` for production-shape examples.

```bash
# Reference subscribers — pick one or all
HOOK_EXAMPLE_DENY=dangerous,rm cargo run --release -p hook-example
cargo run --release --bin policy-denylist -- --deny "bash:rm -rf,sudo"
cargo run --release --bin audit-log     -- --log ~/.harness/audit.jsonl
cargo run --release --bin dlp-scrubber
```

## Worker inventory (44 crates)

```
workers/
├── harness-types/             # closed type vocabulary
├── harness-runtime/           # loop + 8 tools + collected pubsub + hook merge
├── harness-cli/               # thin iii-bus CLI binary
├── harness-tui/               # thin iii-bus TUI binary (ratatui)
├── harness-iii-bridge/        # legacy bridge crate (most code lives in harness-runtime now)
├── harnessd/                  # all-in-one daemon registering every worker
├── provider-base/             # shared HTTP/SSE/error infra + provider register helper
├── provider-{anthropic, openai, openai-responses, google, google-vertex,
│             azure-openai, bedrock, openrouter, groq, cerebras, xai,
│             deepseek, mistral, fireworks, kimi-coding, minimax, zai,
│             huggingface, vercel-ai-gateway, opencode-zen, opencode-go,
│             faux}/            # 22 provider workers
├── oauth-{anthropic, openai-codex, github-copilot,
│         google-gemini-cli, google-antigravity}/  # 5 oauth flows
├── auth-storage/              # credential vault
├── models-catalog/            # model capability database
├── session-tree/              # 8 session::* fns inc. persistent transcripts
├── context-compaction/        # subscriber on agent::events
├── session-corpus/            # corpus::* pipeline
├── document-extract/          # PDF / DOCX text extraction
├── overflow-classify/         # 20-pattern provider context-overflow detector
├── hook-example/              # reference live subscriber across the 3 hook topics
├── policy-subscribers/        # production-shape denylist + audit + DLP subscribers
├── replay-test/               # replay agent-session fixtures + e2e gated test
└── fixtures-gen/              # dev helper for generating golden fixtures
```

## Collected pubsub contract

`iii-sdk` 0.11 has no native `publish_collect`. The runtime workaround:

1. `IiiRuntime::publish_collect` mints `event_id` (uuid v4).
2. Publishes `{ event_id, reply_stream: "agent::hook_reply", payload: {...} }` to the topic.
3. Subscribers receive the envelope, do their work, write reply via `stream::set` on `(stream_name="agent::hook_reply", group_id=event_id)`.
4. Runtime polls `stream::list` until `timeout_ms` (default 5000), collects every reply.
5. `harness_runtime::hooks` (`merge_before` / `merge_after` / `decode_transform`) composes the final outcome.

When iii-sdk grows native `publish_collect`, the polling falls away — the merge logic stays.

## Testing

```bash
# Unit + integration tests (skips live e2e)
cargo test --workspace

# Live end-to-end against a running iii engine
iii --use-default-config &
IIIX_TEST_ENGINE_URL=ws://127.0.0.1:49134 cargo test -p replay-test --test end_to_end
```

The e2e test registers `harness-runtime` + `provider-faux`, drives `agent::run_loop` against a canned response, asserts the transcript is non-error with at least one assistant message.

## Status

Apache-2.0. v0.11.0 — `policy-subscribers` reference workers + refreshed `ARCHITECTURE.md` + verified live e2e. See [release notes](https://github.com/iii-experimental/harness/releases/tag/v0.11.0). Specs in repo: `ARCHITECTURE.md`, `PHASES.md`. Remaining iii-sdk gaps tracked in [`docs/SDK-BLOCKED.md`](docs/SDK-BLOCKED.md).

Both `harness-cli` and `harness-tui` are iii-first thin invokers as of v0.8. Hook subscribers can block tool calls, modify tool results, and rewrite context as of v0.10.

## Contributing

- Apache-2.0 only
- No external agent-harness product names in code, comments, commits, or PR text
- Provider names (Anthropic, OpenAI, Google, etc.) are APIs we authenticate against and may be referenced
- No emojis in any committed text
- Commit per concern, not per file
- No `Cargo.lock` in workspace root (library workspace)

## License

Apache-2.0. See `LICENSE`.
