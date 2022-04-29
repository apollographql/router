use crate::plugins::telemetry::apollo::Config;
use crate::plugins::telemetry::config::MetricsCommon;
use crate::plugins::telemetry::metrics::{MetricsBuilder, MetricsConfigurator};
use crate::stream::StreamExt;
use apollo_spaceport::{
    ContextualizedStats, ReferencedFieldsForType, Reporter, ReporterError, ReporterGraph,
    StatsContext,
};
use async_trait::async_trait;
use deadpool::managed;
use futures::channel::mpsc;
use futures_batch::ChunksTimeoutStreamExt;
use std::collections::HashMap;
use std::time::Duration;
use tower::BoxError;
use url::Url;

const DEFAULT_BATCH_SIZE: usize = 65_536;
const DEFAULT_QUEUE_SIZE: usize = 65_536;

#[derive(Clone)]
pub(crate) enum Sender {
    Noop,
    Spaceport(mpsc::Sender<Metrics>),
}

impl Sender {
    pub(crate) fn send(&self, metrics: Metrics) {
        match &self {
            Sender::Noop => {}
            Sender::Spaceport(channel) => {
                if let Err(err) = channel.to_owned().try_send(metrics) {
                    tracing::warn!(
                        "could not send metrics to spaceport, metric will be dropped: {}",
                        err
                    );
                }
            }
        }
    }
}

impl Default for Sender {
    fn default() -> Self {
        Sender::Noop
    }
}

#[derive(Default)]
pub(crate) struct Metrics {
    client_name: String,
    client_version: String,
    stats_report_key: String,
    query_latency_stats: QueryLatencyStats,
    per_type_stat: HashMap<String, TypeStat>,
    referenced_fields_by_type: HashMap<String, ReferencedFieldsForType>,
}

#[derive(Default)]
pub(crate) struct QueryLatencyStats {
    latency_count: Duration,
    request_count: u64,
    requests_without_field_instrumentation: u64,
    cache_hits: u64,
    persisted_query_hits: u64,
    persisted_query_misses: u64,
    cache_latency_count: Duration,
    root_error_stats: PathErrorStats,
    requests_with_errors_count: u64,
    public_cache_ttl_count: Duration,
    private_cache_ttl_count: Duration,
    registered_operation_count: u64,
    forbidden_operation_count: u64,
}

#[derive(Default)]
pub(crate) struct PathErrorStats {
    children: HashMap<String, PathErrorStats>,
    errors_count: u64,
    requests_with_errors_count: u64,
}

impl AggregatedPathErrorStats {
    pub(crate) fn add(&mut self, stats: PathErrorStats) {
        for (k, v) in stats.children.into_iter() {
            self.children.entry(k).or_default().add(v)
        }
        self.errors_count += stats.errors_count;
        self.requests_with_errors_count += stats.requests_with_errors_count;
    }
}

#[derive(Default)]
pub(crate) struct TypeStat {
    per_field_stat: HashMap<String, FieldStat>,
}

#[derive(Default)]
pub(crate) struct FieldStat {
    errors_count: u64,
    observed_execution_count: u64,
    estimated_execution_count: f64,
    requests_with_errors_count: u64,
    latency_count: Duration,
}

#[derive(Default)]
struct AggregatedMetrics {
    query_latency_stats: AggregatedQueryLatencyStats,
    per_type_stat: HashMap<String, AggregatedTypeStat>,
    referenced_fields_by_type: HashMap<String, ReferencedFieldsForType>,
}

impl AggregatedMetrics {
    pub(crate) fn add(&mut self, metrics: Metrics) {
        self.query_latency_stats.add(metrics.query_latency_stats);
        for (k, v) in metrics.per_type_stat {
            self.per_type_stat.entry(k).or_default().add(v);
        }
        for (k, v) in metrics.referenced_fields_by_type {
            // Merging is not required because metrics are always groupd by schema and query.
            // The tuple (client_name, client_version, stats_report_key, referenced_fields_by_type) is always unique.
            self.referenced_fields_by_type.entry(k).or_insert(v);
        }
    }
}

#[derive(Default)]
struct AggregatedQueryLatencyStats {
    latency_count: DurationHistogram,
    request_count: u64,
    requests_without_field_instrumentation: u64,
    cache_hits: u64,
    persisted_query_hits: u64,
    persisted_query_misses: u64,
    cache_latency_count: DurationHistogram,
    root_error_stats: AggregatedPathErrorStats,
    requests_with_errors_count: u64,
    public_cache_ttl_count: DurationHistogram,
    private_cache_ttl_count: DurationHistogram,
    registered_operation_count: u64,
    forbidden_operation_count: u64,
}

