// This entire file is license key functionality
//! Apollo metrics
use crate::plugins::telemetry::apollo::Config;
use crate::plugins::telemetry::config::MetricsCommon;
use crate::plugins::telemetry::metrics::{MetricsBuilder, MetricsConfigurator};
use crate::stream::StreamExt;
use apollo_spaceport::{
    ReferencedFieldsForType, Report, ReportHeader, Reporter, ReporterError, StatsContext,
};
use async_trait::async_trait;
use deadpool::{managed, Runtime};
use duration_histogram::DurationHistogram;
use futures::channel::mpsc;
use futures_batch::ChunksTimeoutStreamExt;
use serde::Serialize;
use std::collections::HashMap;
use std::ops::AddAssign;
use std::time::{Duration, SystemTime};
use tower::BoxError;
use url::Url;

mod duration_histogram;

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

#[derive(Clone, Hash, Eq, PartialEq, Debug, Serialize)]
pub enum MetricsKey {
    Excluded,
    Regular {
        client_name: String,
        client_version: String,
        stats_report_key: String,
    },
}
impl Default for MetricsKey {
    fn default() -> Self {
        MetricsKey::Excluded
    }
}

#[derive(Default, Debug, Serialize)]
pub(crate) struct Metrics {
    pub(crate) key: MetricsKey,
    pub(crate) query_latency_stats: QueryLatencyStats,
    pub(crate) per_type_stat: HashMap<String, TypeStat>,
    pub(crate) referenced_fields_by_type: HashMap<String, ReferencedFieldsForType>,
    pub(crate) operation_count: u64,
}

// TODO Make some of these fields bool
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

#[derive(Default, Serialize)]
struct AggregatedMetrics {
    key: MetricsKey,
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

        // Merging is not required because metrics are always grouped by schema and query.
        // The tuple (client_name, client_version, stats_report_key, referenced_fields_by_type) is always unique.
        // therefore we can just take ownership of the referenced_fields_by_type map.
        self.referenced_fields_by_type = metrics.referenced_fields_by_type;
        self.operation_count += metrics.operation_count;
    }
}

#[derive(Default, Serialize)]
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

#[derive(Default, Serialize)]
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

#[derive(Default, Serialize)]
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

