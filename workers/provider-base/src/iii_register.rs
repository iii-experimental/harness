//! Shared `register_with_iii` plumbing for provider crates.
//!
//! Each provider crate exposes
//! `pub async fn register_with_iii(iii: &iii_sdk::III) -> anyhow::Result<()>`
//! which publishes `provider::<name>::stream_assistant` on the iii bus. The
//! handler decodes a JSON payload of the shape
//! `{ model, system_prompt, messages, tools }`, builds the per-provider
//! `Config` from environment via the supplied builder closure, calls into
//! the crate's own `pub async fn stream(...)`, drains the resulting
//! [`ReceiverStream<AssistantMessageEvent>`] into a final
//! [`AssistantMessage`], and returns the message as JSON.
//!
//! `iii-sdk` 0.11 returns a single `Value` from a registered function — there
//! is no per-call streaming response surface — so the contract is to collect
//! the event sequence into the terminal `AssistantMessage`. Callers that
//! want incremental events subscribe to `agent::events/<sid>` separately.

use std::future::Future;
use std::sync::Arc;

use harness_types::{
    AgentMessage, AgentTool, AssistantMessage, AssistantMessageEvent, ContentBlock, ErrorKind,
    StopReason, TextContent,
};
use iii_sdk::{FunctionRef, IIIError, RegisterFunctionMessage, III};
use serde_json::Value;
use tokio_stream::wrappers::ReceiverStream;
use tokio_stream::StreamExt;

/// Register `provider::<name>::stream_assistant` as an iii function backed
/// by the supplied `stream_fn`. `build_config` is the per-provider
/// `Config::from_env(model)` adapter, called once per invocation.
///
/// Function pointers and zero-capture closures both implement `Copy`, so the
/// handler can clone-by-copy on every invocation.
pub fn register_provider_stream<C, B, BErr, F, Fut>(
    iii: &III,
    provider_name: &str,
    build_config: B,
    stream_fn: F,
) -> FunctionRef
where
    C: Send + Sync + 'static,
    B: Fn(&str) -> Result<C, BErr> + Copy + Send + Sync + 'static,
    BErr: std::fmt::Display + Send + Sync + 'static,
    F: Fn(Arc<C>, String, Vec<AgentMessage>, Vec<AgentTool>) -> Fut + Copy + Send + Sync + 'static,
    Fut: Future<Output = ReceiverStream<AssistantMessageEvent>> + Send + 'static,
{
    let id = format!("provider::{provider_name}::stream_assistant");
    let description = format!("Stream a response from the {provider_name} provider");
    let provider_label = provider_name.to_string();
    iii.register_function((
        RegisterFunctionMessage::with_id(id).with_description(description),
        move |payload: Value| {
            let provider_label = provider_label.clone();
            async move {
                let model = payload
                    .get("model")
                    .and_then(Value::as_str)
                    .ok_or_else(|| IIIError::Handler("missing required field: model".into()))?
                    .to_string();
                let cfg = build_config(&model)
                    .map_err(|e| IIIError::Handler(format!("config build failed: {e}")))?;
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
                let final_msg = collect_final(&mut s, &provider_label, &model).await;
                serde_json::to_value(final_msg).map_err(|e| IIIError::Handler(e.to_string()))
            }
        },
    ))
}

async fn collect_final(
    stream: &mut ReceiverStream<AssistantMessageEvent>,
    provider: &str,
    model: &str,
) -> AssistantMessage {
    let mut last: Option<AssistantMessage> = None;
    while let Some(ev) = stream.next().await {
        match ev {
            AssistantMessageEvent::Done { message } => return message,
            AssistantMessageEvent::Error { error } => return error,
            AssistantMessageEvent::Start { partial }
            | AssistantMessageEvent::TextStart { partial }
            | AssistantMessageEvent::TextDelta { partial, .. }
            | AssistantMessageEvent::TextEnd { partial }
            | AssistantMessageEvent::ToolcallStart { partial }
            | AssistantMessageEvent::ToolcallDelta { partial, .. }
            | AssistantMessageEvent::ToolcallEnd { partial }
            | AssistantMessageEvent::ThinkingStart { partial }
            | AssistantMessageEvent::ThinkingDelta { partial, .. }
            | AssistantMessageEvent::ThinkingEnd { partial } => {
                last = Some(partial);
            }
            _ => {}
        }
    }
    last.unwrap_or_else(|| AssistantMessage {
        content: vec![ContentBlock::Text(TextContent {
            text: "stream closed without final".into(),
        })],
        stop_reason: StopReason::Error,
        error_message: Some("stream closed without final".into()),
        error_kind: Some(ErrorKind::Transient),
        usage: None,
        model: model.into(),
        provider: provider.into(),
        timestamp: chrono::Utc::now().timestamp_millis(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use harness_types::{ContentBlock, StopReason, TextContent};
    use tokio::sync::mpsc;

    #[allow(dead_code)]
    struct DummyConfig {
        model: String,
    }

    #[allow(dead_code)]
    fn dummy_from_env(model: &str) -> Result<DummyConfig, std::env::VarError> {
        Ok(DummyConfig {
            model: model.to_string(),
        })
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
    /// `(from_env, stream)` pair. Any breakage in the helper signature
    /// surfaces as a `provider-base` build failure rather than cascading
    /// through every provider crate.
    #[allow(dead_code)]
    fn _bounds_witness(iii: &III) {
        let _ = || {
            register_provider_stream::<DummyConfig, _, _, _, _>(
                iii,
                "dummy",
                dummy_from_env,
                dummy_stream,
            )
        };
    }
}
