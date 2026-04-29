# harness

Single-agent loop runtime on [iii-engine](https://iii.dev). 44 narrow workers, all iii-first.

> Status: 0.11.8, 0.x experimental. API surface unstable until production-proven.
> Live-validated: 10 e2e tests against a real iii engine, 41 lib test crates,
> 6 TUI render snapshots. llm-router `provider` field landed upstream
> (iii-hq/workers PR #57, merged 2026-04-29).

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
# → agent::stream_assistant probes for router::decide on the bus once.
# → When present, harness extracts the last user prompt + routing hints,
#   calls router::decide (RoutingRequest -> RoutingDecision), and dispatches
#   to the routed provider/model. Provider derivation: response.provider
#   field (shipped in llm-router via iii-hq/workers PR #57, merged), else
#   split on '/' for namespaced model ids, else fall back to caller hint.
# → When llm-router isn't on the bus, harness dispatches directly. No flag.

# Tier 4 — policy enforcement (denylist + audit + DLP)
cargo run --release --bin policy-denylist -- --deny "bash:rm -rf,sudo" &
cargo run --release --bin audit-log     -- --log ~/.harness/audit.jsonl &
cargo run --release --bin dlp-scrubber &
./target/release/harness "delete all logs"
# → before_tool_call fan-out. policy-denylist replies { block: true, reason }.
#   Loop blocks the call. Agent sees the block, plans around it.
# → after_tool_call fan-out (parallel). audit-log appends a JSONL line per
#   call. dlp-scrubber rewrites secrets (AWS / OpenAI / GitHub / Stripe /
#   Google) in the result text via merge_after's `content` override.
# All three subscribers are live-validated in the e2e suite as of 0.11.7.

# Tier 5 — sub-agent recursion + context transforms
./target/release/harness "use run_subagent for the file scan, summarise"
# → tool::run_subagent spawns a focused child loop; bounded at depth 3
#   (override via max_subagent_depth) to prevent unbounded recursion.
# → transform_context fan-out runs before every stream_assistant call;
#   subscribers can rewrite the in-flight messages array (e.g. context
#   compaction, redaction, system-prompt injection).
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

# zero-config smoke (no API key needed)
./target/release/harness --provider faux --model echo "say hi"

# experimental providers (endpoint URL not yet verified upstream)
./target/release/harness --provider zai --experimental-providers "..."
# Without the flag, harness refuses with a remediation hint.
# Gated set: opencode-go, minimax, huggingface, opencode-zen, zai,
# vercel-ai-gateway, kimi-coding.
```

Flags worth knowing:
- `--max-turns <n>` — hard cap on assistant turns (default 10). Loop emits a synthetic "loop stopped" assistant message and exits cleanly when reached.
- `--engine-url <ws>` — override `ws://127.0.0.1:49134`.
- `--experimental-providers` — opt in to the seven providers whose upstream endpoint URL hasn't been verified.

Built-in tools the agent can call:
- `read`, `write`, `edit` — file ops with diff-style replace
- `ls`, `find`, `grep` — directory walks and substring search
- `bash` — `bash -lc` on host, or routed through `iii-sandbox::exec` when the sandbox worker is registered. Emits a single `tracing::warn!` on the first host fallback so the user knows commands aren't sandboxed.
- `run_subagent` — spawn a focused sub-agent for a subtask, returns its final answer. Bounded at depth 3 by default (override per-call via `max_subagent_depth`); the chain is encoded in the session-id (`root::sub-...::sub-...`).

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

### `models::*` (4)
- `models::list`, `models::get`, `models::supports` — state-first model capability catalog
- `models::register` — write a Model to state under `models:<provider>:<id>` (state is the source of truth; embedded `data/models.json` is a one-time seed used only when state is empty)

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

| Topic | Subscriber reply shape | Merge rule | Live-validated |
|---|---|---|---|
| `agent::before_tool_call` | `{ block: bool, reason: str }` | first-blocker-wins | ✅ `policy_denylist_blocks_tool_dispatch` + `hooks_before_and_after_see_tool_calls` |
| `agent::after_tool_call` | partial `ToolResult` (`content`, `details`, `terminate`) | field-by-field | ✅ `audit_log_records_after_tool_call` + `dlp_scrubber_rewrites_secret_in_tool_result` |
| `agent::transform_context` | `{ messages: [...] }` or bare `[...]` | last reply wins | ✅ `transform_context_subscriber_mutates_messages` |

Subscribers reply via `stream::set` with `(stream_name=agent::hook_reply, group_id=event_id, item_id=<uuid>)`. iii v0.11.x silently drops writes missing `item_id` — every shipped harness subscriber mints one per reply.

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
3. Subscribers receive the envelope, do their work, write reply via `stream::set` on `(stream_name="agent::hook_reply", group_id=event_id, item_id=<fresh uuid>)`. The `item_id` is mandatory — iii v0.11.x silently drops writes without one.
4. Runtime polls `stream::list` until `timeout_ms` (default 5000), accepting both the bare-array shape iii v0.11.x ships and the older `{items: [...]}` envelope. Collects every reply, normalising `item.get("data")` or the bare item.
5. `harness_runtime::hooks` (`merge_before` / `merge_after` / `decode_transform`) composes the final outcome.

When iii-sdk grows a `Stream` trigger that fires per new item without polling (the type already exists in `iii_sdk::builtin_triggers`; semantics around backfill / ordering need validation), the polling falls away. Merge logic stays.

## Testing

```bash
# Lib + integration tests (skips live e2e)
cargo test --workspace --lib --release        # 41 lib test crates
cargo test --release -p harness-tui --test render_snapshot   # 2 ratatui snapshots

# Live end-to-end against a running iii engine
iii --use-default-config &
IIIX_TEST_ENGINE_URL=ws://127.0.0.1:49134 \
  cargo test --release -p replay-test --test end_to_end -- --test-threads=1
```

Nine gated e2e tests cover the bus-mediated paths the unit tests can't:

| Test | What it proves |
|---|---|
| `faux_round_trip` | runtime + faux provider + run_loop round-trip |
| `llm_router_swaps_provider_and_model` | tier-3 router delegation rewrites `(provider, model)` |
| `policy_denylist_blocks_tool_dispatch` | `merge_before` short-circuits dispatch when a subscriber blocks |
| `hooks_before_and_after_see_tool_calls` | hook-example subscribers fire on both topics with counters proof |
| `audit_log_records_after_tool_call` | `policy::audit_log` writes one JSONL line per dispatch |
| `dlp_scrubber_rewrites_secret_in_tool_result` | `policy::dlp_scrubber` replaces secrets via `merge_after.content` |
| `transform_context_subscriber_mutates_messages` | the third hook topic rewrites the in-flight messages array |
| `run_subagent_refuses_at_depth_limit` | sub-agent depth-3 cap fires before nested run_loop spawn |
| `oauth_anthropic_register_smoke` | oauth-anthropic registers `login/refresh/status` and `status` is callable |
| `models_catalog_state_register_round_trip` | `models::register` writes a custom Model to state; `models::get` and `models::supports` see it — proves state-first registry, not the embedded baseline |

TUI snapshots render `App` against a `TestBackend` at fixed dimensions, capture the trimmed cell buffer (style discarded for stability), and diff via `insta`. Re-bless with `cargo insta test -p harness-tui --review`.

## Status

Apache-2.0. v0.11.7 — full live e2e for all three hook topics + sub-agent depth + oauth registration smoke + TUI render snapshots. See [release notes](https://github.com/iii-experimental/harness/releases/tag/v0.11.7). Specs in repo: `ARCHITECTURE.md`, `PHASES.md`. Remaining iii-sdk gaps tracked in [`docs/SDK-BLOCKED.md`](docs/SDK-BLOCKED.md).

Both `harness-cli` and `harness-tui` are iii-first thin invokers as of v0.8. Hook subscribers can block tool calls, modify tool results, and rewrite context as of v0.10.

### Loop semantics scoreboard

| Bucket | Status |
|---|---|
| 12 canonical agent fns | ✅ shipped |
| 8 builtin tools | ✅ shipped |
| 3 hook topics live e2e | ✅ 3/3 |
| 3 policy subscribers live e2e | ✅ 3/3 (denylist + audit + dlp) |
| Sub-agent depth cap (default 3) | ✅ shipped |
| max_turns enforcement | ✅ shipped |
| Provider workers compiled + registered | 22/22 (15 verified, 7 gated) |
| OAuth flows | 5 registered, 1 e2e smoke |
| Models-catalog: state-first | ✅ shipped + e2e |
| TUI snapshot tests | ✅ 6 (idle, after-message, running+tool-call, tool-end, error, wide) |
| llm-router `provider` field upstream | ✅ merged (iii-hq/workers#57) |

### Behaviour fixed since v0.11.0 you may not notice

- `stream::list` envelope drift: CLI/TUI/runtime now accept the bare-array shape iii v0.11.x ships with the `{items: [...]}` fallback (CLI was silent before v0.11.5).
- Hook replies were silently dropped: every harness subscriber now mints `item_id: uuid::v4()` on `stream::set` (v0.11.6).
- `agent::run_loop` SDK-default 30s timeout: CLI/TUI cap at 600s, provider dispatch at 300s, run_subagent at 600s (v0.11.5/v0.11.6).
- Audit-log byte-interleave: append_jsonl serialises writes per path with a process-wide tokio Mutex map (v0.11.7).
- `RouterPresenceCache` blocked concurrent first-callers behind the bus probe; refactored to atomic fast path + double-checked-lock (v0.11.7).

## Contributing

- Apache-2.0 only
- No external agent-harness product names in code, comments, commits, or PR text
- Provider names (Anthropic, OpenAI, Google, etc.) are APIs we authenticate against and may be referenced
- No emojis in any committed text
- Commit per concern, not per file
- No `Cargo.lock` in workspace root (library workspace)

## License

Apache-2.0. See `LICENSE`.
