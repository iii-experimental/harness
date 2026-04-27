# Architecture

The agent loop is a state machine. Every concern outside the state machine is a worker on the iii bus.

## The 10 functions

| Function | Purpose |
|---|---|
| `agent::run_loop` | Orchestrator. Drives the inner state machine. |
| `agent::stream_assistant` | One LLM streaming call. Delegates to `provider::<chosen>::stream` via `router::decide`. |
| `agent::prepare_tool` | Validate args, run `prepare_arguments` shim, fan out `before_tool_call`. |
| `agent::execute_tool` | Dispatch to `tool::<name>`. Built-ins inlined. Bash → engine `shell` worker. |
| `agent::finalize_tool` | Fan out `after_tool_call`, merge results, build `ToolResultMessage`. |
| `agent::transform_context` | Pipeline `transform_context` subscribers in registration order. |
| `agent::convert_to_llm` | `AgentMessage[]` → `Message[]` at wire boundary. |
| `agent::get_steering` | Drain mid-run injection queue at end of each tool batch. |
| `agent::get_followup` | Drain post-stop continuation queue before final exit. |
| `agent::abort` | Set abort signal in state for current session. |

## Wire

```
http::POST /agent/prompt
  -> agent::run_loop(session_id, prompts)

http::POST /agent/<id>/steer
  -> push to agent::session/<id>/steering

http::POST /agent/<id>/abort
  -> set agent::session/<id>/abort_signal

http::POST /agent/<id>/follow_up
  -> push to agent::session/<id>/followup
```

State (iii state worker keys):

```
agent::session/<id>/messages
agent::session/<id>/state
agent::session/<id>/steering
agent::session/<id>/followup
agent::session/<id>/abort_signal
session::<id>::entries
session::<id>::meta
auth::credentials::<provider>
models::catalog::<provider>
```

Streams:

```
agent::events/<session_id>           # AgentEvent variants
tool::<name>::progress/<call_id>     # partial tool results
```

Pubsub topics:

```
agent::before_tool_call              # collect, first-block-wins
agent::after_tool_call               # chain, field-by-field merge
agent::transform_context             # ordered pipeline
```

## The loop

```
fn run_loop(session_id, prompts):
  ctx = state.load(session_id)
  ctx.messages.extend(prompts)
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
        ctx.messages = pubsub.pipeline(transform_context, ctx.messages)
        llm_msgs = call(convert_to_llm, ctx.messages)
        assistant = call(stream_assistant, session_id, llm_msgs)
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
        state.save(session_id, ctx)
        pending = drain_steering(session_id)

      followups = drain_followup(session_id)
      if followups.not_empty():
        pending = followups; continue outer
      break

    emit AgentEnd
```

## Tool batch

```
fn execute_tool_batch(session_id, assistant, tool_calls, ctx):
  has_sequential = tool_calls.any(t => t.execution_mode == Sequential)
  mode = Sequential if has_sequential else ctx.tool_execution_mode

  for call in tool_calls (mode-respecting):
    prep = call(prepare_tool, call, ctx)
      # validates schema, runs prepare_arguments shim
      # fans out before_tool_call; first {block: true} returns Immediate

    if prep.kind == Prepared:
      exec = trigger("tool::<name>", prep.toolCallId, prep.args, signal)
        # streams partial results to agent::events/<session_id>
      final = call(finalize_tool, prep, exec)
        # fans out after_tool_call; merges field-by-field

  emit ToolExecutionEnd per call
  build ToolResultMessage per call
  batch.terminate = ALL results.terminate == true
  batch.results = ordered ToolResultMessages
```

## Sub-agents

A sub-agent is a nested call to `agent::run_loop`. From inside a tool's `execute` function:

```
iii.trigger("agent::run_loop", { parent_session_id, prompt, tools_subset })
```

Engine ties parent and child traces. Child events stream on `agent::events/<child_session_id>`. Parent observes completion when iii returns the call result.

No new abstraction. Reusing the existing function is the entire feature.

## 11 stream-event variants

Provider workers MUST emit these into the iii stream. Loop assembles `AssistantMessage` from sequence:

```
start | text_start/delta/end | thinking_start/delta/end
| toolcall_start/delta/end | usage | stop | done | error
```

`done` and `error` are terminal. Stream MUST NOT throw; failures are encoded as final `error` event with `error_kind: AuthExpired | RateLimited | ContextOverflow | Transient | Permanent`.

## 11 AgentEvent variants

```
AgentStart | AgentEnd
TurnStart | TurnEnd
MessageStart | MessageUpdate | MessageEnd
ToolExecutionStart | ToolExecutionUpdate | ToolExecutionEnd
```

Stable wire format consumed by UIs and observers.

## Tool registry contract

Any worker registering an iii function `tool::<name>` is auto-discoverable. Loop enumerates `tool::*` at session start. Tools may register `tool::<name>::describe()` returning `AgentTool` metadata.

Built-in tools inlined in `harness-runtime`: `read`, `write`, `edit`, `grep`, `find`, `ls`. Bash dispatched to engine `shell` worker. New tools `iii worker add`'d at runtime appear in next turn's catalog without restart.
