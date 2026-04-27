//! Harness runtime: the agent loop and sibling functions.
//!
//! The orchestrator drives a small state machine: stream LLM, batch tool calls,
//! pull steering messages between batches, decide whether to continue.
//!
//! All side-effectful concerns are abstracted behind the [`LoopRuntime`] trait
//! so the loop is testable in isolation against in-memory implementations.
//! Production wiring binds `LoopRuntime` to iii-engine primitives.

pub mod loop_state;
pub mod register;
pub mod runtime;
pub mod tools;

pub use tools::{BashPlaceholder, EditTool, FindTool, GrepTool, LsTool, ReadTool, WriteTool};

pub use loop_state::{run_loop, LoopConfig, LoopOutcome};
pub use register::{
    register_with_iii, EVENTS_STREAM, STATE_SCOPE, TOPIC_AFTER, TOPIC_BEFORE, TOPIC_TRANSFORM,
};
pub use runtime::{
    BatchOutcome, CapturedEvents, EchoTool, EventSink, FinalizedTool, HookOutcome, LoopRuntime,
    MemoryRuntime, ToolHandler,
};