impl AggregatedQueryLatencyStats {
    pub(crate) fn add(&mut self, stats: QueryLatencyStats) {
        self.latency_count
            .increment_duration(stats.latency_count, 1);
        self.request_count += stats.request_count;
        self.requests_without_field_instrumentation += stats.requests_without_field_instrumentation;
        self.cache_hits += stats.cache_hits;
        self.persisted_query_hits += stats.persisted_query_hits;
        self.persisted_query_misses += stats.persisted_query_misses;
        self.cache_latency_count
            .increment_duration(stats.cache_latency_count, 1);
        self.root_error_stats.add(stats.root_error_stats);
        self.requests_with_errors_count += stats.requests_with_errors_count;
        self.public_cache_ttl_count
            .increment_duration(stats.public_cache_ttl_count, 1);
        self.private_cache_ttl_count
            .increment_duration(stats.private_cache_ttl_count, 1);
        self.registered_operation_count += stats.registered_operation_count;
        self.forbidden_operation_count += stats.forbidden_operation_count;
    }
}

#[derive(Default)]
struct AggregatedPathErrorStats {
    children: HashMap<String, AggregatedPathErrorStats>,
    errors_count: u64,
    requests_with_errors_count: u64,
}

#[derive(Default)]
struct AggregatedTypeStat {
    per_field_stat: HashMap<String, AggregatedFieldStat>,
}

impl AggregatedTypeStat {
    pub(crate) fn add(&mut self, stat: TypeStat) {
        for (k, v) in stat.per_field_stat.into_iter() {
            self.per_field_stat.entry(k).or_default().add(v);
        }
    }
}

#[derive(Default)]
struct AggregatedFieldStat {
    errors_count: u64,
    observed_execution_count: u64,
    estimated_execution_count: f64,
    requests_with_errors_count: u64,
    latency_count: DurationHistogram,
}

impl AggregatedFieldStat {
    pub(crate) fn add(&mut self, stat: FieldStat) {
        self.errors_count += stat.errors_count;
        self.observed_execution_count += stat.observed_execution_count;
        self.estimated_execution_count += stat.estimated_execution_count;
        self.requests_with_errors_count += stat.requests_with_errors_count;
        //TODO weight calculation
        self.latency_count.increment_duration(stat.latency_count, 1);
    }
}

impl MetricsConfigurator for Config {
    fn apply(
        &self,
        builder: MetricsBuilder,
        _metrics_config: &MetricsCommon,
    ) -> Result<MetricsBuilder, BoxError> {
        tracing::debug!("configuring Apollo metrics");

        Ok(match self {
            Config {
                endpoint: Some(endpoint),
                apollo_key: Some(key),
                apollo_graph_ref: Some(reference),
                ..
            } => {
                let exporter = ApolloMetricsExporter::new(endpoint, key, reference)?;

                builder
                    .with_apollo_metrics_collector(exporter.provider())
                    .with_exporter(exporter)
            }
            _ => builder,
        })
    }
}

struct ApolloMetricsExporter {
    tx: mpsc::Sender<Metrics>,
}

impl ApolloMetricsExporter {
    fn new(
        endpoint: &Url,
        key: &String,
        reference: &String,
    ) -> Result<ApolloMetricsExporter, BoxError> {
        // Desired behavior:
        // * Metrics are batched with a timeout.
        // * If we cannot connect to spaceport metrics are discarded and a warning raised.
        // * When the stream of metrics finishes we terminate the thread.
        // * If the exporter is dropped the remaining records are flushed.

        let (tx, rx) = mpsc::channel::<Metrics>(DEFAULT_QUEUE_SIZE);

        let reporter_graph = ReporterGraph {
            key: key.to_owned(),
            reference: reference.to_owned(),
        };

        // Deadpool gives us connection pooling to spaceport
        // It also significantly simplifies initialisation of the connection and gives us options in the future for configuring timeouts.
        let pool = deadpool::managed::Pool::<ReporterManager>::builder(ReporterManager {
            endpoint: endpoint.clone(),
        })
        .create_timeout(Some(Duration::from_secs(5)))
        .wait_timeout(Some(Duration::from_secs(5)))
        .build()
        .unwrap();

        // This is the thread that actually sends metrics
        tokio::spawn(async move {
            // We want to collect stats into batches, but also send periodically if a batch is not filled.
            // This implementation is not ideal as we do have to store all the data when really it could be folded as it is generated.
            // But in the interested of getting something over the line quickly let's go with this as it is simple to understand.
            rx.chunks_timeout(DEFAULT_BATCH_SIZE, Duration::from_secs(10))
                .for_each(|stats| async {
                    let stats = consolidate(stats);

                    match pool.get().await {
                        Ok(mut reporter) => {
                            for (key, contextualized_stats, field_usage) in stats.into_iter() {
                                match reporter
                                    .submit_stats(
                                        reporter_graph.clone(),
                                        key,
                                        contextualized_stats,
                                        field_usage,
                                    )
                                    .await
                                {
                                    Ok(_) => {}
                                    Err(e) => {
                                        tracing::warn!(
                                            "failed to submit stats to spaceport: {}",
                                            e
                                        );
                                    }
                                };
                            }
                        }
                        Err(err) => {
                            tracing::warn!(
                                "stats discarded as unable to get connection to spaceport: {}",
                                err
                            );
                        }
                    };
                })
                .await;
        });
        Ok(ApolloMetricsExporter { tx })
    }

