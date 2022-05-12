use super::duration_histogram::DurationHistogram;
use apollo_spaceport::{ReferencedFieldsForType, Report, ReportHeader, StatsContext};
use serde::Serialize;
use std::collections::HashMap;
use std::ops::AddAssign;
use std::time::{Duration, SystemTime};

impl AggregatedMetrics {
    pub(crate) fn aggregate(metrics: Vec<Metrics>) -> HashMap<MetricsKey, AggregatedMetrics> {
        let mut aggregated_metrics = HashMap::<_, AggregatedMetrics>::new();
        for metric in metrics {
            *aggregated_metrics.entry(metric.key.clone()).or_default() += metric;
        }
        aggregated_metrics
    }

    pub(crate) fn to_report(
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
pub(crate) struct AggregatedMetrics {
    key: MetricsKey,
    query_latency_stats: AggregatedQueryLatencyStats,
    per_type_stat: HashMap<String, AggregatedTypeStat>,
    referenced_fields_by_type: HashMap<String, ReferencedFieldsForType>,
    pub(crate) operation_count: u64,
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
pub(crate) struct AggregatedQueryLatencyStats {
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
pub(crate) struct AggregatedPathErrorStats {
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
pub(crate) struct AggregatedTypeStat {
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
pub(crate) struct AggregatedFieldStat {
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

#[cfg(test)]
mod test {
    use super::*;
    use apollo_spaceport::ReferencedFieldsForType;
    use std::collections::HashMap;
    use std::time::Duration;

    #[test]
    fn test_aggregation() {
        let metric_1 = create_test_metric("client_1", "version_1", "report_key_1");
        let metric_2 = create_test_metric("client_1", "version_1", "report_key_1");
        let aggregated_metrics = AggregatedMetrics::aggregate(vec![metric_1, metric_2]);

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
        let aggregated_metrics =
            AggregatedMetrics::aggregate(vec![metric_1, metric_2, metric_3, metric_4, metric_5]);
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
}
