use crate::plugins::telemetry::apollo::Config;
use crate::plugins::telemetry::config::MetricsCommon;
use crate::plugins::telemetry::metrics::{MetricsBuilder, MetricsConfigurator};
use crate::stream::StreamExt;
use apollo_spaceport::{
    ContextualizedStats, ReferencedFieldsForType, Reporter, ReporterError, ReporterGraph,
    StatsContext,
};
use async_trait::async_trait;
use deadpool::{managed, Runtime};
use futures::channel::mpsc;
use futures_batch::ChunksTimeoutStreamExt;
use serde::Serialize;
use std::collections::HashMap;
use std::ops::AddAssign;
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

#[derive(Default, Debug, Serialize)]
pub(crate) struct Metrics {
    pub(crate) client_name: String,
    pub(crate) client_version: String,
    pub(crate) stats_report_key: String,
    pub(crate) query_latency_stats: QueryLatencyStats,
    pub(crate) per_type_stat: HashMap<String, TypeStat>,
    pub(crate) referenced_fields_by_type: HashMap<String, ReferencedFieldsForType>,
    pub(crate) operation_count: u64,
}

#[derive(Default, Debug, Serialize)]
pub(crate) struct QueryLatencyStats {
    pub(crate) latency_count: Duration,
    pub(crate) request_count: u64,
    pub(crate) cache_hits: u64,
    pub(crate) persisted_query_hits: u64,
    pub(crate) persisted_query_misses: u64,
    pub(crate) cache_latency_count: Duration,
    pub(crate) root_error_stats: PathErrorStats,
    pub(crate) requests_with_errors_count: u64,
    pub(crate) public_cache_ttl_count: Duration,
    pub(crate) private_cache_ttl_count: Duration,
    pub(crate) registered_operation_count: u64,
    pub(crate) forbidden_operation_count: u64,
    pub(crate) requests_without_field_instrumentation: u64,
}

#[derive(Default, Debug, Serialize)]
pub(crate) struct PathErrorStats {
    pub(crate) children: HashMap<String, PathErrorStats>,
    pub(crate) errors_count: u64,
    pub(crate) requests_with_errors_count: u64,
}

#[derive(Default, Debug, Serialize)]
pub(crate) struct TypeStat {
    pub(crate) per_field_stat: HashMap<String, FieldStat>,
}

#[derive(Default, Debug, Serialize)]
pub(crate) struct FieldStat {
    pub(crate) return_type: String,
    pub(crate) errors_count: u64,
    pub(crate) observed_execution_count: u64,
    pub(crate) estimated_execution_count: f64,
    pub(crate) requests_with_errors_count: u64,
    pub(crate) latency_count: Duration,
}

#[derive(Default)]
struct AggregatedMetrics {
    query_latency_stats: AggregatedQueryLatencyStats,
    per_type_stat: HashMap<String, AggregatedTypeStat>,
    referenced_fields_by_type: HashMap<String, ReferencedFieldsForType>,
    operation_count: u64,
}

impl AddAssign<Metrics> for AggregatedMetrics {
    fn add_assign(&mut self, metrics: Metrics) {
        self.query_latency_stats += metrics.query_latency_stats;
        for (k, v) in metrics.per_type_stat {
            *self.per_type_stat.entry(k).or_default() += v;
        }
        for (k, v) in metrics.referenced_fields_by_type {
            // Merging is not required because metrics are always groupd by schema and query.
            // The tuple (client_name, client_version, stats_report_key, referenced_fields_by_type) is always unique.
            self.referenced_fields_by_type.entry(k).or_insert(v);
        }

        self.operation_count += metrics.operation_count;
    }
}

#[derive(Default)]
struct AggregatedQueryLatencyStats {
    latency_count: DurationHistogram,
    request_count: u64,
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
    requests_without_field_instrumentation: u64,
}

impl AddAssign<QueryLatencyStats> for AggregatedQueryLatencyStats {
    fn add_assign(&mut self, stats: QueryLatencyStats) {
        self.latency_count
            .increment_duration(stats.latency_count, 1);
        self.request_count += stats.request_count;
        self.cache_hits += stats.cache_hits;
        self.persisted_query_hits += stats.persisted_query_hits;
        self.persisted_query_misses += stats.persisted_query_misses;
        self.cache_latency_count
            .increment_duration(stats.cache_latency_count, 1);
        self.root_error_stats += stats.root_error_stats;
        self.requests_with_errors_count += stats.requests_with_errors_count;
        self.public_cache_ttl_count
            .increment_duration(stats.public_cache_ttl_count, 1);
        self.private_cache_ttl_count
            .increment_duration(stats.private_cache_ttl_count, 1);
        self.registered_operation_count += stats.registered_operation_count;
        self.forbidden_operation_count += stats.forbidden_operation_count;
        self.requests_without_field_instrumentation += stats.requests_without_field_instrumentation;
    }
}

