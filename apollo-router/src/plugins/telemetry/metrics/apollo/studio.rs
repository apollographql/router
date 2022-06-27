use std::collections::HashMap;
use std::ops::AddAssign;
use std::time::Duration;
use std::time::SystemTime;

use apollo_spaceport::ReferencedFieldsForType;
use apollo_spaceport::ReportHeader;
use apollo_spaceport::StatsContext;
use itertools::Itertools;
use serde::Serialize;

use super::duration_histogram::DurationHistogram;

impl Report {
    #[cfg(test)]
    fn new(reports: Vec<SingleReport>) -> Report {
        let mut aggregated_report = Report::default();
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
pub(crate) struct SingleReport {
    pub(crate) traces_and_stats: HashMap<String, SingleTracesAndStats>,
    pub(crate) operation_count: u64,
}

#[derive(Default, Debug, Serialize)]
pub(crate) struct SingleTracesAndStats {
    pub(crate) stats_with_context: SingleContextualizedStats,
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
    pub(crate) estimated_execution_count: f64,
    pub(crate) requests_with_errors_count: u64,
    pub(crate) latency: Duration,
}

#[derive(Default, Serialize)]
pub(crate) struct Report {
    traces_per_query: HashMap<String, TracesAndStats>,
    pub(crate) operation_count: u64,
}

impl AddAssign<SingleReport> for Report {
    fn add_assign(&mut self, report: SingleReport) {
        for (k, v) in report.traces_and_stats {
            *self.traces_per_query.entry(k).or_default() += v;
        }

        self.operation_count += report.operation_count;
    }
}

#[derive(Default, Debug, Serialize)]
pub(crate) struct TracesAndStats {
    #[serde(with = "vectorize")]
    pub(crate) stats_with_context: HashMap<StatsContext, ContextualizedStats>,
    pub(crate) referenced_fields_by_type: HashMap<String, ReferencedFieldsForType>,
}

impl AddAssign<SingleTracesAndStats> for TracesAndStats {
    fn add_assign(&mut self, stats: SingleTracesAndStats) {
        *self
            .stats_with_context
            .entry(stats.stats_with_context.context.clone())
            .or_default() += stats.stats_with_context;

        // No merging required here because references fields by type will always be the same for each stats report key.
        self.referenced_fields_by_type = stats.referenced_fields_by_type;
    }
}

#[derive(Default, Debug, Serialize)]
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

#[derive(Default, Debug, Serialize)]
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

#[derive(Default, Debug, Serialize)]
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

#[derive(Default, Debug, Serialize)]
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

#[derive(Default, Debug, Serialize)]
pub(crate) struct FieldStat {
    return_type: String,
    errors_count: u64,
    estimated_execution_count: f64,
    requests_with_errors_count: u64,
    latency: DurationHistogram,
}

impl AddAssign<SingleFieldStat> for FieldStat {
    fn add_assign(&mut self, stat: SingleFieldStat) {
        self.latency.increment_duration(Some(stat.latency), 1);
        self.requests_with_errors_count += stat.requests_with_errors_count;
        self.estimated_execution_count += stat.estimated_execution_count;
        self.errors_count += stat.errors_count;
        self.return_type = stat.return_type;
    }
}

impl From<ContextualizedStats> for apollo_spaceport::ContextualizedStats {
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

impl From<TracesAndStats> for apollo_spaceport::TracesAndStats {
    fn from(stats: TracesAndStats) -> Self {
        Self {
            stats_with_context: stats.stats_with_context.into_values().map_into().collect(),
            referenced_fields_by_type: stats.referenced_fields_by_type,
            ..Default::default()
        }
    }
}

impl From<QueryLatencyStats> for apollo_spaceport::QueryLatencyStats {
    fn from(stats: QueryLatencyStats) -> Self {
        Self {
            latency_count: stats.request_latencies.buckets,
            request_count: stats.request_latencies.entries,
            cache_hits: stats.cache_hits.entries,
            cache_latency_count: stats.cache_hits.buckets,
            persisted_query_hits: stats.persisted_query_hits,
            persisted_query_misses: stats.persisted_query_misses,
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

impl From<PathErrorStats> for apollo_spaceport::PathErrorStats {
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

impl From<TypeStat> for apollo_spaceport::TypeStat {
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

impl From<FieldStat> for apollo_spaceport::FieldStat {
    fn from(stat: FieldStat) -> Self {
        Self {
            return_type: stat.return_type,
            errors_count: stat.errors_count,
            observed_execution_count: stat.latency.entries,
            estimated_execution_count: stat.estimated_execution_count as u64,
            requests_with_errors_count: stat.requests_with_errors_count,
            latency_count: stat.latency.buckets,
        }
    }
}

pub(crate) mod vectorize {
    use serde::Serialize;
    use serde::Serializer;

    pub(crate) fn serialize<'a, T, K, V, S>(target: T, ser: S) -> Result<S::Ok, S::Error>
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
    use std::collections::HashMap;
    use std::time::Duration;

    use apollo_spaceport::ReferencedFieldsForType;

    use super::*;

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
    ) -> SingleReport {
        // This makes me sad. Really this should have just been a case of generate a couple of metrics using
        // a prop testing library and then assert that things got merged OK. But in practise everything was too hard to use

        let mut count = Count::default();

        SingleReport {
            operation_count: count.inc_u64(),
            traces_and_stats: HashMap::from([(
                stats_report_key.to_string(),
                SingleTracesAndStats {
                    stats_with_context: SingleContextualizedStats {
                        context: StatsContext {
                            client_name: client_name.to_string(),
                            client_version: client_version.to_string(),
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
        SingleFieldStat {
            return_type: "String".into(),
            errors_count: count.inc_u64(),
            estimated_execution_count: count.inc_f64(),
            requests_with_errors_count: count.inc_u64(),
            latency: Duration::from_secs(1),
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
