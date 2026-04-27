//! Deterministic faux LLM provider for replay tests.
//!
//! Looks up a canned [`AssistantMessage`] by a key derived from the request
//! shape. Tests register the expected mapping ahead of time; the loop drives
//! `stream` and observes the deterministic output.

use std::collections::HashMap;
use std::sync::{Arc, OnceLock, RwLock};

use async_trait::async_trait;
use harness_types::{
    AssistantMessage, AssistantMessageEvent, ContentBlock, StopReason, TextContent,
};
use iii_sdk::{IIIError, RegisterFunctionMessage, III};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// One canned response keyed by a stable string. Test setup builds the map.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CannedResponse {
    /// Final assembled assistant message.
    pub message: AssistantMessage,
    /// Streaming chunks producing the message. Ordered. Loop assembles `done`
    /// as terminal automatically when the registered events end without one.
    pub events: Vec<AssistantMessageEvent>,
}

/// Provider trait used by the loop. Concrete providers (anthropic, openai, ...)
/// implement this; the faux impl supplies replay-friendly sequences.
#[async_trait]
pub trait StreamProvider: Send + Sync {
    async fn stream(&self, key: &str) -> Result<Vec<AssistantMessageEvent>, ProviderError>;
}

#[derive(Debug, thiserror::Error)]
pub enum ProviderError {
    #[error("no canned response for key: {0}")]
    NotFound(String),
}

/// In-memory faux provider. Tests register canned responses by key.
#[derive(Debug, Clone, Default)]
pub struct FauxProvider {
    inner: Arc<RwLock<HashMap<String, CannedResponse>>>,
}

impl FauxProvider {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a canned response. Overwrites prior entries for the same key.
    pub fn register(&self, key: impl Into<String>, response: CannedResponse) {
        if let Ok(mut g) = self.inner.write() {
            g.insert(key.into(), response);
        }
    }
}

#[async_trait]
impl StreamProvider for FauxProvider {
    async fn stream(&self, key: &str) -> Result<Vec<AssistantMessageEvent>, ProviderError> {
        let inner = self
            .inner
            .read()
            .map_err(|_| ProviderError::NotFound(key.to_string()))?;
        let canned = inner
            .get(key)
            .ok_or_else(|| ProviderError::NotFound(key.to_string()))?;
        Ok(canned.events.clone())
    }
}

/// Build a minimal text-only canned response: text_start, text_delta, text_end,
/// stop, done. Used by the simplest replay fixtures.
pub fn text_only(text: &str, model: &str, provider: &str, timestamp: i64) -> CannedResponse {
    let final_message = AssistantMessage {
        content: vec![ContentBlock::Text(TextContent {
            text: text.to_string(),
        })],
        stop_reason: StopReason::End,
        error_message: None,
        error_kind: None,
        usage: None,
        model: model.to_string(),
        provider: provider.to_string(),
        timestamp,
    };
    let partial_empty = AssistantMessage {
        content: Vec::new(),
        ..final_message.clone()
    };
    let events = vec![
        AssistantMessageEvent::Start {
            partial: partial_empty.clone(),
        },
        AssistantMessageEvent::TextStart {
            partial: partial_empty,
        },
        AssistantMessageEvent::TextDelta {
            partial: final_message.clone(),
            delta: text.to_string(),
        },
        AssistantMessageEvent::TextEnd {
            partial: final_message.clone(),
        },
        AssistantMessageEvent::Stop {
            stop_reason: StopReason::End,
            error_message: None,
            error_kind: None,
        },
        AssistantMessageEvent::Done {
            message: final_message.clone(),
        },
    ];
    CannedResponse {
        message: final_message,
        events,
    }
}

/// Process-wide [`FauxProvider`] used by [`register_with_iii`]. Tests install
/// canned responses via [`shared_provider`] before driving the iii function.
fn shared_provider() -> &'static FauxProvider {
    static GLOBAL: OnceLock<FauxProvider> = OnceLock::new();
    GLOBAL.get_or_init(FauxProvider::new)
}

/// Convenience: install a canned response on the shared provider used by the
/// iii-registered `provider::faux::stream` function.
pub fn register_canned(key: impl Into<String>, response: CannedResponse) {
    shared_provider().register(key, response);
}

/// Register `provider::faux::stream` on the iii bus.
///
/// The handler accepts `{ key }` and returns
/// `{ events: [<AssistantMessageEvent>...] }` from the shared
/// [`FauxProvider`]. Faux deviates from the standard provider shape because
/// it has no `(config, system_prompt, messages, tools)` surface — the reply
/// is keyed by an opaque test fixture identifier.
pub async fn register_with_iii(iii: &III) -> anyhow::Result<()> {
    iii.register_function((
        RegisterFunctionMessage::with_id("provider::faux::stream".to_string())
            .with_description("Replay a canned faux provider response keyed by `key`".to_string()),
        move |payload: Value| async move {
            let key = payload
                .get("key")
                .and_then(Value::as_str)
                .ok_or_else(|| IIIError::Handler("missing required field: key".into()))?
                .to_string();
            let events = shared_provider()
                .stream(&key)
                .await
                .map_err(|e| IIIError::Handler(e.to_string()))?;
            serde_json::to_value(serde_json::json!({ "events": events }))
                .map_err(|e| IIIError::Handler(e.to_string()))
        },
    ));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn registered_key_returns_events() {
        let p = FauxProvider::new();
        p.register("k1", text_only("hello", "test-model", "test", 100));
        let events = p.stream("k1").await.unwrap();
        assert_eq!(events.len(), 6);
        assert!(matches!(
            events.last().unwrap(),
            AssistantMessageEvent::Done { .. }
        ));
    }

    #[tokio::test]
    async fn unknown_key_errors() {
        let p = FauxProvider::new();
        let err = p.stream("missing").await.unwrap_err();
        assert!(matches!(err, ProviderError::NotFound(_)));
    }

    #[test]
    fn text_only_assembles_final_message() {
        let canned = text_only("hi", "m", "p", 1);
        assert_eq!(canned.message.provider, "p");
        assert!(matches!(canned.message.stop_reason, StopReason::End));
    }
}
