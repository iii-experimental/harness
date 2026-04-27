//! Shared `register_with_iii` plumbing for provider crates.
//!
//! Each provider crate exposes
//! `pub async fn register_with_iii(iii: &iii_sdk::III) -> anyhow::Result<()>`
//! which publishes `provider::<name>::stream` on the iii bus. The handler
//! decodes a JSON payload of the shape
//! `{ config, system_prompt, messages, tools }`, calls into the crate's own
//! `pub async fn stream(...)`, drains the resulting
//! [`ReceiverStream<AssistantMessageEvent>`], and returns
//! `{ events: [<AssistantMessageEvent>...] }`.
//!
//! `iii-sdk` 0.11 returns a single `Value` from a registered function — there
//! is no per-call streaming response surface — so the contract is to collect
//! the event sequence into a JSON array. Callers that want incremental events
//! can subscribe to a stream of their own choosing on top of this primitive.

use std::future::Future;
use std::sync::Arc;

use harness_types::{AgentMessage, AgentTool, AssistantMessageEvent};
use iii_sdk::{FunctionRef, IIIError, RegisterFunctionMessage, III};
use serde::de::DeserializeOwned;
use serde_json::Value;
use tokio_stream::wrappers::ReceiverStream;
use tokio_stream::StreamExt;

/// Register `provider::<name>::stream` as an iii function backed by the
/// supplied `stream_fn`.
///
/// `stream_fn` is the crate's own `pub async fn stream(...)`. Function
/// pointers and zero-capture closures both implement `Copy`, so the handler
/// can clone-by-copy on every invocation. Configs must implement
/// [`DeserializeOwned`].
pub fn register_provider_stream<C, F, Fut>(
    iii: &III,
    provider_name: &str,
    stream_fn: F,
) -> FunctionRef
where
    C: DeserializeOwned + Send + Sync + 'static,
    F: Fn(Arc<C>, String, Vec<AgentMessage>, Vec<AgentTool>) -> Fut + Copy + Send + Sync + 'static,
    Fut: Future<Output = ReceiverStream<AssistantMessageEvent>> + Send + 'static,
{
    let id = format!("provider::{provider_name}::stream");
    let description = format!("Stream a response from the {provider_name} provider");
    iii.register_function((
        RegisterFunctionMessage::with_id(id).with_description(description),
        move |payload: Value| async move {
            let cfg_value = payload
                .get("config")
                .cloned()
                .ok_or_else(|| IIIError::Handler("missing required field: config".into()))?;
            let cfg: C = serde_json::from_value(cfg_value)
                .map_err(|e| IIIError::Handler(format!("invalid config: {e}")))?;
            let system_prompt = payload
                .get("system_prompt")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let messages: Vec<AgentMessage> = payload
                .get("messages")
                .cloned()
                .map(serde_json::from_value)
                .transpose()
                .map_err(|e| IIIError::Handler(format!("invalid messages: {e}")))?
                .unwrap_or_default();
            let tools: Vec<AgentTool> = payload
                .get("tools")
                .cloned()
                .map(serde_json::from_value)
                .transpose()
                .map_err(|e| IIIError::Handler(format!("invalid tools: {e}")))?
                .unwrap_or_default();

            let mut s = stream_fn(Arc::new(cfg), system_prompt, messages, tools).await;
            let mut events: Vec<AssistantMessageEvent> = Vec::new();
            while let Some(ev) = s.next().await {
                events.push(ev);
            }
            serde_json::to_value(serde_json::json!({ "events": events }))
                .map_err(|e| IIIError::Handler(e.to_string()))
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use harness_types::{AssistantMessage, ContentBlock, StopReason, TextContent};
    use serde::Deserialize;
    use tokio::sync::mpsc;

    #[derive(Debug, Deserialize)]
    #[allow(dead_code)]
    struct DummyConfig {
        model: String,
    }

    #[allow(dead_code)]
    async fn dummy_stream(
        cfg: Arc<DummyConfig>,
        _system_prompt: String,
        _messages: Vec<AgentMessage>,
        _tools: Vec<AgentTool>,
    ) -> ReceiverStream<AssistantMessageEvent> {
        let (tx, rx) = mpsc::channel(4);
        tokio::spawn(async move {
            let final_msg = AssistantMessage {
                content: vec![ContentBlock::Text(TextContent { text: "ok".into() })],
                stop_reason: StopReason::End,
                error_message: None,
                error_kind: None,
                usage: None,
                model: cfg.model.clone(),
                provider: "dummy".into(),
                timestamp: 0,
            };
            let _ = tx
                .send(AssistantMessageEvent::Done { message: final_msg })
                .await;
        });
        ReceiverStream::new(rx)
    }

    /// Compile-time guard that the generic bounds on
    /// [`register_provider_stream`] accept a real provider-shaped
    /// `pub async fn stream(...)` function pointer. Any breakage in the
    /// helper signature surfaces as a `provider-base` build failure rather
    /// than cascading through every provider crate.
    #[allow(dead_code)]
    fn _bounds_witness(iii: &III) {
        let _ = || register_provider_stream::<DummyConfig, _, _>(iii, "dummy", dummy_stream);
    }
}
