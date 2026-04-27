//! Vercel AI Gateway Chat Completions streaming via provider-base.

use std::sync::Arc;

use harness_types::{AgentMessage, AgentTool, AssistantMessage, AssistantMessageEvent, StopReason};
use provider_base::{stream_chat_completions, ChatCompletionsConfig, OpenAICompatRequest};
use tokio_stream::wrappers::ReceiverStream;
use tokio_stream::StreamExt;

// TODO: confirm endpoint with provider docs
const API_URL: &str = "https://gateway.ai.cloudflare.com/v1/openai-compat/chat/completions";
const PROVIDER_NAME: &str = "vercel-ai-gateway";

#[derive(Debug, Clone)]
pub struct VercelAiGatewayConfig {
    pub api_key: String,
    pub model: String,
    pub max_tokens: u32,
}

impl VercelAiGatewayConfig {
    pub fn from_env(model: impl Into<String>) -> Result<Self, std::env::VarError> {
        let api_key = std::env::var("VERCEL_AI_GATEWAY_API_KEY")?;
        Ok(Self {
            api_key,
            model: model.into(),
            max_tokens: 4096,
        })
    }
}

pub async fn stream(
    cfg: Arc<VercelAiGatewayConfig>,
    system_prompt: String,
    messages: Vec<AgentMessage>,
    tools: Vec<AgentTool>,
) -> ReceiverStream<AssistantMessageEvent> {
    let base = ChatCompletionsConfig::new(
        API_URL,
        PROVIDER_NAME,
        cfg.model.clone(),
        cfg.api_key.clone(),
    )
    .with_max_tokens(cfg.max_tokens);
    let req = OpenAICompatRequest {
        system_prompt,
        messages,
        tools,
    };
    stream_chat_completions(Arc::new(base), req).await
}

pub async fn collect(mut stream: ReceiverStream<AssistantMessageEvent>) -> AssistantMessage {
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
            | AssistantMessageEvent::ThinkingEnd { partial } => last = Some(partial),
            _ => {}
        }
    }
    last.unwrap_or_else(|| AssistantMessage {
        content: vec![],
        stop_reason: StopReason::Error,
        error_message: Some("stream closed without final".into()),
        error_kind: Some(harness_types::ErrorKind::Transient),
        usage: None,
        model: "unknown".into(),
        provider: PROVIDER_NAME.into(),
        timestamp: chrono::Utc::now().timestamp_millis(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Combined env-mutating test. Cargo runs tests in parallel within a crate,
    /// so splitting these caused a race on `VERCEL_AI_GATEWAY_API_KEY`. Coverage is identical
    /// when sequenced.
    #[test]
    fn config_env_resolution() {
        let prev = std::env::var("VERCEL_AI_GATEWAY_API_KEY").ok();

        std::env::remove_var("VERCEL_AI_GATEWAY_API_KEY");
        assert!(VercelAiGatewayConfig::from_env("test-model").is_err());

        std::env::set_var("VERCEL_AI_GATEWAY_API_KEY", "test-key");
        let cfg = VercelAiGatewayConfig::from_env("test-model").expect("ok");
        assert_eq!(cfg.model, "test-model");
        assert_eq!(cfg.max_tokens, 4096);

        match prev {
            Some(v) => std::env::set_var("VERCEL_AI_GATEWAY_API_KEY", v),
            None => std::env::remove_var("VERCEL_AI_GATEWAY_API_KEY"),
        }
    }
}
