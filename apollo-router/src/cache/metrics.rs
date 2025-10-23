use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;
use std::time::Duration;

use fred::interfaces::MetricsInterface;
use fred::prelude::Pool as RedisPool;
use opentelemetry::KeyValue;
use opentelemetry::metrics::MeterProvider;
use opentelemetry::metrics::ObservableGauge;
use tokio::task::AbortHandle;

use super::redis::ACTIVE_CLIENT_COUNT;
use crate::metrics::meter_provider;

/// Collection of Redis metrics gauges
pub(crate) struct RedisMetricsGauges {
    pub(crate) _queue_length: ObservableGauge<u64>,
    pub(crate) _network_latency: ObservableGauge<f64>,
    pub(crate) _latency: ObservableGauge<f64>,
    pub(crate) _request_size: ObservableGauge<f64>,
    pub(crate) _response_size: ObservableGauge<f64>,
    _active_client_count: ObservableGauge<u64>,
}

/// Weighted sum data for calculating averages
#[derive(Default, Clone)]
struct WeightedSum {
    weighted_sum: u64,
    total_samples: u64,
}

/// Configuration for metrics collection
struct MetricsConfig {
    pool: Arc<RedisPool>,
    caller: &'static str,
    metrics_interval: Duration,
    queue_length: Arc<AtomicU64>,
    network_latency_metric: WeightedAverageMetric,
    latency_metric: WeightedAverageMetric,
    request_size_metric: WeightedAverageMetric,
    response_size_metric: WeightedAverageMetric,
}

/// Configuration for a weighted average metric
#[derive(Clone)]
struct WeightedAverageMetric {
    weighted_sum: Arc<AtomicU64>,
    sample_count: Arc<AtomicU64>,
    name: &'static str,
    description: &'static str,
    unit: &'static str,
    unit_conversion: f64, // e.g., 1000.0 for ms->μs conversion
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

/// Redis metrics collection functionality
pub(crate) struct RedisMetricsCollector {
    // Task handle and gauges
    abort_handle: AbortHandle,
    _gauges: RedisMetricsGauges,
}

impl WeightedAverageMetric {
    /// Create a new weighted average metric
    fn new(
        name: &'static str,
        description: &'static str,
        unit: &'static str,
        unit_conversion: f64,
    ) -> Self {
        Self {
            weighted_sum: Arc::new(AtomicU64::new(0)),
            sample_count: Arc::new(AtomicU64::new(0)),
            name,
            description,
            unit,
            unit_conversion,
        }
    }

    /// Update the atomic counters with new weighted sum data
    fn update(&self, weighted_sum: &WeightedSum) {
        self.weighted_sum
            .store(weighted_sum.weighted_sum, Ordering::Relaxed);
        self.sample_count
            .store(weighted_sum.total_samples, Ordering::Relaxed);
    }
}

impl Drop for RedisMetricsCollector {
    fn drop(&mut self) {
        self.abort_handle.abort();
    }
}

impl RedisMetricsCollector {
    /// Create a new metrics collector and start the collection task
    pub(crate) fn new(
        pool: Arc<RedisPool>,
        caller: &'static str,
        metrics_interval: Duration,
    ) -> Self {
        // Create atomic counters for metrics
        let queue_length = Arc::new(AtomicU64::new(0));

        let network_latency_metric = WeightedAverageMetric::new(
            "experimental.apollo.router.cache.redis.network_latency_avg",
            "Average Redis network latency",
            "s",
            1000.0, // Fred returns milliseconds, convert to seconds for display
        );
        let latency_metric = WeightedAverageMetric::new(
            "experimental.apollo.router.cache.redis.latency_avg",
            "Average Redis command latency",
            "s",
            1000.0, // Fred returns milliseconds, convert to seconds for display
        );
        let request_size_metric = WeightedAverageMetric::new(
            "experimental.apollo.router.cache.redis.request_size_avg",
            "Average Redis request size",
            "bytes",
            1.0,
        );
        let response_size_metric = WeightedAverageMetric::new(
            "experimental.apollo.router.cache.redis.response_size_avg",
            "Average Redis response size",
            "bytes",
            1.0,
        );

        let config = MetricsConfig {
            pool: pool.clone(),
            caller,
            metrics_interval,
            queue_length: queue_length.clone(),
            network_latency_metric,
            latency_metric,
            request_size_metric,
            response_size_metric,
        };

        let (abort_handle, gauges) = Self::start_collection_task_for_metrics(config);

        Self {
            abort_handle,
            _gauges: gauges,
        }
    }

    /// Start the metrics collection task and create gauges
    fn start_collection_task_for_metrics(
        config: MetricsConfig,
    ) -> (AbortHandle, RedisMetricsGauges) {
        let queue_length_gauge =
            Self::create_queue_length_gauge(config.queue_length.clone(), config.caller);
        let network_latency_gauge =
            Self::create_weighted_average_gauge(&config.network_latency_metric, config.caller);
        let latency_gauge =
            Self::create_weighted_average_gauge(&config.latency_metric, config.caller);
        let request_size_gauge =
            Self::create_weighted_average_gauge(&config.request_size_metric, config.caller);
        let response_size_gauge =
            Self::create_weighted_average_gauge(&config.response_size_metric, config.caller);
        let client_count_gauge = Self::create_client_count_gauge();
        let metrics_handle = Self::spawn_metrics_collection_task(config);

        let gauges = RedisMetricsGauges {
            _queue_length: queue_length_gauge,
            _network_latency: network_latency_gauge,
            _latency: latency_gauge,
            _request_size: request_size_gauge,
            _response_size: response_size_gauge,
            _active_client_count: client_count_gauge,
        };

        (metrics_handle.abort_handle(), gauges)
    }