#[derive(Default, Serialize)]
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
                schema_id: Some(schema_id),
                ..
            } => {
                let exporter = ApolloMetricsExporter::new(endpoint, key, reference, schema_id)?;

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
        apollo_key: &str,
        apollo_graph_ref: &str,
        schema_id: &str,
    ) -> Result<ApolloMetricsExporter, BoxError> {
        let apollo_key = apollo_key.to_string();
        // Desired behavior:
        // * Metrics are batched with a timeout.
        // * If we cannot connect to spaceport metrics are discarded and a warning raised.
        // * When the stream of metrics finishes we terminate the thread.
        // * If the exporter is dropped the remaining records are flushed.
        let (tx, rx) = mpsc::channel::<Metrics>(DEFAULT_QUEUE_SIZE);

        // TODO fill out this stuff
        let header = apollo_spaceport::ReportHeader {
            graph_ref: apollo_graph_ref.to_string(),
            hostname: "".to_string(),
            agent_version: "".to_string(),
            service_version: "".to_string(),
            runtime_version: "".to_string(),
            uname: "".to_string(),
            executable_schema_id: schema_id.to_string(),
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
                    let aggregated_metrics = aggregate(stats);

                    match pool.get().await {
                        Ok(mut reporter) => {
                            let report = to_report(header.clone(), aggregated_metrics);
                            match reporter
                                .submit(apollo_spaceport::ReporterRequest {
                                    apollo_key: apollo_key.clone(),
                                    report: Some(report),
                                })
                                .await
                            {
                                Ok(_) => {}
                                Err(e) => {
                                    tracing::warn!("failed to submit stats to spaceport: {}", e);
                                }
                            };
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

fn aggregate(metrics: Vec<Metrics>) -> HashMap<MetricsKey, AggregatedMetrics> {
    let mut aggregated_metrics = HashMap::<_, AggregatedMetrics>::new();
    for metric in metrics {
        *aggregated_metrics.entry(metric.key.clone()).or_default() += metric;
    }
    aggregated_metrics
}

fn to_report(
    header: ReportHeader,
    aggregated_metrics: HashMap<MetricsKey, AggregatedMetrics>,
) -> apollo_spaceport::Report {
    let mut report = Report {
        header: Some(header),
        traces_per_query: Default::default(),
        end_time: Some(SystemTime::now().into()),
        operation_count: 0,
    };

    for (key, metrics) in aggregated_metrics {
        report.operation_count += metrics.operation_count;
        if let MetricsKey::Regular {
            client_name,
            client_version,
            stats_report_key,
        } = key
        {
            report.traces_per_query.insert(
                stats_report_key,
                apollo_spaceport::TracesAndStats {
                    trace: vec![],
                    stats_with_context: vec![apollo_spaceport::ContextualizedStats {
                        context: Some(StatsContext {
                            client_name,
                            client_version,
                        }),
                        query_latency_stats: Some(metrics.query_latency_stats.into()),
                        per_type_stat: metrics
                            .per_type_stat
                            .into_iter()
                            .map(|(k, v)| (k, v.into()))
                            .collect(),
                    }],
                    referenced_fields_by_type: metrics.referenced_fields_by_type,
                    internal_traces_contributing_to_stats: vec![],
                },
            );
        }
    }
    report
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

#[cfg(test)]
mod test {
    use std::future::Future;

    use http::header::HeaderName;

    use apollo_router_core::utils::test::IntoSchema::Canned;
    use apollo_router_core::utils::test::PluginTestHarness;
    use apollo_router_core::RouterRequest;
    use apollo_router_core::{Context, Plugin};

    use crate::plugins::telemetry::{apollo, Telemetry, STUDIO_EXCLUDE};

    use super::super::super::config;
    use super::*;

    #[test]
    fn test_aggregation() {
        let metric_1 = create_test_metric("client_1", "version_1", "report_key_1");
        let metric_2 = create_test_metric("client_1", "version_1", "report_key_1");
        let aggregated_metrics = aggregate(vec![metric_1, metric_2]);

        assert_eq!(aggregated_metrics.len(), 1);
        // The way to read snapshot this is that each field should be increasing by 2, except the duration fields which have a histogram.
        insta::with_settings!({sort_maps => true}, {
            insta::assert_json_snapshot!(aggregated_metrics.get(&MetricsKey::Regular {
                client_name: "client_1".into(),
                client_version: "version_1".into(),
                stats_report_key: "report_key_1".into(),

            }).expect("metric not found"));
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
            key: MetricsKey::Regular {
                client_name: client_name.into(),
                client_version: client_version.into(),
                stats_report_key: stats_report_key.into(),
            },
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
        let plugin = create_plugin_with_apollo_config(super::super::apollo::Config {
            endpoint: None,
            apollo_key: None,
            apollo_graph_ref: None,
            client_name_header: HeaderName::from_static("name_header"),
            client_version_header: HeaderName::from_static("version_header"),
            schema_id: Some("schema_sha".to_string()),
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
        context.insert(STUDIO_EXCLUDE, true)?;
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
        let results = rx
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .map(|mut m| {
                // Fix the latency counts to a known quantity so that insta tests don't fail.
                if m.query_latency_stats.latency_count != Duration::default() {
                    m.query_latency_stats.latency_count = Duration::from_millis(100);
                }
                m
            })
            .collect();
        Ok(results)
    }

    fn create_plugin() -> impl Future<Output = Result<Telemetry, BoxError>> {
        create_plugin_with_apollo_config(apollo::Config {
            endpoint: None,
            apollo_key: Some("key".to_string()),
            apollo_graph_ref: Some("ref".to_string()),
            client_name_header: HeaderName::from_static("name_header"),
            client_version_header: HeaderName::from_static("version_header"),
            schema_id: Some("schema_sha".to_string()),
        })
    }

    async fn create_plugin_with_apollo_config(
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
