use std::collections::HashMap;
use std::ops::AddAssign;
use std::time::Duration;

use serde::Serialize;
use uuid::Uuid;

use super::duration_histogram::DurationHistogram;
use crate::plugins::telemetry::apollo::LicensedOperationCountByType;
use crate::plugins::telemetry::apollo_exporter::proto::reports::ReferencedFieldsForType;
use crate::plugins::telemetry::apollo_exporter::proto::reports::StatsContext;

#[derive(Default, Debug, Serialize)]
pub(crate) struct SingleStatsReport {
    pub(crate) request_id: Uuid,
    pub(crate) stats: HashMap<String, SingleStats>,
    pub(crate) licensed_operation_count_by_type: Option<LicensedOperationCountByType>,
}

#[derive(Default, Debug, Serialize)]
pub(crate) struct SingleStats {
    pub(crate) stats_with_context: SingleContextualizedStats,
    pub(crate) referenced_fields_by_type: HashMap<String, ReferencedFieldsForType>,
}

#[derive(Default, Debug, Serialize)]
pub(crate) struct Stats {
    pub(crate) stats_with_context: ContextualizedStats,
    pub(crate) referenced_fields_by_type: HashMap<String, ReferencedFieldsForType>,
}

#[derive(Default, Debug, Serialize)]
pub(crate) struct SingleContextualizedStats {
    pub(crate) context: StatsContext,
    pub(crate) query_latency_stats: SingleQueryLatencyStats,
    pub(crate) per_type_stat: HashMap<String, SingleTypeStat>,
}
// TODO Make some of these fields bool
#[derive(Default, Debug, Serialize)]
pub(crate) struct SingleQueryLatencyStats {
    pub(crate) latency: Duration,
    pub(crate) cache_hit: bool,
    pub(crate) persisted_query_hit: Option<bool>,
    pub(crate) cache_latency: Option<Duration>,
    pub(crate) root_error_stats: SinglePathErrorStats,
    pub(crate) has_errors: bool,
    pub(crate) public_cache_ttl_latency: Option<Duration>,
    pub(crate) private_cache_ttl_latency: Option<Duration>,
    pub(crate) registered_operation: bool,
    pub(crate) forbidden_operation: bool,
    pub(crate) without_field_instrumentation: bool,
}

#[derive(Default, Debug, Serialize)]
pub(crate) struct SinglePathErrorStats {
    pub(crate) children: HashMap<String, SinglePathErrorStats>,
    pub(crate) errors_count: u64,
    pub(crate) requests_with_errors_count: u64,
}

#[derive(Default, Debug, Serialize)]
pub(crate) struct SingleTypeStat {
    pub(crate) per_field_stat: HashMap<String, SingleFieldStat>,
}

#[derive(Default, Debug, Serialize)]
pub(crate) struct SingleFieldStat {
    pub(crate) return_type: String,
    pub(crate) errors_count: u64,
    pub(crate) requests_with_errors_count: u64,

    // Floating-point estimates that compensate for the sampling rate,
    // rounded to integers when converting to Protobuf after aggregating
    // a number of requests.
    pub(crate) observed_execution_count: u64,
    pub(crate) latency: DurationHistogram<f64>,
}

#[derive(Clone, Default, Debug, Serialize)]
pub(crate) struct ContextualizedStats {
    context: StatsContext,
    query_latency_stats: QueryLatencyStats,
    per_type_stat: HashMap<String, TypeStat>,
}

impl AddAssign<SingleContextualizedStats> for ContextualizedStats {
    fn add_assign(&mut self, stats: SingleContextualizedStats) {
        self.context = stats.context;
        self.query_latency_stats += stats.query_latency_stats;
        for (k, v) in stats.per_type_stat {
            *self.per_type_stat.entry(k).or_default() += v;
        }
    }
}

#[derive(Clone, Default, Debug, Serialize)]
pub(crate) struct QueryLatencyStats {
    request_latencies: DurationHistogram,
    persisted_query_hits: u64,
    persisted_query_misses: u64,
    cache_hits: DurationHistogram,
    root_error_stats: PathErrorStats,
    requests_with_errors_count: u64,
    public_cache_ttl_count: DurationHistogram,
    private_cache_ttl_count: DurationHistogram,
    registered_operation_count: u64,
    forbidden_operation_count: u64,
    requests_without_field_instrumentation: u64,
}