    pub(crate) fn provider(&self) -> Sender {
        Sender::Spaceport(self.tx.clone())
    }
}

fn consolidate(
    metrics: Vec<Metrics>,
) -> Vec<(
    String,
    ContextualizedStats,
    HashMap<String, ReferencedFieldsForType>,
)> {
    let mut aggregated_metrics = HashMap::<_, AggregatedMetrics>::new();

    for metric in metrics {
        aggregated_metrics
            .entry(
                (
                    metric.client_name.clone(),
                    metric.client_version.clone(),
                    metric.stats_report_key.clone(),
                )
                    .clone(),
            )
            .or_default()
            .add(metric);
    }

    aggregated_metrics
        .into_iter()
        .map(|((client_name, client_version, key), aggregated_metric)| {
            let mut contextualized_stats = ContextualizedStats {
                context: Some(StatsContext {
                    client_name,
                    client_version,
                }),
                ..Default::default()
            };
            contextualized_stats.query_latency_stats =
                Some(aggregated_metric.query_latency_stats.into());

            (
                key,
                contextualized_stats,
                aggregated_metric.referenced_fields_by_type,
            )
        })
        .collect()
}

impl From<AggregatedQueryLatencyStats> for apollo_spaceport::QueryLatencyStats {
    fn from(stats: AggregatedQueryLatencyStats) -> Self {
        Self {
            latency_count: stats.latency_count.buckets,
            request_count: stats.request_count,
            cache_hits: stats.cache_hits,
            persisted_query_hits: stats.persisted_query_hits,
            persisted_query_misses: stats.persisted_query_misses,
            cache_latency_count: stats.cache_latency_count.buckets,
            root_error_stats: Some(stats.root_error_stats.into()),
            requests_with_errors_count: stats.requests_with_errors_count,
            public_cache_ttl_count: stats.public_cache_ttl_count.buckets,
            private_cache_ttl_count: stats.private_cache_ttl_count.buckets,
            registered_operation_count: stats.registered_operation_count,
            forbidden_operation_count: stats.forbidden_operation_count,
            requests_without_field_instrumentation: stats.requests_without_field_instrumentation,
        }
    }
}

impl From<AggregatedPathErrorStats> for apollo_spaceport::PathErrorStats {
    fn from(stats: AggregatedPathErrorStats) -> Self {
        Self {
            children: stats
                .children
                .into_iter()
                .map(|(k, v)| (k, v.into()))
                .collect(),
            errors_count: stats.errors_count,
            requests_with_errors_count: stats.requests_with_errors_count,
        }
    }
}

pub struct ReporterManager {
    endpoint: Url,
}

#[async_trait]
impl managed::Manager for ReporterManager {
    type Type = Reporter;
    type Error = ReporterError;

    async fn create(&self) -> Result<Reporter, Self::Error> {
        let url = self.endpoint.to_string();
        Ok(Reporter::try_new(url).await?)
    }

    async fn recycle(&self, _r: &mut Reporter) -> managed::RecycleResult<Self::Error> {
        Ok(())
    }
}

struct DurationHistogram {
    buckets: Vec<i64>,
    entries: u64,
}

impl Default for DurationHistogram {
    fn default() -> Self {
        DurationHistogram::new(None)
    }
}

// The TS implementation of DurationHistogram does Run Length Encoding (RLE)
// to replace sequences of empty buckets with negative numbers. This
// implementation doesn't because:
// Spending too much time in the export() fn exerts back-pressure into the
// telemetry framework and leads to dropped data spans. Given that the
// histogram data is ultimately gzipped for transfer, I wasn't entirely
// sure that this extra processing was worth performing.
impl DurationHistogram {
    const DEFAULT_SIZE: usize = 74; // Taken from TS implementation
    const MAXIMUM_SIZE: usize = 383; // Taken from TS implementation
    const EXPONENT_LOG: f64 = 0.09531017980432493f64; // ln(1.1) Update when ln() is a const fn (see: https://github.com/rust-lang/rust/issues/57241)
    fn new(init_size: Option<usize>) -> Self {
        Self {
            buckets: vec![0; init_size.unwrap_or(DurationHistogram::DEFAULT_SIZE)],
            entries: 0,
        }
    }

