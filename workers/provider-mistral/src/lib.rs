//! Mistral Chat Completions streaming via provider-base.

use std::sync::Arc;

use harness_types::{AgentMessage, AgentTool, AssistantMessage, AssistantMessageEvent, StopReason};
use provider_base::{stream_chat_completions, ChatCompletionsConfig, OpenAICompatRequest};
use tokio_stream::wrappers::ReceiverStream;
use tokio_stream::StreamExt;

const API_URL: &str = "https://api.mistral.ai/v1/chat/completions";
const PROVIDER_NAME: &str = "mistral";
const ENV_VAR: &str = "MISTRAL_API_KEY";

#[derive(Debug, Clone)]
pub struct MistralConfig {
    pub api_key: String,
    pub model: String,
    pub max_tokens: u32,
}

impl MistralConfig {
    pub fn from_env(model: impl Into<String>) -> Result<Self, std::env::VarError> {
        let api_key = std::env::var(ENV_VAR)?;
        Ok(Self {
            api_key,
            model: model.into(),
            max_tokens: 4096,
        })
    }
}

pub async fn stream(
    cfg: Arc<MistralConfig>,
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

    // Single test guards against env-var races between parallel test threads.
    #[test]
    fn config_from_env_behavior() {
        let prev = std::env::var(ENV_VAR).ok();
        std::env::remove_var(ENV_VAR);
        assert!(MistralConfig::from_env("test-model").is_err());
        std::env::set_var(ENV_VAR, "test-key");
        let cfg = MistralConfig::from_env("test-model").unwrap();
        assert_eq!(cfg.api_key, "test-key");
        assert_eq!(cfg.model, "test-model");
        assert_eq!(cfg.max_tokens, 4096);
        match prev {
            Some(v) => std::env::set_var(ENV_VAR, v),
            None => std::env::remove_var(ENV_VAR),
        }
    }
}
