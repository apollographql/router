use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::Mutex;
use tokio::sync::broadcast;
use tokio::sync::mpsc;
use tokio::sync::oneshot;

use super::EventError;
use super::ProviderEvent;
use super::ProviderEventStream;

pub(super) type SharedEvent = Arc<Result<ProviderEvent, EventError>>;

#[derive(Clone, Default)]
pub(super) struct TriggerRegistry {
    triggers: Arc<Mutex<HashMap<EventTrigger, TriggerState>>>,
}

impl TriggerRegistry {
    pub(super) async fn acquire(&self, trigger: EventTrigger) -> Result<TriggerAccess, EventError> {
        loop {
            let waiter = {
                let mut triggers = self.triggers.lock().await;
                match triggers.get_mut(&trigger) {
                    Some(TriggerState::Active(sender)) => {
                        return Ok(TriggerAccess::Shared(sender.subscribe()));
                    }
                    Some(TriggerState::Connecting { waiters, .. }) => {
                        let (sender, receiver) = oneshot::channel();
                        waiters.push(sender);
                        receiver
                    }
                    None => {
                        let token = Arc::new(());
                        triggers.insert(
                            trigger.clone(),
                            TriggerState::Connecting {
                                token: token.clone(),
                                waiters: Vec::new(),
                            },
                        );
                        return Ok(TriggerAccess::Connect(TriggerConnection {
                            registry: self.clone(),
                            trigger,
                            token,
                            armed: true,
                        }));
                    }
                }
            };
            waiter.await.map_err(|_| {
                EventError::new("event provider connection attempt was cancelled")
            })??;
        }
    }

    pub(super) async fn remove_active(
        &self,
        trigger: &EventTrigger,
        sender: &broadcast::Sender<SharedEvent>,
    ) {
        let mut triggers = self.triggers.lock().await;
        if triggers
            .get(trigger)
            .is_some_and(|current| {
                matches!(current, TriggerState::Active(current) if current.same_channel(sender))
            })
        {
            triggers.remove(trigger);
        }
    }

    async fn cancel(&self, trigger: &EventTrigger, token: &Arc<()>) {
        let waiters = {
            let mut triggers = self.triggers.lock().await;
            match triggers.get(trigger) {
                Some(TriggerState::Connecting { token: current, .. })
                    if Arc::ptr_eq(current, token) =>
                {
                    match triggers.remove(trigger) {
                        Some(TriggerState::Connecting { waiters, .. }) => waiters,
                        _ => unreachable!("the connecting trigger was just matched"),
                    }
                }
                _ => return,
            }
        };
        notify_waiters(
            waiters,
            Err(EventError::new(
                "event provider connection attempt was cancelled",
            )),
        );
    }
}

enum TriggerState {
    Connecting {
        token: Arc<()>,
        waiters: Vec<oneshot::Sender<Result<(), EventError>>>,
    },
    Active(broadcast::Sender<SharedEvent>),
}

pub(super) enum TriggerAccess {
    Connect(TriggerConnection),
    Shared(broadcast::Receiver<SharedEvent>),
}

pub(super) struct TriggerConnection {
    registry: TriggerRegistry,
    trigger: EventTrigger,
    token: Arc<()>,
    armed: bool,
}

impl TriggerConnection {
    pub(super) async fn activate(mut self, sender: broadcast::Sender<SharedEvent>) {
        let waiters = {
            let mut triggers = self.registry.triggers.lock().await;
            match triggers.get(&self.trigger) {
                Some(TriggerState::Connecting { token, .. }) if Arc::ptr_eq(token, &self.token) => {
                    match triggers.insert(self.trigger.clone(), TriggerState::Active(sender)) {
                        Some(TriggerState::Connecting { waiters, .. }) => waiters,
                        _ => unreachable!("the connecting trigger was just matched"),
                    }
                }
                _ => return,
            }
        };
        self.armed = false;
        notify_waiters(waiters, Ok(()));
    }

    pub(super) async fn fail(mut self, error: EventError) {
        let waiters = {
            let mut triggers = self.registry.triggers.lock().await;
            match triggers.get(&self.trigger) {
                Some(TriggerState::Connecting { token, .. }) if Arc::ptr_eq(token, &self.token) => {
                    match triggers.remove(&self.trigger) {
                        Some(TriggerState::Connecting { waiters, .. }) => waiters,
                        _ => unreachable!("the connecting trigger was just matched"),
                    }
                }
                _ => return,
            }
        };
        self.armed = false;
        notify_waiters(waiters, Err(error));
    }
}

