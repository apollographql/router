use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use fred::interfaces::MetricsInterface;
use fred::prelude::Pool as RedisPool;
use opentelemetry::KeyValue;
use opentelemetry::metrics::Gauge;
use opentelemetry::metrics::MeterProvider;
use tokio::task::AbortHandle;

use super::redis::ACTIVE_CLIENT_COUNT;
use crate::metrics::FutureMetricsExt;
use crate::metrics::meter_provider;

/// Weighted sum data for calculating averages
#[derive(Default)]
struct WeightedSum {
    weighted_sum: u64,
    total_samples: u64,
}

impl WeightedSum {
    fn average(&self, unit_conversion: f64) -> f64 {
        if self.total_samples > 0 {
            (self.weighted_sum as f64) / (self.total_samples as f64) / unit_conversion
        } else {
            0.0
        }
    }
}

/// Aggregated metrics collected from Redis clients
#[derive(Default)]
struct ClientMetrics {
    total_redelivery_count: u64,
    total_queue_len: u64,
    total_commands_executed: u64,
    network_latency: WeightedSum,
    latency: WeightedSum,
    request_size: WeightedSum,
    response_size: WeightedSum,
}

/// Sync gauges for Redis metrics.
/// These are created once in the background task and recorded to periodically.
struct RedisGauges {
    queue_length: Gauge<u64>,
    network_latency: Gauge<f64>,
    latency: Gauge<f64>,
    request_size: Gauge<f64>,
    response_size: Gauge<f64>,
    client_count: Gauge<u64>,
}

impl RedisGauges {
    fn new() -> Self {
        let meter = meter_provider().meter("apollo/router");

        Self {
            queue_length: meter
                .u64_gauge("apollo.router.cache.redis.command_queue_length")
                .with_description("Number of Redis commands buffered and not yet sent")
                .with_unit("{command}")
                .build(),
            network_latency: meter
                .f64_gauge("experimental.apollo.router.cache.redis.network_latency_avg")
                .with_description("Average Redis network latency")
                .with_unit("s")
                .build(),
            latency: meter
                .f64_gauge("experimental.apollo.router.cache.redis.latency_avg")
                .with_description("Average Redis command latency")
                .with_unit("s")
                .build(),
            request_size: meter
                .f64_gauge("experimental.apollo.router.cache.redis.request_size_avg")
                .with_description("Average Redis request size")
                .with_unit("bytes")
                .build(),
            response_size: meter
                .f64_gauge("experimental.apollo.router.cache.redis.response_size_avg")
                .with_description("Average Redis response size")
                .with_unit("bytes")
                .build(),
            client_count: meter
                .u64_gauge("apollo.router.cache.redis.clients")
                .with_description("Number of active Redis clients")
                .with_unit("{client}")
                .build(),
        }
    }

    fn record(&self, metrics: &ClientMetrics, caller: &'static str) {
        let attrs = &[KeyValue::new("kind", caller)];

        self.queue_length.record(metrics.total_queue_len, attrs);
        // Fred returns milliseconds, convert to seconds
        self.network_latency
            .record(metrics.network_latency.average(1000.0), attrs);
        self.latency.record(metrics.latency.average(1000.0), attrs);
        // Bytes - no conversion needed
        self.request_size
            .record(metrics.request_size.average(1.0), attrs);
        self.response_size
            .record(metrics.response_size.average(1.0), attrs);
        // Client count has no "kind" attribute
        self.client_count
            .record(ACTIVE_CLIENT_COUNT.load(Ordering::Relaxed), &[]);
    }
}

/// Redis metrics collection functionality.
///
/// The background task that polls Redis client metrics is only spawned when
/// `activate()` is called. This ensures all metric instruments are registered
/// with the correct meter provider (after Telemetry.activate() has run).
pub(crate) struct RedisMetricsCollector {
    /// None until activate() is called
    /// TODO(@goto-bus-stop): actually this should maybe be a Once?
    abort_handle: parking_lot::Mutex<Option<AbortHandle>>,
    pool: Arc<RedisPool>,
    caller: &'static str,
    metrics_interval: Duration,
}

impl Drop for RedisMetricsCollector {
    fn drop(&mut self) {
        if let Some(handle) = self.abort_handle.lock().take() {
            handle.abort();
        }
    }
}

impl RedisMetricsCollector {
    /// Create a new metrics collector.
    ///
    /// The background task is NOT started until `activate()` is called.
    pub(crate) fn new(
        pool: Arc<RedisPool>,
        caller: &'static str,
        metrics_interval: Duration,
    ) -> Self {
        Self {
            abort_handle: parking_lot::Mutex::new(None),
            pool,
            caller,
            metrics_interval,
        }
    }

    /// Start the metrics collection task.
    ///
    /// This MUST be called after `Telemetry.activate()` to ensure all metric
    /// instruments are registered with the correct meter provider.
    pub(crate) fn activate(&self) {
        let pool = self.pool.clone();
        let caller = self.caller;
        let metrics_interval = self.metrics_interval;

        let handle = tokio::spawn(
            async move {
                let mut interval = tokio::time::interval(metrics_interval);
                let gauges = RedisGauges::new();

                loop {
                    interval.tick().await;

                    let metrics = Self::collect_client_metrics(&pool);
                    gauges.record(&metrics, caller);
                    Self::emit_counter_metrics(&metrics, caller);
                }
            }
            .with_current_meter_provider(),
        );

        *self.abort_handle.lock() = Some(handle.abort_handle());
    }