impl AddAssign<SingleQueryLatencyStats> for QueryLatencyStats {
    fn add_assign(&mut self, stats: SingleQueryLatencyStats) {
        self.request_latencies
            .increment_duration(Some(stats.latency), 1);
        match stats.persisted_query_hit {
            Some(true) => self.persisted_query_hits += 1,
            Some(false) => self.persisted_query_misses += 1,
            None => {}
        }
        self.cache_hits.increment_duration(stats.cache_latency, 1);
        self.root_error_stats += stats.root_error_stats;
        self.requests_with_errors_count += stats.has_errors as u64;
        self.public_cache_ttl_count
            .increment_duration(stats.public_cache_ttl_latency, 1);
        self.private_cache_ttl_count
            .increment_duration(stats.private_cache_ttl_latency, 1);
        self.registered_operation_count += stats.registered_operation as u64;
        self.forbidden_operation_count += stats.forbidden_operation as u64;
        self.requests_without_field_instrumentation += stats.without_field_instrumentation as u64;
    }
}

#[derive(Clone, Default, Debug, Serialize)]
pub(crate) struct PathErrorStats {
    children: HashMap<String, PathErrorStats>,
    errors_count: u64,
    requests_with_errors_count: u64,
}

impl AddAssign<SinglePathErrorStats> for PathErrorStats {
    fn add_assign(&mut self, stats: SinglePathErrorStats) {
        for (k, v) in stats.children.into_iter() {
            *self.children.entry(k).or_default() += v;
        }
        self.errors_count += stats.errors_count;
        self.requests_with_errors_count += stats.requests_with_errors_count;
    }
}

#[derive(Clone, Default, Debug, Serialize)]
pub(crate) struct TypeStat {
    per_field_stat: HashMap<String, FieldStat>,
}

impl AddAssign<SingleTypeStat> for TypeStat {
    fn add_assign(&mut self, stat: SingleTypeStat) {
        for (k, v) in stat.per_field_stat.into_iter() {
            *self.per_field_stat.entry(k).or_default() += v;
        }
    }
}

#[derive(Clone, Default, Debug, Serialize)]
pub(crate) struct FieldStat {
    return_type: String,
    errors_count: u64,
    requests_with_errors_count: u64,
    observed_execution_count: u64,
    // Floating-point estimates that compensate for the sampling rate,
    // rounded to integers when converting to Protobuf after aggregating
    // a number of requests.
    latency: DurationHistogram<f64>,
}

impl AddAssign<SingleFieldStat> for FieldStat {
    fn add_assign(&mut self, stat: SingleFieldStat) {
        self.latency += stat.latency;
        self.requests_with_errors_count += stat.requests_with_errors_count;
        self.observed_execution_count += stat.observed_execution_count;
        self.errors_count += stat.errors_count;
        self.return_type = stat.return_type;
    }
}

impl From<ContextualizedStats>
    for crate::plugins::telemetry::apollo_exporter::proto::reports::ContextualizedStats
{
    fn from(stats: ContextualizedStats) -> Self {
        Self {
            per_type_stat: stats
                .per_type_stat
                .into_iter()
                .map(|(k, v)| (k, v.into()))
                .collect(),
            query_latency_stats: Some(stats.query_latency_stats.into()),
            context: Some(stats.context),
        }
    }
}

impl From<QueryLatencyStats>
    for crate::plugins::telemetry::apollo_exporter::proto::reports::QueryLatencyStats
{
    fn from(stats: QueryLatencyStats) -> Self {
        Self {
            request_count: stats.request_latencies.total,
            latency_count: stats.request_latencies.buckets_to_i64(),
            cache_hits: stats.cache_hits.total,
            cache_latency_count: stats.cache_hits.buckets_to_i64(),
            persisted_query_hits: stats.persisted_query_hits,
            persisted_query_misses: stats.persisted_query_misses,
            root_error_stats: Some(stats.root_error_stats.into()),
            requests_with_errors_count: stats.requests_with_errors_count,
            public_cache_ttl_count: stats.public_cache_ttl_count.buckets_to_i64(),
            private_cache_ttl_count: stats.private_cache_ttl_count.buckets_to_i64(),
            registered_operation_count: stats.registered_operation_count,
            forbidden_operation_count: stats.forbidden_operation_count,
            requests_without_field_instrumentation: stats.requests_without_field_instrumentation,
        }
    }
}