impl Drop for TriggerConnection {
    fn drop(&mut self) {
        if self.armed {
            let registry = self.registry.clone();
            let trigger = self.trigger.clone();
            let token = self.token.clone();
            tokio::spawn(async move {
                registry.cancel(&trigger, &token).await;
            });
        }
    }
}

fn notify_waiters(
    waiters: Vec<oneshot::Sender<Result<(), EventError>>>,
    result: Result<(), EventError>,
) {
    for waiter in waiters {
        let _ = waiter.send(result.clone());
    }
}

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
    use std::time::Duration;

    use futures::StreamExt;

    use super::*;

    fn trigger(destination: &str) -> EventTrigger {
        EventTrigger {
            provider: "events".to_string(),
            source: "products".to_string(),
            destinations: vec![destination.to_string()],
        }
    }

    fn connection(access: TriggerAccess) -> TriggerConnection {
        match access {
            TriggerAccess::Connect(connection) => connection,
            TriggerAccess::Shared(_) => panic!("expected a new connection"),
        }
    }

    async fn wait_for_waiter(registry: &TriggerRegistry, trigger: &EventTrigger) {
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                let has_waiter = matches!(
                    registry.triggers.lock().await.get(trigger),
                    Some(TriggerState::Connecting { waiters, .. }) if !waiters.is_empty()
                );
                if has_waiter {
                    return;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("a concurrent subscriber registers as a waiter");
    }

    #[tokio::test]
    async fn does_not_hold_the_registry_lock_while_connecting() {
        let registry = TriggerRegistry::default();
        let first = connection(registry.acquire(trigger("products.1")).await.unwrap());

        let second = tokio::time::timeout(
            Duration::from_millis(100),
            registry.acquire(trigger("products.2")),
        )
        .await
        .expect("an unrelated trigger is not blocked")
        .expect("the trigger can be acquired");

        assert!(matches!(second, TriggerAccess::Connect(_)));
        drop(first);
    }

    #[tokio::test]
    async fn coalesces_concurrent_connections_for_the_same_trigger() {
        let registry = TriggerRegistry::default();
        let trigger = trigger("products.updated");
        let connection = connection(registry.acquire(trigger.clone()).await.unwrap());
        let waiting_registry = registry.clone();
        let waiting_trigger = trigger.clone();
        let waiter = tokio::spawn(async move { waiting_registry.acquire(waiting_trigger).await });
        wait_for_waiter(&registry, &trigger).await;

        let (sender, _) = broadcast::channel(4);
        connection.activate(sender).await;

        assert!(matches!(
            waiter.await.unwrap().unwrap(),
            TriggerAccess::Shared(_)
        ));
    }

    #[tokio::test]
    async fn failed_connection_notifies_waiters_and_can_be_retried() {
        let registry = TriggerRegistry::default();
        let trigger = trigger("products.updated");
        let connection = connection(registry.acquire(trigger.clone()).await.unwrap());
        let waiting_registry = registry.clone();
        let waiting_trigger = trigger.clone();
        let waiter = tokio::spawn(async move { waiting_registry.acquire(waiting_trigger).await });
        wait_for_waiter(&registry, &trigger).await;

        connection.fail(EventError::new("connection failed")).await;

        let error = match waiter.await.unwrap() {
            Ok(_) => panic!("the failed connection must be reported"),
            Err(error) => error,
        };
        assert_eq!(error.to_string(), "connection failed");
        assert!(matches!(
            registry.acquire(trigger).await.unwrap(),
            TriggerAccess::Connect(_)
        ));
    }

    #[tokio::test]
    async fn cancelled_connection_notifies_waiters_and_can_be_retried() {
        let registry = TriggerRegistry::default();
        let trigger = trigger("products.updated");
        let connection = connection(registry.acquire(trigger.clone()).await.unwrap());
        let waiting_registry = registry.clone();
        let waiting_trigger = trigger.clone();
        let waiter = tokio::spawn(async move { waiting_registry.acquire(waiting_trigger).await });
        wait_for_waiter(&registry, &trigger).await;

        drop(connection);

        let error = match waiter.await.unwrap() {
            Ok(_) => panic!("the cancelled connection must be reported"),
            Err(error) => error,
        };
        assert_eq!(
            error.to_string(),
            "event provider connection attempt was cancelled"
        );
        assert!(matches!(
            registry.acquire(trigger).await.unwrap(),
            TriggerAccess::Connect(_)
        ));
    }

    #[tokio::test]
    async fn shares_one_trigger_with_every_local_subscriber() {
        let (sender, first) = broadcast::channel(4);
        let trigger = trigger("products.updated");
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
