# Architecture

The agent loop is a state machine. Every concern outside the state machine is a worker on the iii bus. Both reference binaries (`harness-cli`, `harness-tui`) are thin invokers — they connect to a running iii engine, register the runtime + a provider, trigger `agent::run_loop`, and consume the events stream.

## The 12 agent functions

`harness-runtime::register_with_iii` publishes:

| Function | Purpose |
|---|---|
| `agent::run_loop` | Orchestrator. Drives the inner state machine. Persists transcripts to `session-tree` when registered. |
| `agent::stream_assistant` | Provider router. Calls `provider::<name>::stream_assistant` based on payload. |
| `agent::prepare_tool` | Validate args, fan out `before_tool_call` via `publish_collect`. |
| `agent::execute_tool` | Resolve `tool::<name>` and dispatch via `iii.trigger`. |
| `agent::finalize_tool` | Fan out `after_tool_call` via `publish_collect`, merge replies. |
| `agent::transform_context` | Fan out `transform_context` via `publish_collect`, decode the last reply. |
| `agent::convert_to_llm` | `AgentMessage[]` → provider wire shape. Pass-through default. |
| `agent::get_steering` | Drain mid-run injection queue at end of each tool batch. |
| `agent::get_followup` | Drain post-stop continuation queue before final exit. |
| `agent::abort` | Set abort signal in iii state for the session. |
| `agent::push_steering` | HTTP-callable enqueue helper for steering messages. |
| `agent::push_followup` | HTTP-callable enqueue helper for follow-up messages. |

## The 7 built-in tools

Same crate registers `tool::<name>` for each built-in:

```
tool::read   tool::write  tool::edit
tool::ls     tool::grep   tool::find
tool::bash   tool::run_subagent
```

`tool::bash` runtime-discovers `sandbox::exec` via `iii.list_functions()`. When the iii-sandbox worker is registered, bash auto-routes through the microVM. No user flag — the bus is the source of truth.

`tool::run_subagent` triggers `agent::run_loop` recursively with a derived `child_session_id`, returns the child's final assistant text. Args: `prompt`, `provider`, `model`, optional `system_prompt` + `parent_session_id`.

## Wire

HTTP triggers registered alongside the agent functions:

```
POST /agent/prompt                 -> agent::run_loop
POST /agent/<id>/steer             -> agent::push_steering
POST /agent/<id>/abort             -> agent::abort
POST /agent/<id>/follow_up         -> agent::push_followup
```

State (iii state worker, scope `agent`):

```
session/<id>/steering          -> Vec<AgentMessage>
session/<id>/followup          -> Vec<AgentMessage>
session/<id>/abort_signal      -> bool
```

Per-session transcripts live in `session-tree`'s store, not in shared iii state. Hydrated on `agent::run_loop` entry via `session::messages`, persisted on each turn via `session::append`.

Streams:

```
agent::events/<session_id>     # 11 AgentEvent variants
agent::hook_reply/<event_id>   # collected-pubsub replies (per hook fan-out)
```

Pubsub topics:

```
agent::before_tool_call        # collect, merge_before — first-block-wins
agent::after_tool_call         # collect, merge_after — field-by-field merge
agent::transform_context       # collect, decode_transform — last reply wins
```

## Collected pubsub contract

iii-sdk 0.11 has no native `publish_collect`. The runtime workaround:

1. `IiiRuntime::publish_collect` mints `event_id` (uuid v4).
2. Publishes envelope `{ event_id, reply_stream: "agent::hook_reply", payload: {...} }` to the topic.
3. Subscribers receive, do their work, write reply via `stream::set` on `(stream_name="agent::hook_reply", group_id=event_id)`.
4. Runtime polls `stream::list` until `timeout_ms` (default 5000), collects every reply that arrived.
5. Merge logic in `harness_runtime::hooks` composes the final outcome.

Subscribers MUST follow this envelope. See `workers/hook-example/src/lib.rs` and `workers/policy-subscribers/src/lib.rs` for reference impls.

When iii-sdk grows native `publish_collect`, the polling falls away — the merge logic stays.

## The loop

```
fn run_loop(session_id, prompts):
  prior = session::messages(session_id)?  // best-effort hydrate
  ctx.messages = prior + prompts
  for m in prompts: session::append(session_id, m)?  // best-effort persist
  emit AgentStart
  pending = drain_steering(session_id)
  first_turn = true

  outer:
    loop:
      has_more = true
      while has_more or pending.not_empty():
        if not first_turn: emit TurnStart
        first_turn = false

        ctx.messages.extend(pending); pending.clear()
        ctx.messages = transform_context(ctx.messages)   // collected pubsub
        llm_msgs = convert_to_llm(ctx.messages)
        assistant = stream_assistant(session_id, llm_msgs, tools)
        ctx.messages.push(assistant)

        if assistant.stop_reason in (Error, Aborted):
          emit AgentEnd; return

        tool_calls = assistant.tool_calls()
        has_more = false
        if tool_calls.not_empty():
          batch = execute_tool_batch(session_id, assistant, tool_calls, ctx)
          ctx.messages.extend(batch.results)
          has_more = not batch.terminate

        emit TurnEnd
        pending = drain_steering(session_id)

      followups = drain_followup(session_id)
      if followups.not_empty():
        pending = followups; continue outer
      break

    emit AgentEnd
    for m in new_messages: session::append(session_id, m)?  // post-baseline persist
```

