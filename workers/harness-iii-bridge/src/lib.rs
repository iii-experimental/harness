//! Bridge between [`harness_runtime`] and the iii-engine.
//!
//! [`IiiBridgeRuntime`] implements [`harness_runtime::LoopRuntime`] over a
//! pluggable [`IiiClientLike`] abstraction. It:
//!
//! - persists session messages, steering queues, follow-up queues, and the
//!   abort signal as keys on the iii state worker (under scope `agent`);
//! - emits AgentEvents to the `agent::events` stream, group_id `<session_id>`;
//! - resolves tools by enumerating `tool::*` functions registered on the bus;
//! - runs the three hook topics (`agent::before_tool_call`,
//!   `agent::after_tool_call`, `agent::transform_context`) via iii pubsub
//!   fan-out using pure merge logic in [`hooks`].
//!
//! Companion type [`IiiEventSink`] forwards [`harness_types::AgentEvent`]
//! writes to the per-session iii stream so the [`harness_runtime::run_loop`]
//! caller and a remote console can both observe the same event sequence.
//!
//! ## Architecture
//!
//! The runtime is generic over an [`IiiClientLike`] trait so it stays
//! testable without a live engine. An [`IiiSdkClient`] adapter wraps a real
//! [`iii_sdk::III`]; the [`testing::FakeClient`] used in unit tests records
//! state writes/reads and lets the test stage tool responses.
//!
//! ## State key layout
//!
//! All keys live under scope `agent`:
//!
//! ```text
//! session/<id>/messages       -> Vec<AgentMessage>
//! session/<id>/state          -> AgentSessionState
//! session/<id>/steering       -> Vec<AgentMessage>
//! session/<id>/followup       -> Vec<AgentMessage>
//! session/<id>/abort_signal   -> bool
//! ```
//!
//! ## iii-sdk integration notes (0.11.x)
//!
//! - `state::set` / `state::get` / `state::delete` are invoked through
//!   `iii.trigger(TriggerRequest { function_id, payload, .. })`. iii-sdk
//!   does not currently ship a typed `state_get/set/cas` builder API.
//! - `publish` is invoked the same way; it is fire-and-forget. iii-sdk
//!   pubsub does not collect subscriber responses — see [`hooks::merge`]
//!   for the in-process merge contract this bridge applies.
//! - HTTP-trigger registration uses `register_trigger` with
//!   `trigger_type: "http"` and `config: { api_path, http_method }`. Note
//!   that iii engine prepends `/`, so `api_path` must NOT start with one.
//! - The 4 HTTP triggers documented in `ARCHITECTURE.md`
//!   (`POST /agent/prompt`, `POST /agent/<id>/steer`,
//!   `POST /agent/<id>/abort`, `POST /agent/<id>/follow_up`) are wired
//!   in [`register::register_agent_functions`].

pub mod client;
pub mod hooks;
pub mod register;
pub mod runtime;
pub mod sandbox_tool;
pub mod sink;

#[cfg(test)]
pub mod testing;

pub use client::{BridgeError, IiiClientLike, IiiSdkClient, NoOpClient};
pub use register::{register_agent_functions, AgentFunctionRefs, StreamAssistantFn};
pub use runtime::{state_keys, IiiBridgeRuntime};
pub use sandbox_tool::SandboxedBashTool;
pub use sink::IiiEventSink;