    fn duration_to_bucket(duration: Duration) -> usize {
        // If you use as_micros() here to avoid the divide, tests will fail
        // Because, internally, as_micros() is losing remainders
        let log_duration = f64::ln(duration.as_nanos() as f64 / 1000.0);
        let unbounded_bucket = f64::ceil(log_duration / DurationHistogram::EXPONENT_LOG);

        if unbounded_bucket.is_nan() || unbounded_bucket <= 0f64 {
            return 0;
        } else if unbounded_bucket > DurationHistogram::MAXIMUM_SIZE as f64 {
            return DurationHistogram::MAXIMUM_SIZE;
        }

        unbounded_bucket as usize
    }

    fn increment_duration(&mut self, duration: Duration, value: i64) {
        self.increment_bucket(DurationHistogram::duration_to_bucket(duration), value)
    }

    fn increment_bucket(&mut self, bucket: usize, value: i64) {
        if bucket > DurationHistogram::MAXIMUM_SIZE {
            panic!("bucket is out of bounds of the bucket array");
        }
        self.entries += value as u64;
        if bucket >= self.buckets.len() {
            self.buckets.resize(bucket + 1, 0);
        }
        self.buckets[bucket] += value;
    }
}

#[cfg(test)]
mod test {
    use super::*;

    // DurationHistogram Tests
    impl DurationHistogram {
        fn to_array(&self) -> Vec<i64> {
            let mut result = vec![];
            let mut buffered_zeroes = 0;

            for value in &self.buckets {
                if *value == 0 {
                    buffered_zeroes += 1;
                } else {
                    if buffered_zeroes == 1 {
                        result.push(0);
                    } else if buffered_zeroes != 0 {
                        result.push(0 - buffered_zeroes);
                    }
                    result.push(*value);
                    buffered_zeroes = 0;
                }
            }
            result
        }
    }

    #[test]
    fn it_generates_empty_histogram() {
        let histogram = DurationHistogram::new(None);
        let expected: Vec<i64> = vec![];
        assert_eq!(histogram.to_array(), expected);
    }

    #[test]
    fn it_generates_populated_histogram() {
        let mut histogram = DurationHistogram::new(None);
        histogram.increment_bucket(100, 1);
        assert_eq!(histogram.to_array(), vec![-100, 1]);
        histogram.increment_bucket(102, 1);
        assert_eq!(histogram.to_array(), vec![-100, 1, 0, 1]);
        histogram.increment_bucket(382, 1);
        assert_eq!(histogram.to_array(), vec![-100, 1, 0, 1, -279, 1]);
    }

    #[test]
    fn it_buckets_to_zero_and_one() {
        assert_eq!(
            DurationHistogram::duration_to_bucket(Duration::from_nanos(0)),
            0
        );
        assert_eq!(
            DurationHistogram::duration_to_bucket(Duration::from_nanos(1)),
            0
        );
        assert_eq!(
            DurationHistogram::duration_to_bucket(Duration::from_nanos(999)),
            0
        );
        assert_eq!(
            DurationHistogram::duration_to_bucket(Duration::from_nanos(1000)),
            0
        );
        assert_eq!(
            DurationHistogram::duration_to_bucket(Duration::from_nanos(1001)),
            1
        );
    }

    #[test]
    fn it_buckets_to_one_and_two() {
        assert_eq!(
            DurationHistogram::duration_to_bucket(Duration::from_nanos(1100)),
            1
        );
        assert_eq!(
            DurationHistogram::duration_to_bucket(Duration::from_nanos(1101)),
            2
        );
    }

    #[test]
    fn it_buckets_to_threshold() {
        assert_eq!(
            DurationHistogram::duration_to_bucket(Duration::from_nanos(10000)),
            25
        );
        assert_eq!(
            DurationHistogram::duration_to_bucket(Duration::from_nanos(10834)),
            25
        );
        assert_eq!(
            DurationHistogram::duration_to_bucket(Duration::from_nanos(10835)),
            26
        );
    }

    #[test]
    fn it_buckets_common_times() {
        assert_eq!(
            DurationHistogram::duration_to_bucket(Duration::from_nanos(1e5 as u64)),
            49
        );
        assert_eq!(
            DurationHistogram::duration_to_bucket(Duration::from_nanos(1e6 as u64)),
            73
        );
        assert_eq!(
            DurationHistogram::duration_to_bucket(Duration::from_nanos(1e9 as u64)),
            145
        );
    }

    #[test]
    fn it_limits_to_last_bucket() {
        assert_eq!(
            DurationHistogram::duration_to_bucket(Duration::from_nanos(1e64 as u64)),
            DurationHistogram::MAXIMUM_SIZE
        );
    }
}
