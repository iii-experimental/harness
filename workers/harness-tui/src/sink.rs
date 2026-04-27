//! `EventSink` implementation that pushes [`harness_types::AgentEvent`] values
//! into a tokio mpsc channel for the TUI to drain on each tick.

use async_trait::async_trait;
use harness_runtime::EventSink;
use harness_types::AgentEvent;
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};

/// Sink wrapper around an `UnboundedSender<AgentEvent>`. Drops on send error
/// (i.e. when the receiver has been closed) so the loop never blocks on UI
/// teardown.
#[derive(Debug, Clone)]
pub struct ChannelSink {
    tx: UnboundedSender<AgentEvent>,
}

impl ChannelSink {
    pub fn new() -> (Self, UnboundedReceiver<AgentEvent>) {
        let (tx, rx) = unbounded_channel();
        (Self { tx }, rx)
    }
}

#[async_trait]
impl EventSink for ChannelSink {
    async fn emit(&self, event: AgentEvent) {
        let _ = self.tx.send(event);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn emit_pushes_to_receiver() {
        let (sink, mut rx) = ChannelSink::new();
        sink.emit(AgentEvent::AgentStart).await;
        let ev = rx.recv().await.expect("event present");
        assert!(matches!(ev, AgentEvent::AgentStart));
    }

    #[tokio::test]
    async fn drop_receiver_does_not_panic() {
        let (sink, rx) = ChannelSink::new();
        drop(rx);
        sink.emit(AgentEvent::AgentStart).await;
    }
}
