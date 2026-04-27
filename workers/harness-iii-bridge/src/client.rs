//! Client abstraction over the iii-engine WebSocket bus.
//!
//! The runtime depends on the [`IiiClientLike`] trait so it can be exercised
//! without a live engine. [`IiiSdkClient`] wraps a real [`iii_sdk::III`]
//! handle. [`NoOpClient`] is a stub used by tests that don't need bus
//! interaction.

use std::sync::Arc;

use async_trait::async_trait;
use iii_sdk::{TriggerRequest, III};
use serde_json::{json, Value};

/// Errors returned by the bridge.
#[derive(Debug, thiserror::Error)]
pub enum BridgeError {
    /// Underlying iii-sdk failure (websocket disconnect, timeout, handler
    /// error). Carries the original message for debugging.
    #[error("iii sdk error: {0}")]
    Sdk(String),

    /// JSON serialisation or deserialisation failure on a payload exchanged
    /// with the engine.
    #[error("serialisation error: {0}")]
    Serde(String),

    /// A function the bridge needed to call returned a payload that did not
    /// match the expected schema.
    #[error("invalid payload from {function_id}: {reason}")]
    InvalidPayload { function_id: String, reason: String },
}

impl From<serde_json::Error> for BridgeError {
    fn from(value: serde_json::Error) -> Self {
        Self::Serde(value.to_string())
    }
}

/// The slice of iii-engine surface area the bridge actually uses.
///
/// Real implementations forward each method to the equivalent
/// engine-builtin function (`state::*`, `publish`, `stream::set`, etc).
/// A test double can record calls and return canned responses.
#[async_trait]
pub trait IiiClientLike: Send + Sync {
    /// Invoke an arbitrary iii function and await its JSON response.
    async fn invoke(&self, function_id: &str, payload: Value) -> Result<Value, BridgeError>;

    /// Read state at `scope`/`key`. Returns `None` if the value is JSON null
    /// or absent.
    async fn state_get(&self, scope: &str, key: &str) -> Result<Option<Value>, BridgeError> {
        let resp = self
            .invoke("state::get", json!({ "scope": scope, "key": key }))
            .await?;
        if resp.is_null() {
            Ok(None)
        } else {
            Ok(Some(resp))
        }
    }

    /// Write `value` at `scope`/`key`, replacing any prior value.
    async fn state_set(&self, scope: &str, key: &str, value: Value) -> Result<(), BridgeError> {
        self.invoke(
            "state::set",
            json!({ "scope": scope, "key": key, "value": value }),
        )
        .await?;
        Ok(())
    }

    /// Delete `scope`/`key`. Idempotent.
    async fn state_delete(&self, scope: &str, key: &str) -> Result<(), BridgeError> {
        self.invoke("state::delete", json!({ "scope": scope, "key": key }))
            .await?;
        Ok(())
    }

    /// Append `data` as a stream item under `stream_name`/`group_id`. The
    /// `item_id` is supplied by the caller to keep the bridge independent of
    /// any randomness source.
    async fn stream_set(
        &self,
        stream_name: &str,
        group_id: &str,
        item_id: &str,
        data: Value,
    ) -> Result<(), BridgeError> {
        self.invoke(
            "stream::set",
            json!({
                "stream_name": stream_name,
                "group_id": group_id,
                "item_id": item_id,
                "data": data,
            }),
        )
        .await?;
        Ok(())
    }

    /// Fire-and-forget publish to a pubsub topic. Subscribers receive `data`.
    async fn publish(&self, topic: &str, data: Value) -> Result<(), BridgeError> {
        self.invoke("publish", json!({ "topic": topic, "data": data }))
            .await?;
        Ok(())
    }

    /// Best-effort collection of subscriber responses for a hook topic.
    ///
    /// iii-sdk 0.11 pubsub is broadcast fire-and-forget — there is no
    /// engine-level mechanism to collect responses from subscribers. The
    /// default implementation publishes and returns an empty vec; tests
    /// (and any future engine that adds collected pubsub) can override to
    /// return the real responses for hook merging.
    async fn publish_collect(
        &self,
        topic: &str,
        data: Value,
        _timeout_ms: u64,
    ) -> Result<Vec<Value>, BridgeError> {
        self.publish(topic, data).await?;
        Ok(Vec::new())
    }

    /// Enumerate all function ids currently registered on the bus.
    async fn list_function_ids(&self) -> Result<Vec<String>, BridgeError>;
}

/// Adapter wrapping a live [`iii_sdk::III`] handle.
#[derive(Clone)]
pub struct IiiSdkClient {
    inner: Arc<III>,
}

impl IiiSdkClient {
    pub fn new(client: Arc<III>) -> Self {
        Self { inner: client }
    }

    pub fn inner(&self) -> &III {
        &self.inner
    }
}

#[async_trait]
impl IiiClientLike for IiiSdkClient {
    async fn invoke(&self, function_id: &str, payload: Value) -> Result<Value, BridgeError> {
        self.inner
            .trigger(TriggerRequest {
                function_id: function_id.to_string(),
                payload,
                action: None,
                timeout_ms: None,
            })
            .await
            .map_err(|e| BridgeError::Sdk(e.to_string()))
    }

    async fn list_function_ids(&self) -> Result<Vec<String>, BridgeError> {
        let infos = self
            .inner
            .list_functions()
            .await
            .map_err(|e| BridgeError::Sdk(e.to_string()))?;
        Ok(infos.into_iter().map(|f| f.function_id).collect())
    }
}

/// Stub client that returns errors for every operation. Useful as a
/// placeholder when constructing types that require an `IiiClientLike` but
/// the test path never exercises the bus.
#[derive(Clone, Default)]
pub struct NoOpClient;

#[async_trait]
impl IiiClientLike for NoOpClient {
    async fn invoke(&self, function_id: &str, _payload: Value) -> Result<Value, BridgeError> {
        Err(BridgeError::Sdk(format!(
            "NoOpClient cannot invoke '{function_id}'"
        )))
    }

    async fn list_function_ids(&self) -> Result<Vec<String>, BridgeError> {
        Ok(Vec::new())
    }
}
