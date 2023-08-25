//! Internal pub/sub facility for subscription
use std::collections::HashMap;
use std::fmt::Debug;
use std::hash::Hash;
use std::pin::Pin;
use std::sync::Arc;
use std::task::Context;
use std::task::Poll;
use std::time::Duration;
use std::time::Instant;

use futures::channel::mpsc;
use futures::channel::mpsc::SendError;
use futures::channel::oneshot;
use futures::channel::oneshot::Canceled;
use futures::Sink;
use futures::SinkExt;
use futures::Stream;
use futures::StreamExt;
use pin_project_lite::pin_project;
use thiserror::Error;
use tokio::sync::broadcast;
use tokio_stream::wrappers::errors::BroadcastStreamRecvError;
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::wrappers::IntervalStream;

use crate::graphql;
use crate::spec::Schema;
use crate::Configuration;

static NOTIFY_CHANNEL_SIZE: usize = 1024;
static DEFAULT_MSG_CHANNEL_SIZE: usize = 128;

#[derive(Error, Debug)]
pub(crate) enum NotifyError<V> {
    #[error("cannot send data to pubsub")]
    SendError(#[from] SendError),
    #[error("cannot send data to response stream")]
    BroadcastSendError(#[from] broadcast::error::SendError<V>),
    #[error("cannot send data to pubsub because channel has been closed")]
    Canceled(#[from] Canceled),
    #[error("this topic doesn't exist")]
    UnknownTopic,
}

type ResponseSender<V> =
    oneshot::Sender<Option<(broadcast::Sender<Option<V>>, broadcast::Receiver<Option<V>>)>>;

type ResponseSenderWithCreated<V> = oneshot::Sender<(
    broadcast::Sender<Option<V>>,
    broadcast::Receiver<Option<V>>,
    bool,
)>;

enum Notification<K, V> {
    CreateOrSubscribe {
        topic: K,
        // Sender connected to the original source stream
        msg_sender: broadcast::Sender<Option<V>>,
        // To know if it has been created or re-used
        response_sender: ResponseSenderWithCreated<V>,
        heartbeat_enabled: bool,
    },
    Subscribe {
        topic: K,
        // Oneshot channel to fetch the receiver
        response_sender: ResponseSender<V>,
    },
    SubscribeIfExist {
        topic: K,
        // Oneshot channel to fetch the receiver
        response_sender: ResponseSender<V>,
    },
    Unsubscribe {
        topic: K,
    },
    ForceDelete {
        topic: K,
    },
    Exist {
        topic: K,
        response_sender: oneshot::Sender<bool>,
    },
    InvalidIds {
        topics: Vec<K>,
        response_sender: oneshot::Sender<(Vec<K>, Vec<K>)>,
    },
    #[cfg(test)]
    TryDelete {
        topic: K,
    },
    #[cfg(test)]
    Broadcast {
        data: V,
    },
    #[cfg(test)]
    Debug {
        // Returns the number of subscriptions and subscribers
        response_sender: oneshot::Sender<usize>,
    },
}

/// In memory pub/sub implementation
#[derive(Clone)]
pub struct Notify<K, V> {
    sender: mpsc::Sender<Notification<K, V>>,
    /// Size (number of events) of the channel to receive message
    pub(crate) queue_size: Option<usize>,
    router_broadcasts: Arc<RouterBroadcasts>,
}

#[buildstructor::buildstructor]
impl<K, V> Notify<K, V>
where
    K: Send + Hash + Eq + Clone + 'static,
    V: Send + Sync + Clone + 'static,
{
    #[builder]
    pub(crate) fn new(
        ttl: Option<Duration>,
        heartbeat_error_message: Option<V>,
        queue_size: Option<usize>,
        router_broadcasts: Option<Arc<RouterBroadcasts>>,
    ) -> Notify<K, V> {
        let (sender, receiver) = mpsc::channel(NOTIFY_CHANNEL_SIZE);
        tokio::task::spawn(task(receiver, ttl, heartbeat_error_message));
        Notify {
            sender,
            queue_size,
            router_broadcasts: router_broadcasts
                .unwrap_or_else(|| Arc::new(RouterBroadcasts::new())),
        }
    }

    #[doc(hidden)]
    /// NOOP notifier for tests
    pub fn for_tests() -> Self {
        let (sender, _receiver) = mpsc::channel(NOTIFY_CHANNEL_SIZE);
        Notify {
            sender,
            queue_size: None,
            router_broadcasts: Arc::new(RouterBroadcasts::new()),
        }
    }
}

impl<K, V> Notify<K, V> {
    /// Broadcast a new configuration
    pub(crate) fn broadcast_configuration(&self, configuration: Arc<Configuration>) {
        self.router_broadcasts.configuration.0.send(configuration).expect("cannot send the configuration update to the static channel. Should not happen because the receiver will always live in this struct; qed");
    }
    /// Receive the new configuration everytime we have a new router configuration
    pub(crate) fn subscribe_configuration(&self) -> impl Stream<Item = Arc<Configuration>> {
        self.router_broadcasts.subscribe_configuration()
    }
    /// Receive the new schema everytime we have a new schema
    pub(crate) fn broadcast_schema(&self, schema: Arc<Schema>) {
        self.router_broadcasts.schema.0.send(schema).expect("cannot send the schema update to the static channel. Should not happen because the receiver will always live in this struct; qed");
    }
    /// Receive the new schema everytime we have a new schema
    pub(crate) fn subscribe_schema(&self) -> impl Stream<Item = Arc<Schema>> {
        self.router_broadcasts.subscribe_schema()
    }
}

impl<K, V> Notify<K, V>
where
    K: Send + Hash + Eq + Clone + 'static,
    V: Send + Clone + 'static,
{
    #[cfg(not(test))]
    pub(crate) fn set_queue_size(mut self, queue_size: Option<usize>) -> Self {
        self.queue_size = queue_size;
        self
    }

    // boolean in the tuple means `created`
    pub(crate) async fn create_or_subscribe(
        &mut self,
        topic: K,
        heartbeat_enabled: bool,
    ) -> Result<(Handle<K, V>, bool), NotifyError<V>> {
        let (sender, _receiver) =
            broadcast::channel(self.queue_size.unwrap_or(DEFAULT_MSG_CHANNEL_SIZE));

        let (tx, rx) = oneshot::channel();
        self.sender
            .send(Notification::CreateOrSubscribe {
                topic: topic.clone(),
                msg_sender: sender,
                response_sender: tx,
                heartbeat_enabled,
            })
            .await?;

        let (msg_sender, msg_receiver, created) = rx.await?;
        let handle = Handle::new(
            topic,
            self.sender.clone(),
            msg_sender,
            BroadcastStream::from(msg_receiver),
        );

        Ok((handle, created))
    }

    pub(crate) async fn subscribe(&mut self, topic: K) -> Result<Handle<K, V>, NotifyError<V>> {
        let (sender, receiver) = oneshot::channel();

        self.sender
            .send(Notification::Subscribe {
                topic: topic.clone(),
                response_sender: sender,
            })
            .await?;

        let Some((msg_sender, msg_receiver)) = receiver.await? else {
            return Err(NotifyError::UnknownTopic);
        };
        let handle = Handle::new(
            topic,
            self.sender.clone(),
            msg_sender,
            BroadcastStream::from(msg_receiver),
        );

        Ok(handle)
    }

    pub(crate) async fn subscribe_if_exist(
        &mut self,
        topic: K,
    ) -> Result<Option<Handle<K, V>>, NotifyError<V>> {
        let (sender, receiver) = oneshot::channel();

        self.sender
            .send(Notification::SubscribeIfExist {
                topic: topic.clone(),
                response_sender: sender,
            })
            .await?;

        let Some((msg_sender, msg_receiver)) = receiver.await? else {
            return Ok(None);
        };
        let handle = Handle::new(
            topic,
            self.sender.clone(),
            msg_sender,
            BroadcastStream::from(msg_receiver),
        );

        Ok(handle.into())
    }

    pub(crate) async fn exist(&mut self, topic: K) -> Result<bool, NotifyError<V>> {
        // Channel to check if the topic still exists or not
        let (response_tx, response_rx) = oneshot::channel();

        self.sender
            .send(Notification::Exist {
                topic,
                response_sender: response_tx,
            })
            .await?;

        let resp = response_rx.await?;

        Ok(resp)
    }

    pub(crate) async fn invalid_ids(
        &mut self,
        topics: Vec<K>,
    ) -> Result<(Vec<K>, Vec<K>), NotifyError<V>> {
        // Channel to check if the topic still exists or not
        let (response_tx, response_rx) = oneshot::channel();

        self.sender
            .send(Notification::InvalidIds {
                topics,
                response_sender: response_tx,
            })
            .await?;

        let resp = response_rx.await?;

        Ok(resp)
    }

    /// Delete the topic even if several subscribers are still listening
    pub(crate) async fn force_delete(&mut self, topic: K) -> Result<(), NotifyError<V>> {
        // if disconnected, we don't care (the task was stopped)
        self.sender
            .send(Notification::ForceDelete { topic })
            .await
            .map_err(std::convert::Into::into)
    }

    /// Delete the topic if and only if one or zero subscriber is still listening
    /// This function is not async to allow it to be used in a Drop impl
    #[cfg(test)]
    pub(crate) fn try_delete(&mut self, topic: K) -> Result<(), NotifyError<V>> {
        // if disconnected, we don't care (the task was stopped)
        self.sender
            .try_send(Notification::TryDelete { topic })
            .map_err(|try_send_error| try_send_error.into_send_error().into())
    }

    #[cfg(test)]
    pub(crate) async fn broadcast(&mut self, data: V) -> Result<(), NotifyError<V>> {
        self.sender
            .send(Notification::Broadcast { data })
            .await
            .map_err(std::convert::Into::into)
    }

    #[cfg(test)]
    pub(crate) async fn debug(&mut self) -> Result<usize, NotifyError<V>> {
        let (response_tx, response_rx) = oneshot::channel();
        self.sender
            .send(Notification::Debug {
                response_sender: response_tx,
            })
            .await?;

        Ok(response_rx.await.unwrap())
    }
}

#[cfg(test)]
impl<K, V> Default for Notify<K, V>
where
    K: Send + Hash + Eq + Clone + 'static,
    V: Send + Sync + Clone + 'static,
{
    /// Useless notify mainly for test
    fn default() -> Self {
        Self::for_tests()
    }
}

impl<K, V> Debug for Notify<K, V> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Notify").finish()
    }
}

struct HandleGuard<K, V>
where
    K: Clone,
{
    topic: K,
    pubsub_sender: mpsc::Sender<Notification<K, V>>,
}

impl<K, V> Clone for HandleGuard<K, V>
where
    K: Clone,
{
    fn clone(&self) -> Self {
        Self {
            topic: self.topic.clone(),
            pubsub_sender: self.pubsub_sender.clone(),
        }
    }
}

impl<K, V> Drop for HandleGuard<K, V>
where
    K: Clone,
{
    fn drop(&mut self) {
        let err = self.pubsub_sender.try_send(Notification::Unsubscribe {
            topic: self.topic.clone(),
        });
        if let Err(err) = err {
            tracing::trace!("cannot unsubscribe {err:?}");
        }
    }
}

pin_project! {
pub struct Handle<K, V>
where
    K: Clone,
{
    handle_guard: HandleGuard<K, V>,
    #[pin]
    msg_sender: broadcast::Sender<Option<V>>,
    #[pin]
    msg_receiver: BroadcastStream<Option<V>>,
}
}

impl<K, V> Clone for Handle<K, V>
where
    K: Clone,
    V: Clone + Send + 'static,
{
    fn clone(&self) -> Self {
        Self {
            handle_guard: self.handle_guard.clone(),
            msg_receiver: BroadcastStream::new(self.msg_sender.subscribe()),
            msg_sender: self.msg_sender.clone(),
        }
    }
}

impl<K, V> Handle<K, V>
where
    K: Clone,
{
    fn new(
        topic: K,
        pubsub_sender: mpsc::Sender<Notification<K, V>>,
        msg_sender: broadcast::Sender<Option<V>>,
        msg_receiver: BroadcastStream<Option<V>>,
    ) -> Self {
        Self {
            handle_guard: HandleGuard {
                topic,
                pubsub_sender,
            },
            msg_sender,
            msg_receiver,
        }
    }

    pub(crate) fn into_stream(self) -> HandleStream<K, V> {
        HandleStream {
            handle_guard: self.handle_guard,
            msg_receiver: self.msg_receiver,
        }
    }

    pub(crate) fn into_sink(self) -> HandleSink<K, V> {
        HandleSink {
            handle_guard: self.handle_guard,
            msg_sender: self.msg_sender,
        }
    }

    /// Return a sink and a stream
    pub fn split(self) -> (HandleSink<K, V>, HandleStream<K, V>) {
        (
            HandleSink {
                handle_guard: self.handle_guard.clone(),
                msg_sender: self.msg_sender,
            },
            HandleStream {
                handle_guard: self.handle_guard,
                msg_receiver: self.msg_receiver,
            },
        )
    }
}

pin_project! {
pub struct HandleStream<K, V>
where
    K: Clone,
{
    handle_guard: HandleGuard<K, V>,
    #[pin]
    msg_receiver: BroadcastStream<Option<V>>,
}
}

impl<K, V> Stream for HandleStream<K, V>
where
    K: Clone,
    V: Clone + 'static + Send,
{
    type Item = V;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let mut this = self.as_mut().project();

        match Pin::new(&mut this.msg_receiver).poll_next(cx) {
            Poll::Ready(Some(Err(BroadcastStreamRecvError::Lagged(_)))) => {
                tracing::info!(monotonic_counter.apollo_router_skipped_event_count = 1u64,);
                self.poll_next(cx)
            }
            Poll::Ready(None) => Poll::Ready(None),
            Poll::Ready(Some(Ok(Some(val)))) => Poll::Ready(Some(val)),
            Poll::Ready(Some(Ok(None))) => Poll::Ready(None),
            Poll::Pending => Poll::Pending,
        }
    }
}

