# Stream-event taxonomy

Provider workers MUST emit these event variants on the iii stream during a streaming response. The harness loop assembles the final `AssistantMessage` from the sequence.

`done` and `error` are terminal. Streams MUST NOT throw; failures are encoded as the final `error` event with `error_kind` populated.

## Event variants

| Variant | Purpose |
|---|---|
| `start` | Stream is open; partial message is empty or has model/provider metadata |
| `text_start` | A text content block is beginning |
| `text_delta` | Append-text chunk; carries `delta: string` |
| `text_end` | Current text block is complete |
| `thinking_start` | A thinking content block is beginning |
| `thinking_delta` | Append-thinking chunk; carries `delta: string` |
| `thinking_end` | Current thinking block is complete |
| `toolcall_start` | A tool-call content block is beginning |
| `toolcall_delta` | Append to tool-call arguments JSON; carries `delta: string` |
| `toolcall_end` | Tool-call arguments JSON is complete and parseable |
| `usage` | Token accounting; carries `{input, output, cache_read, cache_write}` |
| `stop` | Stop reason carried as enum; optional error fields |
| `done` | Final assembled `AssistantMessage` is complete |
| `error` | Final error `AssistantMessage` is complete |

## Stop reasons

| Reason | Meaning |
|---|---|
| `end` | Model produced an end-of-response signal |
| `length` | Output token limit reached |
| `tool` | Model emitted tool calls and is awaiting results |
| `aborted` | Caller aborted via `agent::abort` or signal |
| `error` | Provider returned an error |

## Error kinds

| Kind | Trigger |
|---|---|
| `auth_expired` | HTTP 401 or provider-specific expired-token signal |
| `rate_limited` | HTTP 429 or provider-specific throttling shape |
| `context_overflow` | Matched by `overflow-classify` regex catalog |
| `transient` | HTTP 5xx or unknown |
| `permanent` | HTTP 4xx (other) |

## Provider obligations

1. Never throw. Errors land as final `error` event.
2. Sanitize outgoing strings — strip unpaired UTF-16 surrogates that break JSON across providers.
3. Resolve API key per-call via `auth::get(provider)`. If OAuth expired, call `oauth::<provider>::refresh` in-flight and retry once.
4. On any error, classify via `overflow-classify::classify_error(text, http_status)` and set `error_kind` on the final `error` event.
5. Honor `options.transport`, `options.cache_retention`, `options.thinking_level` — provider maps to native shape.
6. Return response headers in stream metadata (rate-limit inspection).
