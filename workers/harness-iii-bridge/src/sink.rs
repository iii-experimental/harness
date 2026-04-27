//! `EventSink` that forwards [`AgentEvent`] writes to the iii event stream.
//!
//! Each event is appended as a stream item under
//! `stream_name = "agent::events"`, `group_id = <session_id>`. UIs and
//! observers subscribe to the same stream group and replay the loop
//! verbatim.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use harness_runtime::EventSink;
use harness_types::AgentEvent;

use crate::client::IiiClientLike;

/// Stream name for agent events. Matches the spec in `ARCHITECTURE.md`.
pub const EVENTS_STREAM: &str = "agent::events";

/// Forwards events for a single session to the engine event stream.
pub struct IiiEventSink<C: IiiClientLike + 'static> {
    client: Arc<C>,
    session_id: String,
    counter: AtomicU64,
}

impl<C: IiiClientLike + 'static> IiiEventSink<C> {
    pub fn new(client: Arc<C>, session_id: impl Into<String>) -> Self {
        Self {
            client,
            session_id: session_id.into(),
            counter: AtomicU64::new(0),
        }
    }

    /// Generate a deterministic, monotonically-increasing item id. Format:
    /// `<session_id>-<seq>`. Stable enough that consumers can sort items
    /// without relying on engine-side timestamps.
    fn next_item_id(&self) -> String {
        let seq = self.counter.fetch_add(1, Ordering::Relaxed);
        format!("{}-{seq:08}", self.session_id)
    }
}

#[async_trait]
impl<C: IiiClientLike + 'static> EventSink for IiiEventSink<C> {
    async fn emit(&self, event: AgentEvent) {
        let payload = match serde_json::to_value(&event) {
            Ok(v) => v,
            Err(err) => {
                tracing::warn!(?err, "failed to serialise AgentEvent; dropping");
                return;
            }
        };
        let item_id = self.next_item_id();
        if let Err(err) = self
            .client
            .stream_set(EVENTS_STREAM, &self.session_id, &item_id, payload)
            .await
        {
            tracing::warn!(?err, session_id = %self.session_id, "stream::set failed");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::FakeClient;
    use harness_types::AgentEvent;
    use serde_json::json;

    #[test]
    fn agent_event_wire_format_is_stable() {
        let ev = AgentEvent::AgentStart;
        let json = serde_json::to_value(&ev).unwrap();
        assert_eq!(json, json!({ "type": "agent_start" }));
        let back: AgentEvent = serde_json::from_value(json).unwrap();
        assert_eq!(back, AgentEvent::AgentStart);
    }

    #[tokio::test]
    async fn emit_writes_to_stream_under_session_group() {
        let client = Arc::new(FakeClient::new());
        let sink = IiiEventSink::new(client.clone(), "s1");
        sink.emit(AgentEvent::AgentStart).await;
        sink.emit(AgentEvent::TurnStart).await;
        let writes = client.stream_writes().await;
        assert_eq!(writes.len(), 2);
        assert_eq!(writes[0].stream_name, EVENTS_STREAM);
        assert_eq!(writes[0].group_id, "s1");
        assert_eq!(writes[0].item_id, "s1-00000000");
        assert_eq!(writes[1].item_id, "s1-00000001");
        assert_eq!(writes[0].data, json!({ "type": "agent_start" }));
    }
}