    /// Create the queue length observable gauge
    fn create_queue_length_gauge(
        queue_length: Arc<AtomicU64>,
        caller: &'static str,
    ) -> ObservableGauge<u64> {
        let meter = meter_provider().meter("apollo/router");
        let queue_length_for_gauge = queue_length;

        meter
            .u64_observable_gauge("apollo.router.cache.redis.command_queue_length")
            .with_description("Number of Redis commands buffered and not yet sent")
            .with_unit("{command}")
            .with_callback(move |gauge| {
                gauge.observe(
                    queue_length_for_gauge.load(Ordering::Relaxed),
                    &[KeyValue::new("kind", caller)],
                );
            })
            .build()
    }

    /// Generic method to create a weighted average gauge
    fn create_weighted_average_gauge(
        metric: &WeightedAverageMetric,
        caller: &'static str,
    ) -> ObservableGauge<f64> {
        let meter = meter_provider().meter("apollo/router");
        let weighted_sum_for_gauge = metric.weighted_sum.clone();
        let sample_count_for_gauge = metric.sample_count.clone();
        let unit_conversion = metric.unit_conversion;

        meter
            .f64_observable_gauge(metric.name)
            .with_description(metric.description)
            .with_unit(metric.unit)
            .with_callback(move |gauge| {
                let total_samples = sample_count_for_gauge.load(Ordering::Relaxed);
                let weighted_sum = weighted_sum_for_gauge.load(Ordering::Relaxed);

                let average = if total_samples > 0 {
                    // Convert from milliseconds to seconds for display
                    (weighted_sum as f64) / (total_samples as f64) / unit_conversion
                } else {
                    // Emit 0 to show the gauge exists even when no samples are available at scrape time
                    0.0
                };

                gauge.observe(average, &[KeyValue::new("kind", caller)]);
            })
            .build()
    }

    fn create_client_count_gauge() -> ObservableGauge<u64> {
        let meter = meter_provider().meter("apollo/router");
        meter
            .u64_observable_gauge("apollo.router.cache.redis.clients")
            .with_description("Number of active Redis clients")
            .with_unit("{client}")
            .with_callback(move |gauge| {
                gauge.observe(ACTIVE_CLIENT_COUNT.load(Ordering::Relaxed), &[]);
            })
            .init()
    }

