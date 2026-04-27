//! Test-only fake [`IiiClientLike`] that records every call and lets tests
//! preset responses for state reads, tool invocations, and pubsub fan-out.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};
use tokio::sync::Mutex;

use crate::client::{BridgeError, IiiClientLike};

#[derive(Clone, Debug)]
pub struct StreamWrite {
    pub stream_name: String,
    pub group_id: String,
    pub item_id: String,
    pub data: Value,
}

#[derive(Default)]
struct Inner {
    /// (scope, key) -> current value
    state: HashMap<(String, String), Value>,
    /// "function_id" -> canned response for `invoke`
    invoke_responses: HashMap<String, Value>,
    /// topic -> staged subscriber responses
    topic_responses: HashMap<String, Vec<Value>>,
    /// list of stream::set calls in order
    stream_writes: Vec<StreamWrite>,
    /// list of publish calls (topic, data) in order
    publishes: Vec<(String, Value)>,
    /// function ids visible to `list_function_ids`
    registered_functions: Vec<String>,
}

#[derive(Clone, Default)]
pub struct FakeClient {
    inner: Arc<Mutex<Inner>>,
}

impl FakeClient {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn preset_state(&self, scope: &str, key: &str, value: Value) {
        let mut g = self.inner.lock().await;
        g.state.insert((scope.to_string(), key.to_string()), value);
    }

    pub async fn preset_invoke_response(&self, function_id: &str, response: Value) {
        let mut g = self.inner.lock().await;
        g.invoke_responses.insert(function_id.to_string(), response);
    }

    pub async fn preset_topic_response(&self, topic: &str, responses: Vec<Value>) {
        let mut g = self.inner.lock().await;
        g.topic_responses.insert(topic.to_string(), responses);
    }

    pub async fn add_registered_function(&self, function_id: &str) {
        let mut g = self.inner.lock().await;
        g.registered_functions.push(function_id.to_string());
    }

    pub async fn stream_writes(&self) -> Vec<StreamWrite> {
        self.inner.lock().await.stream_writes.clone()
    }

    pub async fn publishes(&self) -> Vec<(String, Value)> {
        self.inner.lock().await.publishes.clone()
    }
}

#[async_trait]
impl IiiClientLike for FakeClient {
    async fn invoke(&self, function_id: &str, payload: Value) -> Result<Value, BridgeError> {
        match function_id {
            "state::get" => {
                let scope = payload
                    .get("scope")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                let key = payload
                    .get("key")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                let g = self.inner.lock().await;
                Ok(g.state.get(&(scope, key)).cloned().unwrap_or(Value::Null))
            }
            "state::set" => {
                let scope = payload
                    .get("scope")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                let key = payload
                    .get("key")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                let value = payload.get("value").cloned().unwrap_or(Value::Null);
                let mut g = self.inner.lock().await;
                let old = g
                    .state
                    .insert((scope, key), value.clone())
                    .unwrap_or(Value::Null);
                Ok(json!({ "old_value": old, "new_value": value }))
            }
            "state::delete" => {
                let scope = payload
                    .get("scope")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                let key = payload
                    .get("key")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                let mut g = self.inner.lock().await;
                g.state.remove(&(scope, key));
                Ok(json!({ "ok": true }))
            }
            "stream::set" => {
                let stream_name = payload
                    .get("stream_name")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                let group_id = payload
                    .get("group_id")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                let item_id = payload
                    .get("item_id")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                let data = payload.get("data").cloned().unwrap_or(Value::Null);
                let mut g = self.inner.lock().await;
                g.stream_writes.push(StreamWrite {
                    stream_name,
                    group_id,
                    item_id,
                    data,
                });
                Ok(json!({ "ok": true }))
            }
            "publish" => {
                let topic = payload
                    .get("topic")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                let data = payload.get("data").cloned().unwrap_or(Value::Null);
                let mut g = self.inner.lock().await;
                g.publishes.push((topic, data));
                Ok(Value::Null)
            }
            other => {
                let g = self.inner.lock().await;
                g.invoke_responses
                    .get(other)
                    .cloned()
                    .ok_or_else(|| BridgeError::Sdk(format!("no canned response for '{other}'")))
            }
        }
    }

    async fn publish_collect(
        &self,
        topic: &str,
        data: Value,
        _timeout_ms: u64,
    ) -> Result<Vec<Value>, BridgeError> {
        let g = self.inner.lock().await;
        let staged = g.topic_responses.get(topic).cloned().unwrap_or_default();
        drop(g);
        let mut g = self.inner.lock().await;
        g.publishes.push((topic.to_string(), data));
        Ok(staged)
    }

    async fn list_function_ids(&self) -> Result<Vec<String>, BridgeError> {
        Ok(self.inner.lock().await.registered_functions.clone())
    }
}
