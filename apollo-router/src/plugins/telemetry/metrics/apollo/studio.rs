use super::duration_histogram::DurationHistogram;
use apollo_spaceport::{ReferencedFieldsForType, ReportHeader, StatsContext};
use itertools::Itertools;
use serde::Serialize;
use std::collections::HashMap;
use std::ops::AddAssign;
use std::time::{Duration, SystemTime};

impl AggregatedReport {
    pub(crate) fn new(reports: Vec<Report>) -> AggregatedReport {
        let mut aggregated_report = AggregatedReport::default();
        for report in reports {
            aggregated_report += report;
        }
        aggregated_report
    }

    pub(crate) fn into_report(self, header: ReportHeader) -> apollo_spaceport::Report {
        let mut report = apollo_spaceport::Report {
            header: Some(header),
            end_time: Some(SystemTime::now().into()),
            operation_count: self.operation_count,
            ..Default::default()
        };

        for (key, traces_and_stats) in self.traces_per_query {
            report.traces_per_query.insert(key, traces_and_stats.into());
        }
        report
    }
}

#[derive(Default, Debug, Serialize)]
pub(crate) struct Report {
    pub(crate) traces_and_stats: HashMap<String, TracesAndStats>,
    pub(crate) operation_count: u64,
}

#[derive(Default, Debug, Serialize)]
pub(crate) struct TracesAndStats {
    pub(crate) stats_with_context: ContextualizedStats,
    pub(crate) referenced_fields_by_type: HashMap<String, ReferencedFieldsForType>,
}

#[derive(Default, Debug, Serialize)]
pub(crate) struct ContextualizedStats {
    pub(crate) context: StatsContext,
    pub(crate) query_latency_stats: QueryLatencyStats,
    pub(crate) per_type_stat: HashMap<String, TypeStat>,
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
pub(crate) struct AggregatedReport {
    traces_per_query: HashMap<String, AggregatedTracesAndStats>,
    operation_count: u64,
}

impl AddAssign<Report> for AggregatedReport {
    fn add_assign(&mut self, report: Report) {
        for (k, v) in report.traces_and_stats {
            *self.traces_per_query.entry(k).or_default() += v;
        }

        self.operation_count += report.operation_count;
    }
}

#[derive(Default, Debug, Serialize)]
pub(crate) struct AggregatedTracesAndStats {
    #[serde(with = "vectorize")]
    pub(crate) stats_with_context: HashMap<StatsContext, AggregatedContextualizedStats>,
    pub(crate) referenced_fields_by_type: HashMap<String, ReferencedFieldsForType>,
}

impl AddAssign<TracesAndStats> for AggregatedTracesAndStats {
    fn add_assign(&mut self, stats: TracesAndStats) {
        *self
            .stats_with_context
            .entry(stats.stats_with_context.context.clone())
            .or_default() += stats.stats_with_context;

        // No merging required here because references fields by type will always be the same for each stats report key.
        self.referenced_fields_by_type = stats.referenced_fields_by_type;
    }
}

#[derive(Default, Debug, Serialize)]
pub(crate) struct AggregatedContextualizedStats {
    context: StatsContext,
    query_latency_stats: AggregatedQueryLatencyStats,
    per_type_stat: HashMap<String, AggregatedTypeStat>,
}

impl AddAssign<ContextualizedStats> for AggregatedContextualizedStats {
    fn add_assign(&mut self, stats: ContextualizedStats) {
        self.context = stats.context;
        self.query_latency_stats += stats.query_latency_stats;
        for (k, v) in stats.per_type_stat {
            *self.per_type_stat.entry(k).or_default() += v;
        }
    }
}

#[derive(Default, Debug, Serialize)]
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

#[derive(Default, Debug, Serialize)]
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

#[derive(Default, Debug, Serialize)]
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

#[derive(Default, Debug, Serialize)]
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

impl From<AggregatedContextualizedStats> for apollo_spaceport::ContextualizedStats {
    fn from(stats: AggregatedContextualizedStats) -> Self {
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

impl From<AggregatedTracesAndStats> for apollo_spaceport::TracesAndStats {
    fn from(stats: AggregatedTracesAndStats) -> Self {
        Self {
            stats_with_context: stats.stats_with_context.into_values().map_into().collect(),
            referenced_fields_by_type: stats.referenced_fields_by_type,
            ..Default::default()
        }
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

pub mod vectorize {
    use serde::{Serialize, Serializer};

    pub fn serialize<'a, T, K, V, S>(target: T, ser: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
        T: IntoIterator<Item = (&'a K, &'a V)>,
        K: Serialize + 'a,
        V: Serialize + 'a,
    {
        let container: Vec<_> = target.into_iter().collect();
        serde::Serialize::serialize(&container, ser)
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
        let aggregated_metrics = AggregatedReport::new(vec![metric_1, metric_2]);

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
            AggregatedReport::new(vec![metric_1, metric_2, metric_3, metric_4, metric_5]);
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
    ) -> Report {
        // This makes me sad. Really this should have just been a case of generate a couple of metrics using
        // a prop testing library and then assert that things got merged OK. But in practise everything was too hard to use

        let mut count = Count::default();

        Report {
            operation_count: count.inc_u64(),
            traces_and_stats: HashMap::from([(
                stats_report_key.to_string(),
                TracesAndStats {
                    stats_with_context: ContextualizedStats {
                        context: StatsContext {
                            client_name: client_name.to_string(),
                            client_version: client_version.to_string(),
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