    /// Spawn the metrics collection task
    fn spawn_metrics_collection_task(config: MetricsConfig) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(config.metrics_interval);
            loop {
                interval.tick().await;

                let metrics = Self::collect_client_metrics(&config.pool);

                // Update atomic counters for gauges
                config
                    .queue_length
                    .store(metrics.total_queue_len, Ordering::Relaxed);
                config
                    .network_latency_metric
                    .update(&metrics.network_latency);
                config.latency_metric.update(&metrics.latency);
                config.request_size_metric.update(&metrics.request_size);
                config.response_size_metric.update(&metrics.response_size);

                // Emit counters
                Self::emit_counter_metrics(&metrics, config.caller);
            }
        })
    }

    /// Collect metrics from all Redis clients
    fn collect_client_metrics(pool: &Arc<RedisPool>) -> ClientMetrics {
        let mut metrics = ClientMetrics::default();

        for client in pool.clients() {
            // Basic metrics always available
            let redelivery_count = client.take_redelivery_count();
            metrics.total_redelivery_count += redelivery_count as u64;

            let queue_len = client.command_queue_len();
            metrics.total_queue_len += queue_len as u64;

            // Collect weighted average metrics directly
            Self::update_average_weighted_metric(
                client.take_network_latency_metrics(),
                &mut metrics.network_latency,
                1.0, // Fred returns milliseconds, store as-is for precision
            );

            Self::update_average_weighted_metric(
                client.take_latency_metrics(),
                &mut metrics.latency,
                1.0, // Fred returns milliseconds, store as-is for precision
            );

            Self::update_average_weighted_metric(
                client.take_req_size_metrics(),
                &mut metrics.request_size,
                1.0,
            );

            Self::update_average_weighted_metric(
                client.take_res_size_metrics(),
                &mut metrics.response_size,
                1.0,
            );

            // Get commands executed from latency stats (already collected above)
            // Note: We use latency samples as a proxy for total commands executed
            // since latency stats track all commands that were executed
        }

        // Set total commands executed based on latency samples
        metrics.total_commands_executed = metrics.latency.total_samples;

        metrics
    }

    /// Generic method to collect weighted metrics
    fn update_average_weighted_metric(
        stats: fred::types::Stats,
        weighted_sum: &mut WeightedSum,
        unit_conversion: f64,
    ) {
        if stats.samples > 0 {
            // Apply unit conversion (Fred returns milliseconds)
            let converted_avg = (stats.avg * unit_conversion) as u64;
            weighted_sum.weighted_sum += converted_avg * stats.samples;
            weighted_sum.total_samples += stats.samples;
        }
    }

    /// Emit counter metrics
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
    use std::sync::Arc;

    use fred::mocks::SimpleMap;

    use crate::cache::redis::RedisCacheStorage;
    use crate::cache::redis::RedisKey;
    use crate::cache::redis::RedisValue;
    use crate::cache::storage::ValueType;
    use crate::metrics::FutureMetricsExt;
    use crate::metrics::test_utils::MetricType;

    #[test]
    fn test_weighted_sum_calculation() {
        let mut weighted_sum = super::WeightedSum::default();

        // Test adding first stats
        super::RedisMetricsCollector::update_average_weighted_metric(
            fred::types::Stats {
                avg: 10.0, // 10ms (Fred returns milliseconds)
                samples: 5,
                max: 15,
                min: 5,
                stddev: 2.0,
                sum: 50,
            },
            &mut weighted_sum,
            1.0, // Store milliseconds as-is
        );

        assert_eq!(weighted_sum.total_samples, 5);
        assert_eq!(weighted_sum.weighted_sum, 50); // 10.0 * 1.0 * 5 = 50 milliseconds

        // Test adding more stats
        super::RedisMetricsCollector::update_average_weighted_metric(
            fred::types::Stats {
                avg: 20.0, // 20ms (Fred returns milliseconds)
                samples: 3,
                max: 25,
                min: 15,
                stddev: 3.0,
                sum: 60,
            },
            &mut weighted_sum,
            1.0,
        );

        assert_eq!(weighted_sum.total_samples, 8); // 5 + 3
        assert_eq!(weighted_sum.weighted_sum, 110); // 50 + 60 milliseconds
    }

    #[test]
    fn test_weighted_sum_with_zero_samples() {
        let mut weighted_sum = super::WeightedSum::default();

        // Test that zero samples don't affect the weighted sum
        super::RedisMetricsCollector::update_average_weighted_metric(
            fred::types::Stats {
                avg: 0.010, // 10ms in seconds
                samples: 0,
                max: 0,
                min: 0,
                stddev: 0.0,
                sum: 0,
            },
            &mut weighted_sum,
            1000000.0,
        );

        assert_eq!(weighted_sum.total_samples, 0);
        assert_eq!(weighted_sum.weighted_sum, 0);
    }

    #[test]
    fn test_weighted_sum_with_unit_conversion() {
        let mut weighted_sum = super::WeightedSum::default();

        // Test with different unit conversions (bytes - no conversion)
        super::RedisMetricsCollector::update_average_weighted_metric(
            fred::types::Stats {
                avg: 100.0,
                samples: 2,
                max: 120,
                min: 80,
                stddev: 20.0,
                sum: 200,
            },
            &mut weighted_sum,
            1.0, // No conversion (bytes)
        );

        assert_eq!(weighted_sum.total_samples, 2);
        assert_eq!(weighted_sum.weighted_sum, 200); // 100.0 * 1.0 * 2
    }

    #[test]
    fn test_latency_metric_conversion_to_seconds() {
        // This test demonstrates that latency metrics are correctly converted to seconds
        let mut weighted_sum = super::WeightedSum::default();

        // Simulate Redis latency stats (Fred returns milliseconds)
        super::RedisMetricsCollector::update_average_weighted_metric(
            fred::types::Stats {
                avg: 5.0, // 5ms (Fred returns milliseconds)
                samples: 10,
                max: 8,
                min: 2,
                stddev: 1.5,
                sum: 50,
            },
            &mut weighted_sum,
            1.0, // Store milliseconds as-is
        );

        assert_eq!(weighted_sum.total_samples, 10);
        assert_eq!(weighted_sum.weighted_sum, 50); // 5.0 * 1.0 * 10 = 50 milliseconds

        // Verify conversion to seconds for gauge emission
        let final_average_seconds =
            (weighted_sum.weighted_sum as f64) / (weighted_sum.total_samples as f64) / 1000.0;
        assert_eq!(final_average_seconds, 0.005); // Should be 0.005 seconds (5ms converted from ms)
    }

    #[tokio::test]
    async fn test_redis_storage_with_mocks() {
        async {
            let simple_map = Arc::new(SimpleMap::new());
            let storage = RedisCacheStorage::from_mocks(simple_map.clone())
                .await
                .expect("Failed to create Redis storage with mocks");

            #[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
            struct TestValue {
                data: String,
            }

            impl ValueType for TestValue {
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

            // Verify Redis connection metrics are emitted.
            // Since this metric is based on a global AtomicU64, it's not unique across tests - so
            // we can only reliably check for metric existence, rather than a specific value.
            crate::metrics::collect_metrics().metric_exists::<u64>(
                "apollo.router.cache.redis.clients",
                MetricType::Gauge,
                &[],
            );

            // Pause to ensure that queue length is zero
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;

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