impl From<PathErrorStats>
    for crate::plugins::telemetry::apollo_exporter::proto::reports::PathErrorStats
{
    fn from(stats: PathErrorStats) -> Self {
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

impl From<TypeStat> for crate::plugins::telemetry::apollo_exporter::proto::reports::TypeStat {
    fn from(stat: TypeStat) -> Self {
        Self {
            per_field_stat: stat
                .per_field_stat
                .into_iter()
                .map(|(k, v)| (k, v.into()))
                .collect(),
        }
    }
}

impl From<FieldStat> for crate::plugins::telemetry::apollo_exporter::proto::reports::FieldStat {
    fn from(stat: FieldStat) -> Self {
        Self {
            return_type: stat.return_type,
            errors_count: stat.errors_count,
            requests_with_errors_count: stat.requests_with_errors_count,

            observed_execution_count: stat.observed_execution_count,
            // Round sampling-rate-compensated floating-point estimates to nearest integers:
            estimated_execution_count: stat.latency.total as u64,
            latency_count: stat.latency.buckets_to_i64(),
        }
    }
}

#[cfg(test)]
mod test {
    use std::collections::HashMap;
    use std::time::Duration;

    use super::*;
    use crate::plugins::telemetry::apollo::Report;
    use crate::query_planner::OperationKind;

    #[test]
    fn test_aggregation() {
        let metric_1 = create_test_metric("client_1", "version_1", "report_key_1");
        let metric_2 = create_test_metric("client_1", "version_1", "report_key_1");
        let aggregated_metrics = Report::new(vec![metric_1, metric_2]);
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
        let aggregated_metrics =
            Report::new(vec![metric_1, metric_2, metric_3, metric_4, metric_5]);
        assert_eq!(aggregated_metrics.traces_per_query.len(), 2);
        assert_eq!(
            aggregated_metrics.traces_per_query["report_key_1"]
                .stats_with_context
                .len(),
            3
        );
        assert_eq!(
            aggregated_metrics.traces_per_query["report_key_2"]
                .stats_with_context
                .len(),
            1
        );
    }

    fn create_test_metric(
        client_name: &str,
        client_version: &str,
        stats_report_key: &str,
    ) -> SingleStatsReport {
        // This makes me sad. Really this should have just been a case of generate a couple of metrics using
        // a prop testing library and then assert that things got merged OK. But in practise everything was too hard to use

        let mut count = Count::default();

        SingleStatsReport {
            request_id: Uuid::default(),
            licensed_operation_count_by_type: LicensedOperationCountByType {
                r#type: OperationKind::Query,
                subtype: None,
                licensed_operation_count: count.inc_u64(),
            }
            .into(),
            stats: HashMap::from([(
                stats_report_key.to_string(),
                SingleStats {
                    stats_with_context: SingleContextualizedStats {
                        context: StatsContext {
                            client_name: client_name.to_string(),
                            client_version: client_version.to_string(),
                            operation_type: String::new(),
                            operation_subtype: String::new(),
                        },
                        query_latency_stats: SingleQueryLatencyStats {
                            latency: Duration::from_secs(1),
                            cache_hit: true,
                            persisted_query_hit: Some(true),
                            cache_latency: Some(Duration::from_secs(1)),
                            root_error_stats: SinglePathErrorStats {
                                children: HashMap::from([(
                                    "path1".to_string(),
                                    SinglePathErrorStats {
                                        children: HashMap::from([(
                                            "path2".to_string(),
                                            SinglePathErrorStats {
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
                            has_errors: true,
                            public_cache_ttl_latency: Some(Duration::from_secs(1)),
                            private_cache_ttl_latency: Some(Duration::from_secs(1)),
                            registered_operation: true,
                            forbidden_operation: true,
                            without_field_instrumentation: true,
                        },
                        per_type_stat: HashMap::from([
                            (
                                "type1".into(),
                                SingleTypeStat {
                                    per_field_stat: HashMap::from([
                                        ("field1".into(), field_stat(&mut count)),
                                        ("field2".into(), field_stat(&mut count)),
                                    ]),
                                },
                            ),
                            (
                                "type2".into(),
                                SingleTypeStat {
                                    per_field_stat: HashMap::from([
                                        ("field1".into(), field_stat(&mut count)),
                                        ("field2".into(), field_stat(&mut count)),
                                    ]),
                                },
                            ),
                        ]),
                    },
                    referenced_fields_by_type: HashMap::from([(
                        "type1".into(),
                        ReferencedFieldsForType {
                            field_names: vec!["field1".into(), "field2".into()],
                            is_interface: false,
                        },
                    )]),
                },
            )]),
        }
    }

    fn field_stat(count: &mut Count) -> SingleFieldStat {
        let mut latency = DurationHistogram::default();
        latency.increment_duration(Some(Duration::from_secs(1)), 1.0);
        SingleFieldStat {
            return_type: "String".into(),
            errors_count: count.inc_u64(),
            observed_execution_count: count.inc_u64(),
            requests_with_errors_count: count.inc_u64(),
            latency,
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
    }
}