pin_project! {
pub struct HandleSink<K, V>
where
    K: Clone,
{
    handle_guard: HandleGuard<K, V>,
    #[pin]
    msg_sender: broadcast::Sender<Option<V>>,
}
}

impl<K, V> HandleSink<K, V>
where
    K: Clone,
    V: Clone + 'static + Send,
{
    /// Send data to the subscribed topic
    pub(crate) fn send_sync(&mut self, data: V) -> Result<(), NotifyError<V>> {
        self.msg_sender.send(data.into()).map_err(|err| {
            NotifyError::BroadcastSendError(broadcast::error::SendError(err.0.unwrap()))
        })?;

        Ok(())
    }
}

impl<K, V> Sink<V> for HandleSink<K, V>
where
    K: Clone,
    V: Clone + 'static + Send,
{
    type Error = graphql::Error;

    fn poll_ready(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn start_send(self: Pin<&mut Self>, item: V) -> Result<(), Self::Error> {
        self.msg_sender.send(Some(item)).map_err(|_err| {
            graphql::Error::builder()
                .message("cannot send payload through pubsub")
                .extension_code("NOTIFICATION_HANDLE_SEND_ERROR")
                .build()
        })?;
        Ok(())
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn poll_close(
        mut self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
    ) -> Poll<Result<(), Self::Error>> {
        let topic = self.handle_guard.topic.clone();
        let _ = self
            .handle_guard
            .pubsub_sender
            .try_send(Notification::ForceDelete { topic });
        Poll::Ready(Ok(()))
    }
}

impl<K, V> Handle<K, V> where K: Clone {}

async fn task<K, V>(
    mut receiver: mpsc::Receiver<Notification<K, V>>,
    ttl: Option<Duration>,
    heartbeat_error_message: Option<V>,
) where
    K: Send + Hash + Eq + Clone + 'static,
    V: Send + Clone + 'static,
{
    let mut pubsub: PubSub<K, V> = PubSub::new(ttl);

    let mut ttl_fut: Box<dyn Stream<Item = tokio::time::Instant> + Send + Unpin> = match ttl {
        Some(ttl) => Box::new(IntervalStream::new(tokio::time::interval(ttl))),
        None => Box::new(tokio_stream::pending()),
    };

    loop {
        tokio::select! {
            _ = ttl_fut.next() => {
                let heartbeat_error_message = heartbeat_error_message.clone();
                pubsub.kill_dead_topics(heartbeat_error_message).await;
                tracing::info!(
                    value.apollo_router_opened_subscriptions = pubsub.subscriptions.len() as u64,
                );
            }
            message = receiver.next() => {
                match message {
                    Some(message) => {
                        match message {
                            Notification::Unsubscribe { topic } => pubsub.unsubscribe(topic),
                            Notification::ForceDelete { topic } => pubsub.force_delete(topic),
                            Notification::CreateOrSubscribe { topic,  msg_sender, response_sender, heartbeat_enabled } => {
                                pubsub.subscribe_or_create(topic, msg_sender, response_sender, heartbeat_enabled);
                            }
                            Notification::Subscribe {
                                topic,
                                response_sender,
                            } => {
                                pubsub.subscribe(topic, response_sender);
                            }
                            Notification::SubscribeIfExist {
                                topic,
                                response_sender,
                            } => {
                                if pubsub.is_used(&topic) {
                                    pubsub.subscribe(topic, response_sender);
                                } else {
                                    pubsub.force_delete(topic);
                                    let _ = response_sender.send(None);
                                }
                            }
                            Notification::InvalidIds {
                                topics,
                                response_sender,
                            } => {
                                let invalid_topics = pubsub.invalid_topics(topics);
                                let _ = response_sender.send(invalid_topics);
                            }
                            Notification::Exist {
                                topic,
                                response_sender,
                            } => {
                                let exist = pubsub.exist(&topic);
                                let _ = response_sender.send(exist);
                                if exist {
                                    pubsub.touch(&topic);
                                }
                            }
                            #[cfg(test)]
                            Notification::TryDelete { topic } => pubsub.try_delete(topic),
                            #[cfg(test)]
                            Notification::Broadcast { data } => {
                                pubsub.broadcast(data).await;
                            }
                            #[cfg(test)]
                            Notification::Debug { response_sender } => {
                                let _ = response_sender.send(pubsub.subscriptions.len());
                            }
                        }
                    },
                    None => break,
                }
            }
        }
    }
}

#[derive(Debug)]
struct Subscription<V> {
    msg_sender: broadcast::Sender<Option<V>>,
    heartbeat_enabled: bool,
    updated_at: Instant,
}

impl<V> Subscription<V> {
    fn new(msg_sender: broadcast::Sender<Option<V>>, heartbeat_enabled: bool) -> Self {
        Self {
            msg_sender,
            heartbeat_enabled,
            updated_at: Instant::now(),
        }
    }
    // Update the updated_at value
    fn touch(&mut self) {
        self.updated_at = Instant::now();
    }
}

struct PubSub<K, V>
where
    K: Hash + Eq,
{
    subscriptions: HashMap<K, Subscription<V>>,
    ttl: Option<Duration>,
}

impl<K, V> Default for PubSub<K, V>
where
    K: Hash + Eq,
{
    fn default() -> Self {
        Self {
            // subscribers: HashMap::new(),
            subscriptions: HashMap::new(),
            ttl: None,
        }
    }
}

impl<K, V> PubSub<K, V>
where
    K: Hash + Eq + Clone,
    V: Clone + 'static,
{
    fn new(ttl: Option<Duration>) -> Self {
        Self {
            subscriptions: HashMap::new(),
            ttl,
        }
    }

    fn create_topic(
        &mut self,
        topic: K,
        sender: broadcast::Sender<Option<V>>,
        heartbeat_enabled: bool,
    ) {
        self.subscriptions
            .insert(topic, Subscription::new(sender, heartbeat_enabled));
    }

    fn subscribe(&mut self, topic: K, sender: ResponseSender<V>) {
        match self.subscriptions.get_mut(&topic) {
            Some(subscription) => {
                let _ = sender.send(Some((
                    subscription.msg_sender.clone(),
                    subscription.msg_sender.subscribe(),
                )));
            }
            None => {
                let _ = sender.send(None);
            }
        }
    }

    fn subscribe_or_create(
        &mut self,
        topic: K,
        msg_sender: broadcast::Sender<Option<V>>,
        sender: ResponseSenderWithCreated<V>,
        heartbeat_enabled: bool,
    ) {
        match self.subscriptions.get(&topic) {
            Some(subscription) => {
                let _ = sender.send((
                    subscription.msg_sender.clone(),
                    subscription.msg_sender.subscribe(),
                    false,
                ));
            }
            None => {
                self.create_topic(topic, msg_sender.clone(), heartbeat_enabled);

                let _ = sender.send((msg_sender.clone(), msg_sender.subscribe(), true));
            }
        }
    }

    fn unsubscribe(&mut self, topic: K) {
        let mut topic_to_delete = false;
        match self.subscriptions.get(&topic) {
            Some(subscription) => {
                topic_to_delete = subscription.msg_sender.receiver_count() == 0;
            }
            None => tracing::trace!("Cannot find the subscription to unsubscribe"),
        }
        if topic_to_delete {
            self.subscriptions.remove(&topic);
        };
    }

    /// Check if the topic is used by anyone else than the current handle
    fn is_used(&self, topic: &K) -> bool {
        self.subscriptions
            .get(topic)
            .map(|s| s.msg_sender.receiver_count() > 0)
            .unwrap_or_default()
    }

    /// Update the heartbeat
    fn touch(&mut self, topic: &K) {
        if let Some(sub) = self.subscriptions.get_mut(topic) {
            sub.touch();
        }
    }

    /// Check if the topic exists
    fn exist(&self, topic: &K) -> bool {
        self.subscriptions.contains_key(topic)
    }

    /// Given a list of topics, returns the list of valid and invalid topics
    /// Heartbeat the given valid topics
    fn invalid_topics(&mut self, topics: Vec<K>) -> (Vec<K>, Vec<K>) {
        topics.into_iter().fold(
            (Vec::new(), Vec::new()),
            |(mut valid_ids, mut invalid_ids), e| {
                match self.subscriptions.get_mut(&e) {
                    Some(sub) => {
                        sub.touch();
                        valid_ids.push(e);
                    }
                    None => {
                        invalid_ids.push(e);
                    }
                }

                (valid_ids, invalid_ids)
            },
        )
    }

    /// clean all topics which didn't heartbeat
    async fn kill_dead_topics(&mut self, heartbeat_error_message: Option<V>) {
        if let Some(ttl) = self.ttl {
            let drained = self.subscriptions.drain();
            let (remaining_subs, closed_subs) = drained.into_iter().fold(
                (HashMap::new(), HashMap::new()),
                |(mut acc, mut acc_error), (topic, sub)| {
                    if (!sub.heartbeat_enabled || sub.updated_at.elapsed() <= ttl)
                        && sub.msg_sender.receiver_count() > 0
                    {
                        acc.insert(topic, sub);
                    } else {
                        acc_error.insert(topic, sub);
                    }

                    (acc, acc_error)
                },
            );
            self.subscriptions = remaining_subs;

            // Send error message to all killed connections
            for (_subscriber_id, subscription) in closed_subs {
                if let Some(heartbeat_error_message) = &heartbeat_error_message {
                    let _ = subscription
                        .msg_sender
                        .send(heartbeat_error_message.clone().into());
                    let _ = subscription.msg_sender.send(None);
                }
            }
        }
    }

    #[cfg(test)]
    fn try_delete(&mut self, topic: K) {
        if let Some(sub) = self.subscriptions.get(&topic) {
            if sub.msg_sender.receiver_count() > 1 {
                return;
            }
        }

        self.force_delete(topic);
    }

    fn force_delete(&mut self, topic: K) {
        tracing::trace!("deleting subscription");
        let sub = self.subscriptions.remove(&topic);
        if let Some(sub) = sub {
            let _ = sub.msg_sender.send(None);
        }
    }

    #[cfg(test)]
    async fn broadcast(&mut self, value: V) -> Option<()>
    where
        V: Clone,
    {
        let mut fut = vec![];
        for (sub_id, sub) in &self.subscriptions {
            let cloned_value = value.clone();
            let sub_id = sub_id.clone();
            fut.push(
                sub.msg_sender
                    .send(cloned_value.into())
                    .is_err()
                    .then_some(sub_id),
            );
        }
        // clean closed sender
        let sub_to_clean: Vec<K> = fut.into_iter().flatten().collect();
        self.subscriptions
            .retain(|k, s| s.msg_sender.receiver_count() > 0 && !sub_to_clean.contains(k));

        Some(())
    }
}

pub(crate) struct RouterBroadcasts {
    configuration: (
        broadcast::Sender<Arc<Configuration>>,
        broadcast::Receiver<Arc<Configuration>>,
    ),
    schema: (
        broadcast::Sender<Arc<Schema>>,
        broadcast::Receiver<Arc<Schema>>,
    ),
}

impl RouterBroadcasts {
    pub(crate) fn new() -> Self {
        Self {
            configuration: broadcast::channel(1),
            schema: broadcast::channel(1),
        }
    }

    pub(crate) fn subscribe_configuration(&self) -> impl Stream<Item = Arc<Configuration>> {
        BroadcastStream::new(self.configuration.0.subscribe())
            .filter_map(|cfg| futures::future::ready(cfg.ok()))
    }

    pub(crate) fn subscribe_schema(&self) -> impl Stream<Item = Arc<Schema>> {
        BroadcastStream::new(self.schema.0.subscribe())
            .filter_map(|schema| futures::future::ready(schema.ok()))
    }
}

#[cfg(test)]
mod tests {

    use uuid::Uuid;

    use super::*;

    #[tokio::test]
    async fn subscribe() {
        let mut notify = Notify::builder().build();
        let topic_1 = Uuid::new_v4();
        let topic_2 = Uuid::new_v4();

        let (handle1, created) = notify.create_or_subscribe(topic_1, false).await.unwrap();
        assert!(created);
        let (_handle2, created) = notify.create_or_subscribe(topic_2, false).await.unwrap();
        assert!(created);

        let handle_1_bis = notify.subscribe(topic_1).await.unwrap();
        let handle_1_other = notify.subscribe(topic_1).await.unwrap();
        let mut cloned_notify = notify.clone();

        let mut handle = cloned_notify.subscribe(topic_1).await.unwrap().into_sink();
        handle
            .send_sync(serde_json_bytes::json!({"test": "ok"}))
            .unwrap();
        drop(handle);
        drop(handle1);
        let mut handle_1_bis = handle_1_bis.into_stream();
        let new_msg = handle_1_bis.next().await.unwrap();
        assert_eq!(new_msg, serde_json_bytes::json!({"test": "ok"}));
        let mut handle_1_other = handle_1_other.into_stream();
        let new_msg = handle_1_other.next().await.unwrap();
        assert_eq!(new_msg, serde_json_bytes::json!({"test": "ok"}));

        assert!(notify.exist(topic_1).await.unwrap());
        assert!(notify.exist(topic_2).await.unwrap());

        drop(_handle2);
        drop(handle_1_bis);
        drop(handle_1_other);

        let subscriptions_nb = notify.debug().await.unwrap();
        assert_eq!(subscriptions_nb, 0);
    }

    #[tokio::test]
    async fn it_subscribe_and_delete() {
        let mut notify = Notify::builder().build();
        let topic_1 = Uuid::new_v4();
        let topic_2 = Uuid::new_v4();

        let (handle1, created) = notify.create_or_subscribe(topic_1, true).await.unwrap();
        assert!(created);
        let (_handle2, created) = notify.create_or_subscribe(topic_2, true).await.unwrap();
        assert!(created);

        let mut _handle_1_bis = notify.subscribe(topic_1).await.unwrap();
        let mut _handle_1_other = notify.subscribe(topic_1).await.unwrap();
        let mut cloned_notify = notify.clone();
        let mut handle = cloned_notify.subscribe(topic_1).await.unwrap().into_sink();
        handle
            .send_sync(serde_json_bytes::json!({"test": "ok"}))
            .unwrap();
        drop(handle);
        assert!(notify.exist(topic_1).await.unwrap());
        drop(_handle_1_bis);
        drop(_handle_1_other);

        notify.try_delete(topic_1).unwrap();

        let subscriptions_nb = notify.debug().await.unwrap();
        assert_eq!(subscriptions_nb, 1);

        assert!(!notify.exist(topic_1).await.unwrap());

        notify.force_delete(topic_1).await.unwrap();

        let mut handle1 = handle1.into_stream();
        let new_msg = handle1.next().await.unwrap();
        assert_eq!(new_msg, serde_json_bytes::json!({"test": "ok"}));
        assert!(handle1.next().await.is_none());
        assert!(notify.exist(topic_2).await.unwrap());
        notify.try_delete(topic_2).unwrap();

        let subscriptions_nb = notify.debug().await.unwrap();
        assert_eq!(subscriptions_nb, 0);
    }

    #[tokio::test]
    async fn it_test_ttl() {
        let mut notify = Notify::builder()
            .ttl(Duration::from_millis(100))
            .heartbeat_error_message(serde_json_bytes::json!({"error": "connection_closed"}))
            .build();
        let topic_1 = Uuid::new_v4();
        let topic_2 = Uuid::new_v4();

        let (handle1, created) = notify.create_or_subscribe(topic_1, true).await.unwrap();
        assert!(created);
        let (_handle2, created) = notify.create_or_subscribe(topic_2, true).await.unwrap();
        assert!(created);

        let handle_1_bis = notify.subscribe(topic_1).await.unwrap();
        let handle_1_other = notify.subscribe(topic_1).await.unwrap();
        let mut cloned_notify = notify.clone();
        tokio::spawn(async move {
            let mut handle = cloned_notify.subscribe(topic_1).await.unwrap().into_sink();
            handle
                .send_sync(serde_json_bytes::json!({"test": "ok"}))
                .unwrap();
        });
        drop(handle1);

        let mut handle_1_bis = handle_1_bis.into_stream();
        let new_msg = handle_1_bis.next().await.unwrap();
        assert_eq!(new_msg, serde_json_bytes::json!({"test": "ok"}));
        let mut handle_1_other = handle_1_other.into_stream();
        let new_msg = handle_1_other.next().await.unwrap();
        assert_eq!(new_msg, serde_json_bytes::json!({"test": "ok"}));

        tokio::time::sleep(Duration::from_millis(200)).await;
        let res = handle_1_bis.next().await.unwrap();
        assert_eq!(res, serde_json_bytes::json!({"error": "connection_closed"}));

        assert!(handle_1_bis.next().await.is_none());

        assert!(!notify.exist(topic_1).await.unwrap());
        assert!(!notify.exist(topic_2).await.unwrap());

        let subscriptions_nb = notify.debug().await.unwrap();
        assert_eq!(subscriptions_nb, 0);
    }
}