    /// Collect metrics from all Redis clients
    fn collect_client_metrics(pool: &Arc<RedisPool>) -> ClientMetrics {
        let mut metrics = ClientMetrics::default();

        for client in pool.clients() {
            let redelivery_count = client.take_redelivery_count();
            metrics.total_redelivery_count += redelivery_count as u64;

            let queue_len = client.command_queue_len();
            metrics.total_queue_len += queue_len as u64;

            Self::update_weighted_sum(
                client.take_network_latency_metrics(),
                &mut metrics.network_latency,
            );
            Self::update_weighted_sum(client.take_latency_metrics(), &mut metrics.latency);
            Self::update_weighted_sum(client.take_req_size_metrics(), &mut metrics.request_size);
            Self::update_weighted_sum(client.take_res_size_metrics(), &mut metrics.response_size);
        }

        metrics.total_commands_executed = metrics.latency.total_samples;
        metrics
    }

    fn update_weighted_sum(stats: fred::types::Stats, weighted_sum: &mut WeightedSum) {
        if stats.samples > 0 {
            weighted_sum.weighted_sum += (stats.avg as u64) * stats.samples;
            weighted_sum.total_samples += stats.samples;
        }
    }

    fn emit_counter_metrics(metrics: &ClientMetrics, caller: &'static str) {
        u64_counter_with_unit!(
            "apollo.router.cache.redis.redelivery_count",
            "Number of Redis command redeliveries due to connection issues",
            "{redelivery}",
            metrics.total_redelivery_count,
            kind = caller
        );

        u64_counter_with_unit!(
            "apollo.router.cache.redis.commands_executed",
            "Number of Redis commands executed",
            "{command}",
            metrics.total_commands_executed,
            kind = caller
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache::redis::RedisCacheStorage;
    use crate::cache::redis::RedisKey;
    use crate::cache::redis::RedisValue;
    use crate::metrics::test_utils::MetricType;
    use opentelemetry::KeyValue;

    #[test]
    fn test_weighted_sum_average() {
        let mut ws = WeightedSum::default();

        // Empty case
        assert_eq!(ws.average(1.0), 0.0);

        // Add some samples: avg=10ms, 5 samples
        ws.weighted_sum = 50; // 10 * 5
        ws.total_samples = 5;

        // No conversion
        assert_eq!(ws.average(1.0), 10.0);

        // Convert ms to seconds
        assert_eq!(ws.average(1000.0), 0.01);
    }

    #[test]
    fn test_update_weighted_sum() {
        let mut ws = WeightedSum::default();

        // Test with samples
        RedisMetricsCollector::update_weighted_sum(
            fred::types::Stats {
                avg: 10.0,
                samples: 5,
                max: 15,
                min: 5,
                stddev: 2.0,
                sum: 50,
            },
            &mut ws,
        );

        assert_eq!(ws.total_samples, 5);
        assert_eq!(ws.weighted_sum, 50); // 10 * 5

        // Test with zero samples (should not change)
        RedisMetricsCollector::update_weighted_sum(
            fred::types::Stats {
                avg: 100.0,
                samples: 0,
                max: 0,
                min: 0,
                stddev: 0.0,
                sum: 0,
            },
            &mut ws,
        );

        assert_eq!(ws.total_samples, 5); // unchanged
        assert_eq!(ws.weighted_sum, 50); // unchanged
    }

    #[tokio::test]
    async fn test_redis_storage_with_mocks() {
        async {
            let simple_map = Arc::new(fred::mocks::SimpleMap::new());
            let storage = RedisCacheStorage::from_mocks(simple_map.clone())
                .await
                .expect("Failed to create Redis storage with mocks");
            storage.activate();

            #[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
            struct TestValue {
                data: String,
            }

            impl crate::cache::storage::ValueType for TestValue {
                fn estimated_size(&self) -> Option<usize> {
                    Some(self.data.len())
                }
            }

            let test_key = RedisKey("test_key".to_string());
            let test_value = RedisValue(TestValue {
                data: "test_value".to_string(),
            });

            // Perform Redis operations
            storage
                .insert(test_key.clone(), test_value.clone(), None)
                .await;
            let retrieved: Result<RedisValue<TestValue>, _> = storage.get(test_key.clone()).await;

            // Verify the mock actually worked
            assert!(retrieved.is_ok(), "Should have retrieved value from mock");
            assert_eq!(retrieved.unwrap().0.data, "test_value");

            // Poll until command queue length is zero (fixed sleeps are flaky under CI load).
            let queue_attrs = &[KeyValue::new("kind", "test")];
            for _ in 0..50 {
                if crate::metrics::collect_metrics().assert(
                    "apollo.router.cache.redis.command_queue_length",
                    MetricType::Gauge,
                    0.0,
                    false,
                    queue_attrs,
                ) {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(40)).await;
            }

            // Verify Redis connection metrics are emitted.
            // Since this metric is based on a global AtomicU64, it's not unique across tests - so
            // we can only reliably check for metric existence, rather than a specific value.
            assert!(crate::metrics::collect_metrics().metric_exists(
                "apollo.router.cache.redis.clients",
                MetricType::Gauge,
                &[],
            ));

            // Verify Redis gauge metrics are available (observables are created immediately)
            assert_gauge!(
                "apollo.router.cache.redis.command_queue_length",
                0.0,
                kind = "test"
            );

            // Verify Redis average metrics are available (may be 0 initially)
            assert_gauge!(
                "experimental.apollo.router.cache.redis.latency_avg",
                0.0,
                kind = "test"
            );

            assert_gauge!(
                "experimental.apollo.router.cache.redis.network_latency_avg",
                0.0,
                kind = "test"
            );
        }
        .with_metrics()
        .await;
    }
}