## Tool batch

```
fn execute_tool_batch(session_id, assistant, tool_calls, ctx):
  has_sequential = tool_calls.any(t => t.execution_mode == Sequential)
  mode = Sequential if has_sequential else ctx.tool_execution_mode

  for call in tool_calls (mode-respecting):
    prep = prepare_tool(call, ctx)
      # validates schema, fans out before_tool_call (collected pubsub)
      # first {block: true} reply triggers Immediate skip

    if prep.kind == Prepared:
      exec = trigger("tool::<name>", call.id, args)
        # streams partial results to agent::events/<session_id>
      final = finalize_tool(prep, exec)
        # fans out after_tool_call (collected pubsub), merges field-by-field

  emit ToolExecutionEnd per call
  build ToolResultMessage per call
  batch.terminate = ALL results.terminate == true
  batch.results = ordered ToolResultMessages
```

## Sub-agents

`tool::run_subagent` is itself an iii function. The handler triggers `agent::run_loop` with a child session id and returns the child's final assistant text as a tool result. Engine ties parent + child via the trigger graph; both sessions stream events on `agent::events/<their_session_id>`.

No new abstraction. Reusing the existing function is the entire feature.

## 14 stream-event variants

Provider workers emit these via `provider-base::register_provider_stream`'s drain logic. Loop never assembles partial state — it consumes the final `AssistantMessage` returned by `provider::<name>::stream_assistant`.

```
start | text_start/delta/end | thinking_start/delta/end
| toolcall_start/delta/end | usage | stop | done | error
```

`done` and `error` are terminal. Stream MUST NOT throw; failures encode as a terminal `error` event with classified `error_kind` (`AuthExpired | RateLimited | ContextOverflow | Transient | Permanent`).

## 11 AgentEvent variants

Streamed on `agent::events/<session_id>`. Stable wire format consumed by UIs (CLI/TUI) and observers.

```
AgentStart | AgentEnd
TurnStart | TurnEnd
MessageStart | MessageUpdate | MessageEnd
ToolExecutionStart | ToolExecutionUpdate | ToolExecutionEnd
TraceUpdate
```

## Tool registry

Any worker registering `tool::<name>` is auto-discoverable. The loop's `resolve_tool` checks `iii.list_functions()` for the matching id and dispatches via `iii.trigger`.

Built-in tools (read/write/edit/ls/grep/find/bash/run_subagent) live as iii functions in `harness-runtime` — same surface as third-party tools. Add a new tool by registering `tool::<name>` from any worker; it appears in the next turn's catalog without restart.

## Worker inventory (43 crates)

Loop, runtime, types:
- `harness-types` — closed type vocabulary
- `harness-runtime` — loop + 8 tools + collected pubsub + hooks merge
- `harness-cli` — thin iii-bus client
- `harness-tui` — thin iii-bus client (ratatui)
- `harness-iii-bridge` — legacy bridge crate (most code moved into `harness-runtime`)
- `harnessd` — all-in-one daemon registering every harness worker

Providers (22, all expose `register_with_iii(iii)`):
- `provider-base` — shared HTTP/SSE/error infra
- `provider-anthropic`, `provider-openai`, `provider-openai-responses`, `provider-google`, `provider-google-vertex`, `provider-azure-openai`, `provider-bedrock`, `provider-openrouter`, `provider-groq`, `provider-cerebras`, `provider-xai`, `provider-deepseek`, `provider-mistral`, `provider-fireworks`, `provider-kimi-coding`, `provider-minimax`, `provider-zai`, `provider-huggingface`, `provider-vercel-ai-gateway`, `provider-opencode-zen`, `provider-opencode-go`, `provider-faux`

OAuth (5, all expose `register_with_iii(iii)`):
- `oauth-anthropic`, `oauth-openai-codex`, `oauth-github-copilot`, `oauth-google-gemini-cli`, `oauth-google-antigravity`

Auth + catalog:
- `auth-storage` — `auth::*` (5 fns) credential vault
- `models-catalog` — `models::*` (3 fns) capability database

Sessions + persistence:
- `session-tree` — `session::*` (8 fns) including create/append/messages for transcript persistence
- `context-compaction` — subscribes to `agent::events`, republishes overflow signals
- `session-corpus` — `corpus::*` (4 fns) scan/redact/review/publish
- `document-extract` — `document::extract` for PDF/DOCX

Hooks + policy:
- `hook-example` — reference live subscriber across the 3 hook topics
- `policy-subscribers` — production-shape reference subscribers (denylist, audit log, DLP scrubber). Three standalone binaries.

Other:
- `overflow-classify` — provider context-overflow detector (20 patterns)
- `replay-test`, `fixtures-gen` — test + dev helpers (e2e gated on `IIIX_TEST_ENGINE_URL`)