#[derive(Default)]
struct AggregatedPathErrorStats {
    children: HashMap<String, AggregatedPathErrorStats>,
    errors_count: u64,
    requests_with_errors_count: u64,
}

impl AddAssign<PathErrorStats> for AggregatedPathErrorStats {
    fn add_assign(&mut self, stats: PathErrorStats) {
        for (k, v) in stats.children.into_iter() {
            *self.children.entry(k).or_default() += v;
        }
        self.errors_count += stats.errors_count;
        self.requests_with_errors_count += stats.requests_with_errors_count;
    }
}

#[derive(Default)]
struct AggregatedTypeStat {
    per_field_stat: HashMap<String, AggregatedFieldStat>,
}

impl AddAssign<TypeStat> for AggregatedTypeStat {
    fn add_assign(&mut self, stat: TypeStat) {
        for (k, v) in stat.per_field_stat.into_iter() {
            *self.per_field_stat.entry(k).or_default() += v;
        }
    }
}

#[derive(Default)]
struct AggregatedFieldStat {
    return_type: String,
    errors_count: u64,
    observed_execution_count: u64,
    estimated_execution_count: f64,
    requests_with_errors_count: u64,
    latency_count: DurationHistogram,
}

impl AddAssign<FieldStat> for AggregatedFieldStat {
    fn add_assign(&mut self, stat: FieldStat) {
        self.latency_count.increment_duration(stat.latency_count, 1);
        self.requests_with_errors_count += stat.requests_with_errors_count;
        self.estimated_execution_count += stat.estimated_execution_count;
        self.observed_execution_count += stat.observed_execution_count;
        self.errors_count += stat.errors_count;
        self.return_type = stat.return_type;
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
        .runtime(Runtime::Tokio1)
        .build()
        .unwrap();

        // This is the thread that actually sends metrics
        tokio::spawn(async move {
            // We want to collect stats into batches, but also send periodically if a batch is not filled.
            // This implementation is not ideal as we do have to store all the data when really it could be folded as it is generated.
            // But in the interested of getting something over the line quickly let's go with this as it is simple to understand.
            rx.chunks_timeout(DEFAULT_BATCH_SIZE, Duration::from_secs(10))
                .for_each(|stats| async {
                    let stats = aggregate(stats);

                    match pool.get().await {
                        Ok(mut reporter) => {
                            for (key, contextualized_stats, field_usage, operation_count) in
                                stats.into_iter()
                            {
                                match reporter
                                    .submit_stats(
                                        reporter_graph.clone(),
                                        key,
                                        contextualized_stats,
                                        field_usage,
                                        operation_count,
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

fn aggregate(
    metrics: Vec<Metrics>,
) -> Vec<(
    String,
    ContextualizedStats,
    HashMap<String, ReferencedFieldsForType>,
    u64,
)> {
    let mut aggregated_metrics = HashMap::<_, AggregatedMetrics>::new();

    for metric in metrics {
        *aggregated_metrics
            .entry(
                (
                    metric.client_name.clone(),
                    metric.client_version.clone(),
                    metric.stats_report_key.clone(),
                )
                    .clone(),
            )
            .or_default() += metric;
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
            contextualized_stats.per_type_stat = aggregated_metric
                .per_type_stat
                .into_iter()
                .map(|(k, v)| (k, v.into()))
                .collect();
            contextualized_stats.query_latency_stats =
                Some(aggregated_metric.query_latency_stats.into());

            (
                key,
                contextualized_stats,
                aggregated_metric.referenced_fields_by_type,
                aggregated_metric.operation_count,
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

impl From<AggregatedTypeStat> for apollo_spaceport::TypeStat {
    fn from(stat: AggregatedTypeStat) -> Self {
        Self {
            per_field_stat: stat
                .per_field_stat
                .into_iter()
                .map(|(k, v)| (k, v.into()))
                .collect(),
        }
    }
}

impl From<AggregatedFieldStat> for apollo_spaceport::FieldStat {
    fn from(stat: AggregatedFieldStat) -> Self {
        Self {
            return_type: stat.return_type,
            errors_count: stat.errors_count,
            observed_execution_count: stat.observed_execution_count,
            estimated_execution_count: stat.estimated_execution_count as u64,
            requests_with_errors_count: stat.requests_with_errors_count,
            latency_count: stat.latency_count.buckets,
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
    use super::super::super::config;
    use super::*;
    use crate::plugins::telemetry::{apollo, Telemetry, EXCLUDE};
    use apollo_router_core::utils::test::IntoSchema::Canned;
    use apollo_router_core::utils::test::PluginTestHarness;
    use apollo_router_core::RouterRequest;
    use apollo_router_core::{Context, Plugin};
    use http::header::HeaderName;
    use std::future::Future;

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

    #[test]
    fn test_aggregation() {
        let metric_1 = create_test_metric("client_1", "version_1", "report_key_1");
        let metric_2 = create_test_metric("client_1", "version_1", "report_key_1");
        let aggregated_metrics = aggregate(vec![metric_1, metric_2]);

        assert_eq!(aggregated_metrics.len(), 1);
        // The way to read snapshot this is that each field should be increasing by 2, except the duration fields which have a histogram.
        insta::with_settings!({sort_maps => true}, {
            insta::assert_json_snapshot!(aggregated_metrics);
        });
    }

    #[test]
    fn test_aggregation_grouping() {
        let metric_1 = create_test_metric("client_1", "version_1", "report_key_1");
        let metric_2 = create_test_metric("client_1", "version_1", "report_key_1");
        let metric_3 = create_test_metric("client_2", "version_1", "report_key_1");
        let metric_4 = create_test_metric("client_1", "version_2", "report_key_1");
        let metric_5 = create_test_metric("client_1", "version_1", "report_key_2");
        let aggregated_metrics = aggregate(vec![metric_1, metric_2, metric_3, metric_4, metric_5]);
        assert_eq!(aggregated_metrics.len(), 4);
    }

    fn create_test_metric(
        client_name: &str,
        client_version: &str,
        stats_report_key: &str,
    ) -> Metrics {
        // This makes me sad. Really this should have just been a case of generate a couple of metrics using
        // a prop testing library and then assert that things got merged OK. But in practise everything was too hard to use

        let mut count = Count::default();

        Metrics {
            client_name: client_name.into(),
            client_version: client_version.into(),
            stats_report_key: stats_report_key.into(),
            query_latency_stats: QueryLatencyStats {
                latency_count: Duration::from_secs(1),
                request_count: count.inc_u64(),
                cache_hits: count.inc_u64(),
                persisted_query_hits: count.inc_u64(),
                persisted_query_misses: count.inc_u64(),
                cache_latency_count: Duration::from_secs(1),
                root_error_stats: PathErrorStats {
                    children: HashMap::from([(
                        "path1".to_string(),
                        PathErrorStats {
                            children: HashMap::from([(
                                "path2".to_string(),
                                PathErrorStats {
                                    children: Default::default(),
                                    errors_count: count.inc_u64(),
                                    requests_with_errors_count: count.inc_u64(),
                                },
                            )]),
                            errors_count: count.inc_u64(),
                            requests_with_errors_count: count.inc_u64(),
                        },
                    )]),
                    errors_count: count.inc_u64(),
                    requests_with_errors_count: count.inc_u64(),
                },
                requests_with_errors_count: count.inc_u64(),
                public_cache_ttl_count: Duration::from_secs(1),
                private_cache_ttl_count: Duration::from_secs(1),
                registered_operation_count: count.inc_u64(),
                forbidden_operation_count: count.inc_u64(),
                requests_without_field_instrumentation: count.inc_u64(),
            },
            per_type_stat: HashMap::from([
                (
                    "type1".into(),
                    TypeStat {
                        per_field_stat: HashMap::from([
                            ("field1".into(), field_stat(&mut count)),
                            ("field2".into(), field_stat(&mut count)),
                        ]),
                    },
                ),
                (
                    "type2".into(),
                    TypeStat {
                        per_field_stat: HashMap::from([
                            ("field1".into(), field_stat(&mut count)),
                            ("field2".into(), field_stat(&mut count)),
                        ]),
                    },
                ),
            ]),
            referenced_fields_by_type: HashMap::from([(
                "type1".into(),
                ReferencedFieldsForType {
                    field_names: vec!["field1".into(), "field2".into()],
                    is_interface: false,
                },
            )]),
            operation_count: count.inc_u64(),
        }
    }

    fn field_stat(count: &mut Count) -> FieldStat {
        FieldStat {
            return_type: "String".into(),
            errors_count: count.inc_u64(),
            observed_execution_count: count.inc_u64(),
            estimated_execution_count: count.inc_f64(),
            requests_with_errors_count: count.inc_u64(),
            latency_count: Duration::from_secs(1),
        }
    }

    #[derive(Default)]
    struct Count {
        count: u64,
    }
    impl Count {
        fn inc_u64(&mut self) -> u64 {
            self.count += 1;
            self.count
        }
        fn inc_f64(&mut self) -> f64 {
            self.count += 1;
            self.count as f64
        }
    }

    #[tokio::test]
    async fn apollo_metrics_disabled() -> Result<(), BoxError> {
        let plugin = create_plugin_with_apollo_copnfig(super::super::apollo::Config {
            endpoint: None,
            apollo_key: None,
            apollo_graph_ref: None,
            client_name_header: HeaderName::from_static("name_header"),
            client_version_header: HeaderName::from_static("version_header"),
        })
        .await?;
        assert!(matches!(plugin.apollo_metrics_sender, Sender::Noop));
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn apollo_metrics_enabled() -> Result<(), BoxError> {
        let plugin = create_plugin().await?;
        assert!(matches!(plugin.apollo_metrics_sender, Sender::Spaceport(_)));
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn apollo_metrics_single_operation() -> Result<(), BoxError> {
        let query = "query {topProducts{name}}";
        let results = get_metrics_for_request(query, None, None).await?;
        insta::with_settings!({sort_maps => true}, {
            insta::assert_json_snapshot!(results);
        });
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn apollo_metrics_multiple_operations() -> Result<(), BoxError> {
        let query = "query {topProducts{name}} query {topProducts{name}}";
        let results = get_metrics_for_request(query, None, None).await?;
        insta::with_settings!({sort_maps => true}, {
            insta::assert_json_snapshot!(results);
        });
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn apollo_metrics_parse_failure() -> Result<(), BoxError> {
        let query = "garbage";
        let results = get_metrics_for_request(query, None, None).await?;
        insta::with_settings!({sort_maps => true}, {
            insta::assert_json_snapshot!(results);
        });
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn apollo_metrics_unknown_operation() -> Result<(), BoxError> {
        let query = "query {topProducts{name}}";
        let results = get_metrics_for_request(query, Some("UNKNOWN"), None).await?;
        insta::with_settings!({sort_maps => true}, {
            insta::assert_json_snapshot!(results);
        });
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn apollo_metrics_validation_failure() -> Result<(), BoxError> {
        let query = "query {topProducts{unknown}}";
        let results = get_metrics_for_request(query, None, None).await?;
        insta::with_settings!({sort_maps => true}, {
            insta::assert_json_snapshot!(results);
        });

        Ok(())
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn apollo_metrics_exclude() -> Result<(), BoxError> {
        let query = "query {topProducts{name}}";
        let context = Context::new();
        context.insert(EXCLUDE, true)?;
        let results = get_metrics_for_request(query, None, Some(context)).await?;
        insta::with_settings!({sort_maps => true}, {
            insta::assert_json_snapshot!(results);
        });

        Ok(())
    }

    async fn get_metrics_for_request(
        query: &str,
        operation_name: Option<&str>,
        context: Option<Context>,
    ) -> Result<Vec<Metrics>, BoxError> {
        let _ = tracing_subscriber::fmt::try_init();
        let mut plugin = create_plugin().await?;
        // Replace the apollo metrics sender so we can test metrics collection.
        let (tx, rx) = futures::channel::mpsc::channel(100);
        plugin.apollo_metrics_sender = Sender::Spaceport(tx);
        let mut test_harness = PluginTestHarness::builder()
            .plugin(plugin)
            .schema(Canned)
            .build()
            .await?;
        let _ = test_harness
            .call(
                RouterRequest::fake_builder()
                    .header("name_header", "test_client")
                    .header("version_header", "1.0-test")
                    .query(query)
                    .and_operation_name(operation_name)
                    .and_context(context)
                    .build()?,
            )
            .await;

        drop(test_harness);
        let results = rx.collect::<Vec<_>>().await;
        Ok(results)
    }

    fn create_plugin() -> impl Future<Output = Result<Telemetry, BoxError>> {
        create_plugin_with_apollo_copnfig(apollo::Config {
            endpoint: None,
            apollo_key: Some("key".to_string()),
            apollo_graph_ref: Some("ref".to_string()),
            client_name_header: HeaderName::from_static("name_header"),
            client_version_header: HeaderName::from_static("version_header"),
        })
    }

    async fn create_plugin_with_apollo_copnfig(
        apollo_config: apollo::Config,
    ) -> Result<Telemetry, BoxError> {
        Telemetry::new(config::Conf {
            metrics: None,
            tracing: None,
            apollo: Some(apollo_config),
        })
        .await
    }
}
