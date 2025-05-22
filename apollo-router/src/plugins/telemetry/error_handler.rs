use std::time::Duration;
use std::time::Instant;

use dashmap::DashMap;
use once_cell::sync::OnceCell;
use opentelemetry::metrics::MetricsError;

#[derive(Eq, PartialEq, Hash)]
enum ErrorType {
    Trace,
    Metric,
    Other,
}
static OTEL_ERROR_LAST_LOGGED: OnceCell<DashMap<ErrorType, Instant>> = OnceCell::new();

pub(crate) fn handle_error<T: Into<opentelemetry::global::Error>>(err: T) {
    // We have to rate limit these errors because when they happen they are very frequent.
    // Use a dashmap to store the message type with the last time it was logged.
    handle_error_with_map(err, OTEL_ERROR_LAST_LOGGED.get_or_init(DashMap::new));
}

// Allow for map injection to avoid using global map in tests
fn handle_error_with_map<T: Into<opentelemetry::global::Error>>(
    err: T,
    last_logged_map: &DashMap<ErrorType, Instant>,
) {
    let err = err.into();

    // We don't want the dashmap to get big, so we key the error messages by type.
    let error_type = match err {
        opentelemetry::global::Error::Trace(_) => ErrorType::Trace,
        opentelemetry::global::Error::Metric(_) => ErrorType::Metric,
        _ => ErrorType::Other,
    };
    #[cfg(not(test))]
    let threshold = Duration::from_secs(10);
    #[cfg(test)]
    let threshold = Duration::from_millis(100);

    if let opentelemetry::global::Error::Metric(err) = &err {
        // For now we have to suppress Metrics error: reader is shut down or not registered
        // https://github.com/open-telemetry/opentelemetry-rust/issues/1244

        if err.to_string() == "Metrics error: reader is shut down or not registered" {
            return;
        }

        // Keep track of the number of cardinality overflow errors otel emits. This can be removed after upgrading to 0.28.0 when the cardinality limit is removed.
        // The version upgrade will also cause this log to be removed from our visibility even if we were set up custom a cardinality limit.
        // https://github.com/open-telemetry/opentelemetry-rust/pull/2528
        if err.to_string()
            == "Metrics error: Warning: Maximum data points for metric stream exceeded. Entry added to overflow. Subsequent overflows to same metric until next collect will not be logged."
        {
            u64_counter!(
                "apollo.router.telemetry.metrics.cardinality_overflow",
                "A count of how often a telemetry metric hit the hard cardinality limit",
                1
            );
        }
    }

    // Copy here so that we don't retain a mutable reference into the dashmap and lock the shard
    let now = Instant::now();
    let last_logged = *last_logged_map
        .entry(error_type)
        .and_modify(|last_logged| {
            if last_logged.elapsed() > threshold {
                *last_logged = now;
            }
        })
        .or_insert_with(|| now);

    if last_logged == now {
        // These events are logged with explicitly no parent. This allows them to be detached from traces.
        match err {
            opentelemetry::global::Error::Trace(err) => {
                ::tracing::error!("OpenTelemetry trace error occurred: {}", err)
            }
            opentelemetry::global::Error::Metric(err) => {
                if let MetricsError::Other(msg) = &err {
                    if msg.contains("Warning") {
                        ::tracing::warn!(parent: None, "OpenTelemetry metric warning occurred: {}", msg);
                        return;
                    }

                    // TODO: We should be able to remove this after upgrading to 0.26.0, which addresses the double-shutdown
                    // called out in https://github.com/open-telemetry/opentelemetry-rust/issues/1661
                    if msg == "metrics provider already shut down" {
                        return;
                    }
                }
                ::tracing::error!(parent: None, "OpenTelemetry metric error occurred: {}", err);
            }
            opentelemetry::global::Error::Other(err) => {
                ::tracing::error!(parent: None, "OpenTelemetry error occurred: {}", err)
            }
            other => {
                ::tracing::error!(parent: None, "OpenTelemetry error occurred: {:?}", other)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fmt::Debug;
    use std::ops::DerefMut;
    use std::sync::Arc;
    use std::time::Duration;

    use dashmap::DashMap;
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
    use crate::plugins::telemetry::error_handler::handle_error_with_map;

    #[tokio::test]
    async fn test_handle_error_throttling() {
        let error_map = DashMap::new();
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
            handle_error_with_map(
                opentelemetry::global::Error::Other("other error".to_string()),
                &error_map,
            );
            handle_error_with_map(
                opentelemetry::global::Error::Other("other error".to_string()),
                &error_map,
            );
            handle_error_with_map(
                opentelemetry::global::Error::Trace("trace error".to_string().into()),
                &error_map,
            );
        }
        .with_subscriber(tracing_subscriber::registry().with(test_layer.clone()))
        .await;

        test_layer.assert_log_entry_count("other error", 1);
        test_layer.assert_log_entry_count("trace error", 1);

        // Sleep a bit and then log again, it should get logged
        tokio::time::sleep(Duration::from_millis(200)).await;
        async {
            handle_error_with_map(
                opentelemetry::global::Error::Other("other error".to_string()),
                &error_map,
            );
        }
        .with_subscriber(tracing_subscriber::registry().with(test_layer.clone()))
        .await;
        test_layer.assert_log_entry_count("other error", 2);
    }

    #[tokio::test]
    async fn test_cardinality_overflow() {
        async {
            let error_map = DashMap::new();
            let msg = "Warning: Maximum data points for metric stream exceeded. Entry added to overflow. Subsequent overflows to same metric until next collect will not be logged.";
            handle_error_with_map(
                opentelemetry::global::Error::Metric(opentelemetry::metrics::MetricsError::Other(msg.to_string())),
                &error_map,
            );

            assert_counter!(
                "apollo.router.telemetry.metrics.cardinality_overflow",
                1
            );
        }
        .with_metrics()
        .await;
    }
}
