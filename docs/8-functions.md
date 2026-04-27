# The 10 functions

Canonical decomposition of the agent loop into iii functions. Hosted by `harness-runtime`.

| Function | Purpose | Side effects |
|---|---|---|
| `agent::run_loop` | Orchestrator. Drives the inner state machine. | Reads/writes session state; emits `agent::events/<id>` stream |
| `agent::stream_assistant` | One LLM streaming call. | Calls `router::decide` then `provider::<chosen>::stream`; pushes events |
| `agent::prepare_tool` | Validate args, run `prepare_arguments` shim, fan out `before_tool_call`. | Pubsub fan-out; no state writes |
| `agent::execute_tool` | Dispatch to `tool::<name>`. Built-ins inlined. Bash → engine `shell`. | Tool side effects |
| `agent::finalize_tool` | Fan out `after_tool_call`, merge results, build `ToolResultMessage`. | Pubsub fan-out |
| `agent::transform_context` | Pipeline `transform_context` subscribers in registration order. | Pubsub pipeline |
| `agent::convert_to_llm` | `AgentMessage[]` → `Message[]` at wire boundary. | Pure |
| `agent::get_steering` | Drain mid-run injection queue. | State writes (atomic pop) |
| `agent::get_followup` | Drain post-stop continuation queue. | State writes (atomic pop) |
| `agent::abort` | Set abort signal in state. | State write |

## Loop flow

```
http::POST /agent/prompt
  -> agent::run_loop
       -> agent::transform_context (pubsub pipeline)
       -> agent::convert_to_llm
       -> router::decide
       -> provider::<chosen>::stream  (writes agent::events/<id>)
       -> agent::prepare_tool
            -> pubsub: agent::before_tool_call
       -> agent::execute_tool
            -> tool::<name>           (writes tool::<name>::progress/<call_id>)
       -> agent::finalize_tool
            -> pubsub: agent::after_tool_call
       -> agent::get_steering
       (loop)
       -> agent::get_followup
       (loop or end)
```

## Pubsub topic semantics

| Topic | Merge | Returns |
|---|---|---|
| `agent::before_tool_call` | First `block: true` wins | `{block?, reason?}` |
| `agent::after_tool_call` | Field-by-field merge in registration order | `{content?, details?, isError?, terminate?}` |
| `agent::transform_context` | Sequential pipeline; each output feeds next | `Vec<AgentMessage>` |

## Sub-agent

A sub-agent is a nested invocation of `agent::run_loop` from inside a tool's `execute` body. The engine ties parent and child traces. Child events stream on `agent::events/<child_session_id>`.
