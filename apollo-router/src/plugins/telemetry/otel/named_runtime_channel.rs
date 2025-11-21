use std::fmt::Debug;
use std::time::Duration;

use futures::future::BoxFuture;
use opentelemetry_sdk::runtime::Runtime;
use opentelemetry_sdk::runtime::RuntimeChannel;
use opentelemetry_sdk::runtime::Tokio;
use opentelemetry_sdk::runtime::TrySend;
use opentelemetry_sdk::runtime::TrySendError;

/// Wraps an otel tokio runtime to provide a name in the error messages and metrics
#[derive(Debug, Clone)]
pub(crate) struct NamedTokioRuntime {
    name: &'static str,
    parent: Tokio,
}

impl NamedTokioRuntime {
    pub(crate) fn new(name: &'static str) -> Self {
        Self {
            name,
            parent: Tokio,
        }
    }
}

impl Runtime for NamedTokioRuntime {
    type Interval = <Tokio as Runtime>::Interval;
    type Delay = <Tokio as Runtime>::Delay;

    fn interval(&self, duration: Duration) -> Self::Interval {
        self.parent.interval(duration)
    }

    fn spawn(&self, future: BoxFuture<'static, ()>) {
        self.parent.spawn(future)
    }

    fn delay(&self, duration: Duration) -> Self::Delay {
        self.parent.delay(duration)
    }
}

impl<T: Debug + Send> RuntimeChannel<T> for NamedTokioRuntime {
    type Receiver = <Tokio as RuntimeChannel<T>>::Receiver;
    type Sender = NamedSender<T>;

    fn batch_message_channel(&self, capacity: usize) -> (Self::Sender, Self::Receiver) {
        let (sender, receiver) = tokio::sync::mpsc::channel(capacity);
        (
            NamedSender::new(self.name, sender),
            tokio_stream::wrappers::ReceiverStream::new(receiver),
        )
    }
}

#[derive(Debug)]
pub(crate) struct NamedSender<T> {
    name: &'static str,
    channel_full_message: String,
    channel_closed_message: String,
    sender: tokio::sync::mpsc::Sender<T>,
}

impl<T: Send> NamedSender<T> {
    fn new(name: &'static str, sender: tokio::sync::mpsc::Sender<T>) -> NamedSender<T> {
        NamedSender {
            name,
            channel_full_message: format!(
                "cannot send message to batch processor '{name}' as the channel is full"
            ),
            channel_closed_message: format!(
                "cannot send message to batch processor '{name}' as the channel is closed"
            ),
            sender,
        }
    }
}

impl<T: Send> TrySend for NamedSender<T> {
    type Message = T;

    fn try_send(&self, item: Self::Message) -> Result<(), TrySendError> {
        // Convert the error into something that has a name
        self.sender.try_send(item).map_err(|err| {
            let error = match &err {
                tokio::sync::mpsc::error::TrySendError::Full(_) => "channel full",
                tokio::sync::mpsc::error::TrySendError::Closed(_) => "channel closed",
            };
            u64_counter!(
                "apollo.router.telemetry.batch_processor.errors",
                "Errors when sending to a batch processor",
                1,
                "name" = self.name,
                "error" = error
            );

            match err {
                tokio::sync::mpsc::error::TrySendError::Full(_) => {
                    TrySendError::Other(self.channel_full_message.as_str().into())
                }
                tokio::sync::mpsc::error::TrySendError::Closed(_) => {
                    TrySendError::Other(self.channel_closed_message.as_str().into())
                }
            }
        })
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::metrics::FutureMetricsExt;

    #[tokio::test]
    async fn test_channel_full_error_metrics() {
        async {
            let runtime = NamedTokioRuntime::new("test_processor");
            let (sender, mut _receiver) = runtime.batch_message_channel(1);

            // Fill the channel
            sender.try_send("first").expect("should send first message");

            // This should fail and emit metrics
            let result = sender.try_send("second");
            assert!(result.is_err());

            assert_counter!(
                "apollo.router.telemetry.batch_processor.errors",
                1,
                "name" = "test_processor",
                "error" = "channel full"
            );
        }
        .with_metrics()
        .await;
    }

    #[tokio::test]
    async fn test_channel_closed_error_metrics() {
        async {
            let runtime = NamedTokioRuntime::new("test_processor");
            let (sender, receiver) = runtime.batch_message_channel(1);

            // Drop receiver to close channel
            drop(receiver);

            let result = sender.try_send("message");
            assert!(result.is_err());

            assert_counter!(
                "apollo.router.telemetry.batch_processor.errors",
                1,
                "name" = "test_processor",
                "error" = "channel closed"
            );
        }
        .with_metrics()
        .await;
    }

    #[tokio::test]
    async fn test_successful_message_send() {
        async {
            let runtime = NamedTokioRuntime::new("test_processor");
            let (sender, _receiver) = runtime.batch_message_channel(1);

            let result = sender.try_send("message");
            assert!(result.is_ok());

            // No metrics should be emitted for success case
            let metrics = crate::metrics::collect_metrics();
            assert!(
                metrics
                    .find("apollo.router.telemetry.batch_processor.errors")
                    .is_none()
            );
        }
        .with_metrics()
        .await;
    }
}
