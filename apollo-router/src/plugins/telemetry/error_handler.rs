


#[cfg(test)]
mod tests {
    use std::fmt::Debug;
    use std::ops::DerefMut;
    use std::sync::Arc;
    use std::time::Duration;

    use parking_lot::Mutex;
    use tracing_core::Event;
    use tracing_core::Field;
    use tracing_core::Subscriber;
    use tracing_core::field::Visit;
    use tracing_futures::WithSubscriber;
    use tracing_subscriber::Layer;
    use tracing_subscriber::layer::Context;
    use tracing_subscriber::layer::SubscriberExt;

    use crate::metrics::FutureMetricsExt;

    #[tokio::test]
    async fn test_handle_error_throttling() {
        // Set up a fake subscriber so we can check log events. If this is useful then maybe it can be factored out into something reusable
        #[derive(Default)]
        struct TestVisitor {
            log_entries: Vec<String>,
        }

        #[derive(Default, Clone)]
        struct TestLayer {
            visitor: Arc<Mutex<TestVisitor>>,
        }
        impl TestLayer {
            fn assert_log_entry_count(&self, message: &str, expected: usize) {
                let log_entries = self.visitor.lock().log_entries.clone();
                let actual = log_entries.iter().filter(|e| e.contains(message)).count();
                assert_eq!(actual, expected);
            }
        }
        impl Visit for TestVisitor {
            fn record_debug(&mut self, field: &Field, value: &dyn Debug) {
                self.log_entries
                    .push(format!("{}={:?}", field.name(), value));
            }
        }

        impl<S> Layer<S> for TestLayer
        where
            S: Subscriber,
            Self: 'static,
        {
            fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
                event.record(self.visitor.lock().deref_mut())
            }
        }

        let test_layer = TestLayer::default();

        async {
            // Log twice rapidly, they should get deduped
            ::tracing::error!("other error");
        }
        .with_subscriber(tracing_subscriber::registry().with(test_layer.clone()))
        .await;

        test_layer.assert_log_entry_count("other error", 1);
        test_layer.assert_log_entry_count("trace error", 1);

        // Sleep a bit and then log again, it should get logged
        tokio::time::sleep(Duration::from_millis(200)).await;
        async {
            ::tracing::error!("other error");
        }
        .with_subscriber(tracing_subscriber::registry().with(test_layer.clone()))
        .await;
        test_layer.assert_log_entry_count("other error", 2);
    }

    #[tokio::test]
    async fn test_cardinality_overflow() {
        async {
            let msg = "Warning: Maximum data points for metric stream exceeded. Entry added to overflow. Subsequent overflows to same metric until next collect will not be logged.";
            ::tracing::warn!("{}", msg);

            assert_counter!(
                "apollo.router.telemetry.metrics.cardinality_overflow",
                1
            );
        }
        .with_metrics()
        .await;
    }
}
