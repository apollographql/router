use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::Mutex;
use tokio::sync::broadcast;
use tokio::sync::mpsc;

use super::EventError;
use super::ProviderEvent;
use super::ProviderEventStream;

pub(super) type SharedEvent = Arc<Result<ProviderEvent, EventError>>;
pub(super) type TriggerRegistry = Arc<Mutex<HashMap<EventTrigger, broadcast::Sender<SharedEvent>>>>;

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
pub(super) struct EventTrigger {
    pub(super) provider: String,
    pub(super) source: String,
    pub(super) destinations: Vec<String>,
}

pub(super) fn forward_shared_events(
    mut receiver: broadcast::Receiver<SharedEvent>,
    buffer_capacity: usize,
    trigger: EventTrigger,
) -> ProviderEventStream {
    let (sender, stream_receiver) = mpsc::channel(buffer_capacity);
    tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = sender.closed() => break,
                event = receiver.recv() => match event {
                    Ok(event) => {
                        if sender.send(event.as_ref().clone()).await.is_err() {
                            break;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(dropped)) => {
                        u64_counter!(
                            "apollo.router.operations.subscriptions.events.dropped",
                            "Events dropped from a local subscription buffer",
                            dropped,
                            event.provider.name = trigger.provider.clone(),
                            event.source = trigger.source.clone(),
                            reason = "buffer_lag"
                        );
                        tracing::warn!(
                            provider = %trigger.provider,
                            source = %trigger.source,
                            dropped,
                            "event subscription dropped oldest buffered events"
                        );
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        }
    });
    Box::pin(tokio_stream::wrappers::ReceiverStream::new(stream_receiver))
}

#[cfg(test)]
mod tests {
    use futures::StreamExt;

    use super::*;

    #[tokio::test]
    async fn shares_one_trigger_with_every_local_subscriber() {
        let (sender, first) = broadcast::channel(4);
        let trigger = EventTrigger {
            provider: "events".to_string(),
            source: "products".to_string(),
            destinations: vec!["products.updated".to_string()],
        };
        let mut first = forward_shared_events(first, 4, trigger.clone());
        let mut second = forward_shared_events(sender.subscribe(), 4, trigger);
        sender
            .send(Arc::new(Ok(ProviderEvent {
                payload: bytes::Bytes::from_static(br#"{"__typename":"Product","id":"1"}"#),
            })))
            .expect("subscribers are live");

        assert_eq!(
            first.next().await.unwrap().unwrap().payload,
            second.next().await.unwrap().unwrap().payload
        );
    }
}
