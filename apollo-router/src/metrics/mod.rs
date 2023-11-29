#[cfg(test)]
use std::future::Future;
#[cfg(test)]
use std::pin::Pin;
use std::sync::OnceLock;

#[cfg(test)]
use futures::FutureExt;

use crate::metrics::aggregation::AggregateMeterProvider;

pub(crate) mod aggregation;
pub(crate) mod filter;
pub(crate) mod layer;

// During tests this is a task local so that we can test metrics without having to worry about other tests interfering.

#[cfg(test)]
pub(crate) mod test_utils {
    use std::fmt::Debug;
    use std::fmt::Display;
    use std::sync::Arc;
    use std::sync::OnceLock;
    use std::sync::Weak;

    use itertools::Itertools;
    use num_traits::NumCast;
    use num_traits::ToPrimitive;
    use opentelemetry::Array;
    use opentelemetry::KeyValue;
    use opentelemetry::Value;
    use opentelemetry_sdk::metrics::data::Gauge;
    use opentelemetry_sdk::metrics::data::Histogram;
    use opentelemetry_sdk::metrics::data::ResourceMetrics;
    use opentelemetry_sdk::metrics::data::Sum;
    use opentelemetry_sdk::metrics::data::Temporality;
    use opentelemetry_sdk::metrics::reader::AggregationSelector;
    use opentelemetry_sdk::metrics::reader::MetricReader;
    use opentelemetry_sdk::metrics::reader::TemporalitySelector;
    use opentelemetry_sdk::metrics::Aggregation;
    use opentelemetry_sdk::metrics::InstrumentKind;
    use opentelemetry_sdk::metrics::ManualReader;
    use opentelemetry_sdk::metrics::MeterProviderBuilder;
    use opentelemetry_sdk::metrics::Pipeline;
    use opentelemetry_sdk::AttributeSet;
    use tokio::task_local;

    use crate::metrics::aggregation::AggregateMeterProvider;
    use crate::metrics::aggregation::MeterProviderType;
    use crate::metrics::filter::FilterMeterProvider;
    task_local! {
        pub(crate) static AGGREGATE_METER_PROVIDER_ASYNC: OnceLock<(AggregateMeterProvider, ClonableManualReader)>;
    }
    thread_local! {
        pub(crate) static AGGREGATE_METER_PROVIDER: OnceLock<(AggregateMeterProvider, ClonableManualReader)> = OnceLock::new();
    }

    #[derive(Debug, Clone, Default)]
    pub(crate) struct ClonableManualReader {
        reader: Arc<ManualReader>,
    }

    impl TemporalitySelector for ClonableManualReader {
        fn temporality(&self, kind: InstrumentKind) -> Temporality {
            self.reader.temporality(kind)
        }
    }

    impl AggregationSelector for ClonableManualReader {
        fn aggregation(&self, kind: InstrumentKind) -> Aggregation {
            self.reader.aggregation(kind)
        }
    }
    impl MetricReader for ClonableManualReader {
        fn register_pipeline(&self, pipeline: Weak<Pipeline>) {
            self.reader.register_pipeline(pipeline)
        }

        fn collect(&self, rm: &mut ResourceMetrics) -> opentelemetry::metrics::Result<()> {
            self.reader.collect(rm)
        }

        fn force_flush(&self) -> opentelemetry::metrics::Result<()> {
            self.reader.force_flush()
        }

        fn shutdown(&self) -> opentelemetry::metrics::Result<()> {
            self.reader.shutdown()
        }
    }

    fn create_test_meter_provider() -> (AggregateMeterProvider, ClonableManualReader) {
        {
            let meter_provider = AggregateMeterProvider::default();
            let reader = ClonableManualReader::default();

            meter_provider.set(
                MeterProviderType::Public,
                Some(FilterMeterProvider::public_metrics(
                    MeterProviderBuilder::default()
                        .with_reader(reader.clone())
                        .build(),
                )),
            );

            (meter_provider, reader)
        }
    }
    pub(crate) fn meter_provider_and_readers() -> (AggregateMeterProvider, ClonableManualReader) {
        if tokio::runtime::Handle::try_current().is_ok() {
            if let Ok(task_local) = AGGREGATE_METER_PROVIDER_ASYNC
                .try_with(|cell| cell.get_or_init(create_test_meter_provider).clone())
            {
                task_local
            } else {
                // We need to silently fail here. Otherwise we fail every multi-threaded test that touches metrics
                (
                    AggregateMeterProvider::default(),
                    ClonableManualReader::default(),
                )
            }
        } else {
            AGGREGATE_METER_PROVIDER
                .with(|cell| cell.get_or_init(create_test_meter_provider).clone())
        }
    }

    pub(crate) struct Metrics {
        resource_metrics: ResourceMetrics,
    }

    impl Default for Metrics {
        fn default() -> Self {
            Metrics {
                resource_metrics: ResourceMetrics {
                    resource: Default::default(),
                    scope_metrics: vec![],
                },
            }
        }
    }

    pub(crate) fn collect_metrics() -> Metrics {
        let mut metrics = Metrics::default();
        let (_, reader) = meter_provider_and_readers();
        reader.collect(&mut metrics.resource_metrics).unwrap();
        metrics
    }

    impl Metrics {
        pub(crate) fn find(&self, name: &str) -> Option<&opentelemetry_sdk::metrics::data::Metric> {
            self.resource_metrics
                .scope_metrics
                .iter()
                .flat_map(|scope_metrics| {
                    scope_metrics
                        .metrics
                        .iter()
                        .filter(|metric| metric.name == name)
                })
                .next()
        }

        pub(crate) fn assert<T: NumCast + Display + 'static>(
            &self,
            name: &str,
            ty: MetricType,
            value: T,
            attributes: &[KeyValue],
        ) {
            let attributes = AttributeSet::from(attributes);
            if let Some(value) = value.to_u64() {
                if self.metric_exists(name, &ty, value, &attributes) {
                    return;
                }
            }

            if let Some(value) = value.to_i64() {
                if self.metric_exists(name, &ty, value, &attributes) {
                    return;
                }
            }

            if let Some(value) = value.to_f64() {
                if self.metric_exists(name, &ty, value, &attributes) {
                    return;
                }
            }

            self.panic_metric_not_found(name, value, &attributes);
        }

        fn panic_metric_not_found<T: Display + 'static>(
            &self,
            name: &str,
            value: T,
            attributes: &AttributeSet,
        ) {
            panic!(
                "metric: {}, {}, {} not found.\nMetrics that were found:\n{}",
                name,
                value,
                Self::pretty_attributes(attributes),
                self.resource_metrics
                    .scope_metrics
                    .iter()
                    .flat_map(|scope_metrics| { scope_metrics.metrics.iter() })
                    .flat_map(|metric| { Self::pretty_metric(metric) })
                    .map(|metric| { format!("  {}", metric) })
                    .join("\n")
            )
        }

        fn pretty_metric(metric: &opentelemetry_sdk::metrics::data::Metric) -> Vec<String> {
            let mut results = Vec::new();
            results.append(&mut Self::pretty_data_point::<u64>(metric));
            results.append(&mut Self::pretty_data_point::<i64>(metric));
            results.append(&mut Self::pretty_data_point::<f64>(metric));
            results
        }

        fn pretty_data_point<T: Display + 'static>(
            metric: &opentelemetry_sdk::metrics::data::Metric,
        ) -> Vec<String> {
            let mut results = Vec::new();
            if let Some(gauge) = metric.data.as_any().downcast_ref::<Gauge<T>>() {
                for datapoint in gauge.data_points.iter() {
                    results.push(format!(
                        "\"{}\", {}, {}",
                        metric.name,
                        datapoint.value,
                        Self::pretty_attributes(&datapoint.attributes)
                    ));
                }
            }
            if let Some(sum) = metric.data.as_any().downcast_ref::<Sum<T>>() {
                for datapoint in sum.data_points.iter() {
                    results.push(format!(
                        "\"{}\", {}, {}",
                        metric.name,
                        datapoint.value,
                        Self::pretty_attributes(&datapoint.attributes)
                    ));
                }
            }
            if let Some(histogram) = metric.data.as_any().downcast_ref::<Histogram<T>>() {
                for datapoint in histogram.data_points.iter() {
                    results.push(format!(
                        "\"{}\", {}, {}",
                        metric.name,
                        datapoint.sum,
                        Self::pretty_attributes(&datapoint.attributes)
                    ));
                }
            }

            results
        }

        fn pretty_attributes(attributes: &AttributeSet) -> String {
            attributes
                .iter()
                .map(|(key, value)| {
                    format!(
                        "\"{}\" => {}",
                        key.as_str(),
                        match value {
                            Value::Bool(v) => {
                                v.to_string()
                            }
                            Value::I64(v) => {
                                v.to_string()
                            }
                            Value::F64(v) => {
                                format!("{}f64", v)
                            }
                            Value::String(v) => {
                                format!("\"{}\"", v)
                            }
                            Value::Array(Array::Bool(v)) => {
                                format!("[{}]", v.iter().map(|v| v.to_string()).join(", "))
                            }
                            Value::Array(Array::F64(v)) => {
                                format!("[{}]", v.iter().map(|v| format!("{}f64", v)).join(", "))
                            }
                            Value::Array(Array::I64(v)) => {
                                format!("[{}]", v.iter().map(|v| v.to_string()).join(", "))
                            }
                            Value::Array(Array::String(v)) => {
                                format!("[{}]", v.iter().map(|v| format!("\"{}\"", v)).join(", "))
                            }
                        }
                    )
                })
                .join(", ")
        }

        fn metric_exists<T: Debug + PartialEq + Display + ToPrimitive + 'static>(
            &self,
            name: &str,
            ty: &MetricType,
            value: T,
            attributes: &AttributeSet,
        ) -> bool {
            if let Some(metric) = self.find(name) {
                // Try to downcast the metric to each type of aggregation and assert that the value is correct.
                if let Some(gauge) = metric.data.as_any().downcast_ref::<Gauge<T>>() {
                    // Find the datapoint with the correct attributes.
                    if matches!(ty, MetricType::Gauge) {
                        return gauge.data_points.iter().any(|datapoint| {
                            datapoint.attributes == *attributes && datapoint.value == value
                        });
                    }
                } else if let Some(sum) = metric.data.as_any().downcast_ref::<Sum<T>>() {
                    // Note that we can't actually tell if the sum is monotonic or not, so we just check if it's a sum.
                    if matches!(ty, MetricType::Counter | MetricType::UpDownCounter) {
                        return sum.data_points.iter().any(|datapoint| {
                            datapoint.attributes == *attributes && datapoint.value == value
                        });
                    }
                } else if let Some(histogram) = metric.data.as_any().downcast_ref::<Histogram<T>>()
                {
                    if matches!(ty, MetricType::Histogram) {
                        if let Some(value) = value.to_u64() {
                            return histogram.data_points.iter().any(|datapoint| {
                                datapoint.attributes == *attributes && datapoint.count == value
                            });
                        }
                    }
                }
            }
            false
        }
    }

    pub(crate) enum MetricType {
        Counter,
        UpDownCounter,
        Histogram,
        Gauge,
    }
}
#[cfg(test)]
pub(crate) fn meter_provider() -> AggregateMeterProvider {
    test_utils::meter_provider_and_readers().0
}

#[cfg(test)]
pub(crate) use test_utils::collect_metrics;

#[cfg(not(test))]
static AGGREGATE_METER_PROVIDER: OnceLock<AggregateMeterProvider> = OnceLock::new();
#[cfg(not(test))]
pub(crate) fn meter_provider() -> AggregateMeterProvider {
    AGGREGATE_METER_PROVIDER
        .get_or_init(Default::default)
        .clone()
}

#[macro_export]
/// Get or create a u64 monotonic counter metric and add a value to it
///
/// This macro is a replacement for the telemetry crate's MetricsLayer. We will eventually convert all metrics to use these macros and deprecate the MetricsLayer.
/// The reason for this is that the MetricsLayer has:
/// * No support for dynamic attributes
/// * No support dynamic metrics.
/// * Imperfect mapping to metrics API that can only be checked at runtime.
/// New metrics should be added using these macros.
#[allow(unused_macros)]
macro_rules! u64_counter {
    ($($name:ident).+, $description:literal, $value: expr, $($attr_key:literal = $attr_value:expr),+) => {
        let attributes = vec![$(opentelemetry::KeyValue::new($attr_key, $attr_value)),+];
        metric!(u64, counter, add, stringify!($($name).+), $description, $value, &attributes);
    };

    ($($name:ident).+, $description:literal, $value: expr, $($($attr_key:ident).+ = $attr_value:expr),+) => {
        let attributes = vec![$(opentelemetry::KeyValue::new(stringify!($($attr_key).+), $attr_value)),+];
        metric!(u64, counter, add, stringify!($($name).+), $description, $value, &attributes);
    };

    ($name:literal, $description:literal, $value: expr, $($attr_key:literal = $attr_value:expr),+) => {
        let attributes = vec![$(opentelemetry::KeyValue::new($attr_key, $attr_value)),+];
        metric!(u64, counter, add, $name, $description, $value, &attributes);
    };

    ($name:literal, $description:literal, $value: expr, $($($attr_key:ident).+ = $attr_value:expr),+) => {
        let attributes = vec![$(opentelemetry::KeyValue::new(stringify!($($attr_key).+), $attr_value)),+];
        metric!(u64, counter, add, $name, $description, $value, &attributes);
    };

    ($name:literal, $description:literal, $value: expr, $attrs: expr) => {
        metric!(u64, counter, add, $name, $description, $value, $attrs);
    };

    ($name:literal, $description:literal, $value: expr) => {
        metric!(u64, counter, add, $name, $description, $value, []);
    }
}

/// Get or create a f64 monotonic counter metric and add a value to it
///
/// This macro is a replacement for the telemetry crate's MetricsLayer. We will eventually convert all metrics to use these macros and deprecate the MetricsLayer.
/// The reason for this is that the MetricsLayer has:
/// * No support for dynamic attributes
/// * No support dynamic metrics.
/// * Imperfect mapping to metrics API that can only be checked at runtime.
/// New metrics should be added using these macros.
#[allow(unused_macros)]
macro_rules! f64_counter {
    ($($name:ident).+, $description:literal, $value: expr, $($attr_key:literal = $attr_value:expr),+) => {
        let attributes = vec![$(opentelemetry::KeyValue::new($attr_key, $attr_value)),+];
        metric!(f64, counter, add, stringify!($($name).+), $description, $value, &attributes);
    };

    ($($name:ident).+, $description:literal, $value: expr, $($($attr_key:ident).+ = $attr_value:expr),+) => {
        let attributes = vec![$(opentelemetry::KeyValue::new(stringify!($($attr_key).+), $attr_value)),+];
        metric!(f64, counter, add, stringify!($($name).+), $description, $value, &attributes);
    };

    ($name:literal, $description:literal, $value: expr, $($attr_key:literal = $attr_value:expr),+) => {
        let attributes = vec![$(opentelemetry::KeyValue::new($attr_key, $attr_value)),+];
        metric!(f64, counter, add, $name, $description, $value, &attributes);
    };

    ($name:literal, $description:literal, $value: expr, $($($attr_key:ident).+ = $attr_value:expr),+) => {
        let attributes = vec![$(opentelemetry::KeyValue::new(stringify!($($attr_key).+), $attr_value)),+];
        metric!(f64, counter, add, $name, $description, $value, &attributes);
    };
    ($name:literal, $description:literal, $value: expr, $attrs: expr) => {
        metric!(f64, counter, add, $name, $description, $value, $attrs);
    };

    ($name:literal, $description:literal, $value: expr) => {
        metric!(f64, counter, add, $name, $description, $value, &[]);
    }
}

/// Get or create an i64 up down counter metric and add a value to it
///
/// This macro is a replacement for the telemetry crate's MetricsLayer. We will eventually convert all metrics to use these macros and deprecate the MetricsLayer.
/// The reason for this is that the MetricsLayer has:
/// * No support for dynamic attributes
/// * No support dynamic metrics.
/// * Imperfect mapping to metrics API that can only be checked at runtime.
/// New metrics should be added using these macros.

#[allow(unused_macros)]
macro_rules! i64_up_down_counter {
    ($($name:ident).+, $description:literal, $value: expr, $($attr_key:literal = $attr_value:expr),+) => {
        let attributes = vec![$(opentelemetry::KeyValue::new($attr_key, $attr_value)),+];
        metric!(i64, up_down_counter, add, stringify!($($name).+), $description, $value, &attributes);
    };

    ($($name:ident).+, $description:literal, $value: expr, $($($attr_key:ident).+ = $attr_value:expr),+) => {
        let attributes = vec![$(opentelemetry::KeyValue::new(stringify!($($attr_key).+), $attr_value)),+];
        metric!(i64, up_down_counter, add, stringify!($($name).+), $description, $value, &attributes);
    };

    ($name:literal, $description:literal, $value: expr, $($attr_key:literal = $attr_value:expr),+) => {
        let attributes = vec![$(opentelemetry::KeyValue::new($attr_key, $attr_value)),+];
        metric!(i64, up_down_counter, add, $name, $description, $value, &attributes);
    };

    ($name:literal, $description:literal, $value: expr, $($($attr_key:ident).+ = $attr_value:expr),+) => {
        let attributes = vec![$(opentelemetry::KeyValue::new(stringify!($($attr_key).+), $attr_value)),+];
        metric!(i64, up_down_counter, add, $name, $description, $value, &attributes);
    };

    ($name:literal, $description:literal, $value: expr, $attrs: expr) => {
        metric!(i64, up_down_counter, add, $name, $description, $value, $attrs);
    };

    ($name:literal, $description:literal, $value: expr) => {
        metric!(i64, up_down_counter, add, $name, $description, $value, &[]);
    };
}

/// Get or create an f64 up down counter metric and add a value to it
///
/// This macro is a replacement for the telemetry crate's MetricsLayer. We will eventually convert all metrics to use these macros and deprecate the MetricsLayer.
/// The reason for this is that the MetricsLayer has:
/// * No support for dynamic attributes
/// * No support dynamic metrics.
/// * Imperfect mapping to metrics API that can only be checked at runtime.
/// New metrics should be added using these macros.
#[allow(unused_macros)]
macro_rules! f64_up_down_counter {
    ($($name:ident).+, $description:literal, $value: expr, $($attr_key:literal = $attr_value:expr),+) => {
        let attributes = vec![$(opentelemetry::KeyValue::new($attr_key, $attr_value)),+];
        metric!(f64, up_down_counter, add, stringify!($($name).+), $description, $value, &attributes);
    };

    ($($name:ident).+, $description:literal, $value: expr, $($($attr_key:ident).+ = $attr_value:expr),+) => {
        let attributes = vec![$(opentelemetry::KeyValue::new(stringify!($($attr_key).+), $attr_value)),+];
        metric!(f64, up_down_counter, add, stringify!($($name).+), $description, $value, &attributes);
    };

    ($name:literal, $description:literal, $value: expr, $($attr_key:literal = $attr_value:expr),+) => {
        let attributes = vec![$(opentelemetry::KeyValue::new($attr_key, $attr_value)),+];
        metric!(f64, up_down_counter, add, $name, $description, $value, &attributes);
    };

    ($name:literal, $description:literal, $value: expr, $($($attr_key:ident).+ = $attr_value:expr),+) => {
        let attributes = vec![$(opentelemetry::KeyValue::new(stringify!($($attr_key).+), $attr_value)),+];
        metric!(f64, up_down_counter, add, $name, $description, $value, &attributes);
    };

    ($name:literal, $description:literal, $value: expr, $attrs: expr) => {
        metric!(f64, up_down_counter, add, $name, $description, $value, $attrs);
    };

    ($name:literal, $description:literal, $value: expr) => {
        metric!(f64, up_down_counter, add, $name, $description, $value, &[]);
    };
}

/// Get or create an f64 histogram metric and add a value to it
///
/// This macro is a replacement for the telemetry crate's MetricsLayer. We will eventually convert all metrics to use these macros and deprecate the MetricsLayer.
/// The reason for this is that the MetricsLayer has:
/// * No support for dynamic attributes
/// * No support dynamic metrics.
/// * Imperfect mapping to metrics API that can only be checked at runtime.
/// New metrics should be added using these macros.
#[allow(unused_macros)]
macro_rules! f64_histogram {
    ($($name:ident).+, $description:literal, $value: expr, $($attr_key:literal = $attr_value:expr),+) => {
        let attributes = vec![$(opentelemetry::KeyValue::new($attr_key, $attr_value)),+];
        metric!(f64, histogram, record, stringify!($($name).+), $description, $value, &attributes);
    };

    ($($name:ident).+, $description:literal, $value: expr, $($($attr_key:ident).+ = $attr_value:expr),+) => {
        let attributes = vec![$(opentelemetry::KeyValue::new(stringify!($($attr_key).+), $attr_value)),+];
        metric!(f64, histogram, record, stringify!($($name).+), $description, $value, &attributes);
    };

    ($name:literal, $description:literal, $value: expr, $($attr_key:literal = $attr_value:expr),+) => {
        let attributes = vec![$(opentelemetry::KeyValue::new($attr_key, $attr_value)),+];
        metric!(f64, histogram, record, $name, $description, $value, &attributes);
    };

    ($name:literal, $description:literal, $value: expr, $($($attr_key:ident).+ = $attr_value:expr),+) => {
        let attributes = vec![$(opentelemetry::KeyValue::new(stringify!($($attr_key).+), $attr_value)),+];
        metric!(f64, histogram, record, $name, $description, $value, &attributes);
    };

    ($name:literal, $description:literal, $value: expr, $attrs: expr) => {
        metric!(f64, histogram, record, $name, $description, $value, $attrs);
    };

    ($name:literal, $description:literal, $value: expr) => {
        metric!(f64, histogram, record, $name, $description, $value, &[]);
    };
}

/// Get or create an u64 histogram metric and add a value to it
///
/// This macro is a replacement for the telemetry crate's MetricsLayer. We will eventually convert all metrics to use these macros and deprecate the MetricsLayer.
/// The reason for this is that the MetricsLayer has:
/// * No support for dynamic attributes
/// * No support dynamic metrics.
/// * Imperfect mapping to metrics API that can only be checked at runtime.
/// New metrics should be added using these macros.
#[allow(unused_macros)]
macro_rules! u64_histogram {
    ($($name:ident).+, $description:literal, $value: expr, $($attr_key:literal = $attr_value:expr),+) => {
        let attributes = vec![$(opentelemetry::KeyValue::new($attr_key, $attr_value)),+];
        metric!(u64, histogram, record, stringify!($($name).+), $description, $value, &attributes);
    };

    ($($name:ident).+, $description:literal, $value: expr, $($($attr_key:ident).+ = $attr_value:expr),+) => {
        let attributes = vec![$(opentelemetry::KeyValue::new(stringify!($($attr_key).+), $attr_value)),+];
        metric!(u64, histogram, record, stringify!($($name).+), $description, $value, &attributes);
    };

    ($name:literal, $description:literal, $value: expr, $($attr_key:literal = $attr_value:expr),+) => {
        let attributes = vec![$(opentelemetry::KeyValue::new($attr_key, $attr_value)),+];
        metric!(u64, histogram, record, $name, $description, $value, &attributes);
    };

    ($name:literal, $description:literal, $value: expr, $($($attr_key:ident).+ = $attr_value:expr),+) => {
        let attributes = vec![$(opentelemetry::KeyValue::new(stringify!($($attr_key).+), $attr_value)),+];
        metric!(u64, histogram, record, $name, $description, $value, &attributes);
    };

    ($name:literal, $description:literal, $value: expr, $attrs: expr) => {
        metric!(u64, histogram, record, $name, $description, $value, $attrs);
    };

    ($name:literal, $description:literal, $value: expr) => {
        metric!(u64, histogram, record, $name, $description, $value, &[]);
    };
}

/// Get or create an i64 histogram metric and add a value to it
///
/// This macro is a replacement for the telemetry crate's MetricsLayer. We will eventually convert all metrics to use these macros and deprecate the MetricsLayer.
/// The reason for this is that the MetricsLayer has:
/// * No support for dynamic attributes
/// * No support dynamic metrics.
/// * Imperfect mapping to metrics API that can only be checked at runtime.
/// New metrics should be added using these macros.
#[allow(unused_macros)]
macro_rules! i64_histogram {
    ($($name:ident).+, $description:literal, $value: expr, $($attr_key:literal = $attr_value:expr),+) => {
        let attributes = vec![$(opentelemetry::KeyValue::new($attr_key, $attr_value)),+];
        metric!(i64, histogram, record, stringify!($($name).+), $description, $value, &attributes);
    };

    ($($name:ident).+, $description:literal, $value: expr, $($($attr_key:ident).+ = $attr_value:expr),+) => {
        let attributes = vec![$(opentelemetry::KeyValue::new(stringify!($($attr_key).+), $attr_value)),+];
        metric!(i64, histogram, record, stringify!($($name).+), $description, $value, &attributes);
    };

    ($name:literal, $description:literal, $value: expr, $($attr_key:literal = $attr_value:expr),+) => {
        let attributes = vec![$(opentelemetry::KeyValue::new($attr_key, $attr_value)),+];
        metric!(i64, histogram, record, $name, $description, $value, &attributes);
    };

    ($name:literal, $description:literal, $value: expr, $($($attr_key:ident).+ = $attr_value:expr),+) => {
        let attributes = vec![$(opentelemetry::KeyValue::new(stringify!($($attr_key).+), $attr_value)),+];
        metric!(i64, histogram, record, $name, $description, $value, &attributes);
    };

    ($name:literal, $description:literal, $value: expr, $attrs: expr) => {
        metric!(i64, histogram, record, $name, $description, $value, $attrs);
    };

    ($name:literal, $description:literal, $value: expr) => {
        metric!(i64, histogram, record, $name, $description, $value, &[]);
    };
}

thread_local! {
    // This is used exactly once in testing callsite caching.
    #[cfg(test)]
    pub(crate) static CACHE_CALLSITE: std::sync::atomic::AtomicBool = const {std::sync::atomic::AtomicBool::new(false)};
}
macro_rules! metric {
    ($ty:ident, $instrument:ident, $mutation:ident, $name:expr, $description:literal, $value: expr, $attrs: expr) => {

        // The way this works is that we have a static at each call site that holds a weak reference to the instrument.
        // We make a call we try to upgrade the weak reference. If it succeeds we use the instrument.
        // Otherwise we create a new instrument and update the static.
        // The aggregate meter provider is used to hold on to references of all instruments that have been created and will clear references when the underlying configuration has changed.
        // There is a Mutex involved, however it is only locked for the duration of the upgrade once the instrument has been created.
        // The Reason a Mutex is used rather than an RwLock is that we are not holding the lock for any significant period of time and the cost of an RwLock is potentially higher.
        // If we profile and deem it's worth switching to RwLock then we can do that.

        paste::paste! {
            {
                // There is a single test for caching callsites. Other tests do not cache because they will interfere with each other due to them using a task local meter provider to aid testing.
                #[cfg(test)]
                let cache_callsite = crate::metrics::CACHE_CALLSITE.with(|cell| cell.load(std::sync::atomic::Ordering::SeqCst));

                // The compiler will optimize this in non test builds
                #[cfg(not(test))]
                let cache_callsite = true;

                if cache_callsite {
                    static INSTRUMENT_CACHE: std::sync::OnceLock<std::sync::Mutex<std::sync::Weak<opentelemetry::metrics::[<$instrument:camel>]<$ty>>>> = std::sync::OnceLock::new();

                    let mut instrument_guard = INSTRUMENT_CACHE
                        .get_or_init(|| {
                            let meter_provider = crate::metrics::meter_provider();
                            let instrument_ref = meter_provider.create_registered_instrument(|p| p.meter("apollo/router").[<$ty _ $instrument>]($name).with_description($description).init());
                            std::sync::Mutex::new(std::sync::Arc::downgrade(&instrument_ref))
                        })
                        .lock()
                        .expect("lock poisoned");
                    let instrument = if let Some(instrument) = instrument_guard.upgrade() {
                        // Fast path, we got the instrument, drop the mutex guard immediately.
                        drop(instrument_guard);
                        instrument
                    } else {
                        // Slow path, we need to obtain the instrument again.
                        let meter_provider = crate::metrics::meter_provider();
                        let instrument_ref = meter_provider.create_registered_instrument(|p| p.meter("apollo/router").[<$ty _ $instrument>]($name).with_description($description).init());
                        *instrument_guard = std::sync::Arc::downgrade(&instrument_ref);
                        // We've updated the instrument and got a strong reference to it. We can drop the mutex guard now.
                        drop(instrument_guard);
                        instrument_ref
                    };
                    instrument.$mutation($value, &$attrs);
                }
                else {
                    let meter_provider = crate::metrics::meter_provider();
                    let meter = opentelemetry::metrics::MeterProvider::meter(&meter_provider, "apollo/router");
                    let instrument = meter.[<$ty _ $instrument>]($name).with_description($description).init();
                    instrument.$mutation($value, &$attrs);
                }
            }
        }
    };
}

#[cfg(test)]
macro_rules! assert_counter {
    ($($name:ident).+, $value: expr, $($attr_key:literal = $attr_value:expr),+) => {
        let attributes = vec![$(opentelemetry::KeyValue::new($attr_key, $attr_value)),+];
        crate::metrics::collect_metrics().assert(stringify!($($name).+), crate::metrics::test_utils::MetricType::Counter, $value, &attributes);
    };

    ($($name:ident).+, $value: expr, $($($attr_key:ident).+ = $attr_value:expr),+) => {
        let attributes = vec![$(opentelemetry::KeyValue::new(stringify!($($attr_key).+), $attr_value)),+];
        crate::metrics::collect_metrics().assert(stringify!($($name).+), crate::metrics::test_utils::MetricType::Counter, $value, &attributes);
    };

    ($name:literal, $value: expr, $($attr_key:literal = $attr_value:expr),+) => {
        let attributes = vec![$(opentelemetry::KeyValue::new($attr_key, $attr_value)),+];
        crate::metrics::collect_metrics().assert($name, crate::metrics::test_utils::MetricType::Counter, $value, &attributes);
    };

    ($name:literal, $value: expr, $($($attr_key:ident).+ = $attr_value:expr),+) => {
        let attributes = vec![$(opentelemetry::KeyValue::new(stringify!($($attr_key).+), $attr_value)),+];
        crate::metrics::collect_metrics().assert($name, crate::metrics::test_utils::MetricType::Counter, $value, &attributes);
    };

    ($name:literal, $value: expr) => {
        crate::metrics::collect_metrics().assert($name, crate::metrics::test_utils::MetricType::Counter, $value, &[]);
    };
}

#[cfg(test)]
macro_rules! assert_up_down_counter {
    ($($name:ident).+, $ty: expr, $value: expr, $($attr_key:literal = $attr_value:expr),+) => {
        let attributes = vec![$(opentelemetry::KeyValue::new($attr_key, $attr_value)),+];
        crate::metrics::collect_metrics().assert(stringify!($($name).+), crate::metrics::test_utils::MetricType::UpDownCounter, $value, &attributes);
    };

    ($($name:ident).+, $value: expr, $($($attr_key:ident).+ = $attr_value:expr),+) => {
        let attributes = vec![$(opentelemetry::KeyValue::new(stringify!($($attr_key).+), $attr_value)),+];
        crate::metrics::collect_metrics().assert(stringify!($($name).+), crate::metrics::test_utils::MetricType::UpDownCounter, $value, &attributes);
    };

    ($name:literal, $value: expr, $($attr_key:literal = $attr_value:expr),+) => {
        let attributes = vec![$(opentelemetry::KeyValue::new($attr_key, $attr_value)),+];
        crate::metrics::collect_metrics().assert($name, crate::metrics::test_utils::MetricType::UpDownCounter, $value, &attributes);
    };

    ($name:literal, $value: expr, $($($attr_key:ident).+ = $attr_value:expr),+) => {
        let attributes = vec![$(opentelemetry::KeyValue::new(stringify!($($attr_key).+), $attr_value)),+];
        crate::metrics::collect_metrics().assert($name, crate::metrics::test_utils::MetricType::UpDownCounter, $value, &attributes);
    };

    ($name:literal, $value: expr) => {
        crate::metrics::collect_metrics().assert($name, crate::metrics::test_utils::MetricType::UpDownCounter, $value, &[]);
    };
}

#[cfg(test)]
macro_rules! assert_gauge {
    ($($name:ident).+, $ty: expr, $value: expr, $($attr_key:literal = $attr_value:expr),+) => {
        let attributes = vec![$(opentelemetry::KeyValue::new($attr_key, $attr_value)),+];
        crate::metrics::collect_metrics().assert(stringify!($($name).+), crate::metrics::test_utils::MetricType::Gauge, $value, &attributes);
    };

    ($($name:ident).+, $value: expr, $($($attr_key:ident).+ = $attr_value:expr),+) => {
        let attributes = vec![$(opentelemetry::KeyValue::new(stringify!($($attr_key).+), $attr_value)),+];
        crate::metrics::collect_metrics().assert(stringify!($($name).+), crate::metrics::test_utils::MetricType::Gauge, $value, &attributes);
    };

    ($name:literal, $value: expr, $($attr_key:literal = $attr_value:expr),+) => {
        let attributes = vec![$(opentelemetry::KeyValue::new($attr_key, $attr_value)),+];
        crate::metrics::collect_metrics().assert($name, crate::metrics::test_utils::MetricType::Gauge, $value, &attributes);
    };

    ($name:literal, $value: expr, $($($attr_key:ident).+ = $attr_value:expr),+) => {
        let attributes = vec![$(opentelemetry::KeyValue::new(stringify!($($attr_key).+), $attr_value)),+];
        crate::metrics::collect_metrics().assert($name, crate::metrics::test_utils::MetricType::Gauge, $value, &attributes);
    };

    ($name:literal, $value: expr) => {
        crate::metrics::collect_metrics().assert($name, crate::metrics::test_utils::MetricType::Gauge, $value, &[]);
    };
}

#[cfg(test)]
macro_rules! assert_histogram {
    ($($name:ident).+, $ty: expr, $value: expr, $($attr_key:literal = $attr_value:expr),+) => {
        let attributes = vec![$(opentelemetry::KeyValue::new($attr_key, $attr_value)),+];
        crate::metrics::collect_metrics().assert(stringify!($($name).+), crate::metrics::test_utils::MetricType::Histogram, $value, &attributes);
    };

    ($($name:ident).+, $value: expr, $($($attr_key:ident).+ = $attr_value:expr),+) => {
        let attributes = vec![$(opentelemetry::KeyValue::new(stringify!($($attr_key).+), $attr_value)),+];
        crate::metrics::collect_metrics().assert(stringify!($($name).+), crate::metrics::test_utils::MetricType::Histogram, $value, &attributes);
    };

    ($name:literal, $value: expr, $($attr_key:literal = $attr_value:expr),+) => {
        let attributes = vec![$(opentelemetry::KeyValue::new($attr_key, $attr_value)),+];
        crate::metrics::collect_metrics().assert($name, crate::metrics::test_utils::MetricType::Histogram, $value, &attributes);
    };

    ($name:literal, $value: expr, $($($attr_key:ident).+ = $attr_value:expr),+) => {
        let attributes = vec![$(opentelemetry::KeyValue::new(stringify!($($attr_key).+), $attr_value)),+];
        crate::metrics::collect_metrics().assert($name, crate::metrics::test_utils::MetricType::Histogram, $value, &attributes);
    };

    ($name:literal, $value: expr) => {
        crate::metrics::collect_metrics().assert($name, $value, &[]);
    };
}

#[cfg(test)]
pub(crate) type MetricFuture<T> = Pin<Box<dyn Future<Output = <T as Future>::Output> + Send>>;

#[cfg(test)]
pub(crate) trait FutureMetricsExt<T> {
    fn with_metrics(
        self,
    ) -> tokio::task::futures::TaskLocalFuture<
        OnceLock<(AggregateMeterProvider, test_utils::ClonableManualReader)>,
        MetricFuture<Self>,
    >
    where
        Self: Sized + Future + Send + 'static,
        <Self as Future>::Output: Send + 'static,
    {
        test_utils::AGGREGATE_METER_PROVIDER_ASYNC.scope(
            Default::default(),
            async move {
                let result = self.await;
                let _ = tokio::task::spawn_blocking(|| {
                    meter_provider().shutdown();
                })
                .await;
                result
            }
            .boxed(),
        )
    }
}

#[cfg(test)]
impl<T> FutureMetricsExt<T> for T where T: Future {}

#[cfg(test)]
mod test {
    use opentelemetry::metrics::MeterProvider;
    use opentelemetry::KeyValue;

    use crate::metrics::aggregation::MeterProviderType;
    use crate::metrics::meter_provider;
    use crate::metrics::FutureMetricsExt;

    #[test]
    fn test_gauge() {
        meter_provider()
            .meter("test")
            .u64_observable_gauge("test")
            .with_callback(|m| m.observe(5, &[]))
            .init();
        assert_gauge!("test", 5);
    }

    #[test]
    fn test_no_attributes() {
        u64_counter!("test", "test description", 1);
        assert_counter!("test", 1);
    }

    #[test]
    fn test_dynamic_attributes() {
        let attributes = vec![KeyValue::new("attr", "val")];
        u64_counter!("test", "test description", 1, attributes);
        assert_counter!("test", 1, "attr" = "val");
    }

    #[test]
    fn test_multiple_calls() {
        fn my_method(val: &'static str) {
            u64_counter!("test", "test description", 1, "attr" = val);
        }

        my_method("jill");
        my_method("jill");
        my_method("bob");
        assert_counter!("test", 2, "attr" = "jill");
        assert_counter!("test", 1, "attr" = "bob");
    }

    #[test]
    fn test_non_async() {
        // Each test is run in a separate thread, metrics are stored in a thread local.
        u64_counter!("test", "test description", 1, "attr" = "val");
        assert_counter!("test", 1, "attr" = "val");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_async_multi() {
        // Multi-threaded runtime needs to use a tokio task local to avoid tests interfering with each other
        async {
            u64_counter!("test", "test description", 1, "attr" = "val");
            assert_counter!("test", 1, "attr" = "val");
        }
        .with_metrics()
        .await;
    }

    #[tokio::test]
    async fn test_async_single() {
        async {
            // It's a single threaded tokio runtime, so we can still use a thread local
            u64_counter!("test", "test description", 1, "attr" = "val");
            assert_counter!("test", 1, "attr" = "val");
        }
        .with_metrics()
        .await;
    }

    #[tokio::test]
    async fn test_u64_counter() {
        async {
            u64_counter!("test", "test description", 1, attr = "val");
            u64_counter!("test", "test description", 1, attr.test = "val");
            u64_counter!("test", "test description", 1, attr.test_underscore = "val");
            u64_counter!(
                test.dot,
                "test description",
                1,
                "attr.test_underscore" = "val"
            );
            u64_counter!(
                test.dot,
                "test description",
                1,
                attr.test_underscore = "val"
            );
            assert_counter!("test", 1, "attr" = "val");
            assert_counter!("test", 1, "attr.test" = "val");
            assert_counter!("test", 1, attr.test_underscore = "val");
            assert_counter!(test.dot, 2, attr.test_underscore = "val");
            assert_counter!(test.dot, 2, "attr.test_underscore" = "val");
        }
        .with_metrics()
        .await;
    }

    #[tokio::test]
    async fn test_f64_counter() {
        async {
            f64_counter!("test", "test description", 1.5, "attr" = "val");
            assert_counter!("test", 1.5, "attr" = "val");
        }
        .with_metrics()
        .await;
    }

    #[tokio::test]
    async fn test_i64_up_down_counter() {
        async {
            i64_up_down_counter!("test", "test description", 1, "attr" = "val");
            assert_up_down_counter!("test", 1, "attr" = "val");
        }
        .with_metrics()
        .await;
    }

    #[tokio::test]
    async fn test_f64_up_down_counter() {
        async {
            f64_up_down_counter!("test", "test description", 1.5, "attr" = "val");
            assert_up_down_counter!("test", 1.5, "attr" = "val");
        }
        .with_metrics()
        .await;
    }

    #[tokio::test]
    async fn test_u64_histogram() {
        async {
            u64_histogram!("test", "test description", 1, "attr" = "val");
            assert_histogram!("test", 1, "attr" = "val");
        }
        .with_metrics()
        .await;
    }

    #[tokio::test]
    async fn test_i64_histogram() {
        async {
            i64_histogram!("test", "test description", 1, "attr" = "val");
            assert_histogram!("test", 1, "attr" = "val");
        }
        .with_metrics()
        .await;
    }

    #[tokio::test]
    async fn test_f64_histogram() {
        async {
            f64_histogram!("test", "test description", 1.0, "attr" = "val");
            assert_histogram!("test", 1, "attr" = "val");
        }
        .with_metrics()
        .await;
    }

    #[tokio::test]
    #[should_panic]
    async fn test_type_histogram() {
        async {
            f64_histogram!("test", "test description", 1.0, "attr" = "val");
            assert_counter!("test", 1, "attr" = "val");
        }
        .with_metrics()
        .await;
    }

    #[tokio::test]
    #[should_panic]
    async fn test_type_counter() {
        async {
            f64_counter!("test", "test description", 1.0, "attr" = "val");
            assert_histogram!("test", 1, "attr" = "val");
        }
        .with_metrics()
        .await;
    }

    #[tokio::test]
    #[should_panic]
    async fn test_type_up_down_counter() {
        async {
            f64_up_down_counter!("test", "test description", 1.0, "attr" = "val");
            assert_histogram!("test", 1, "attr" = "val");
        }
        .with_metrics()
        .await;
    }

    #[tokio::test]
    #[should_panic]
    async fn test_type_gauge() {
        async {
            meter_provider()
                .meter("test")
                .u64_observable_gauge("test")
                .with_callback(|m| m.observe(5, &[]))
                .init();
            assert_histogram!("test", 1, "attr" = "val");
        }
        .with_metrics()
        .await;
    }

    #[test]
    fn test_callsite_caching() {
        // Creating instruments may be slow due to multiple levels of locking that needs to happen through the various metrics layers.
        // Callsite caching is implemented to prevent this happening on every call.
        // See the metric macro above to see more information.
        super::CACHE_CALLSITE.with(|cell| cell.store(true, std::sync::atomic::Ordering::SeqCst));
        fn test() {
            // This is a single callsite so should only have one metric
            u64_counter!("test", "test description", 1, "attr" = "val");
        }

        // Callsite hasn't been used yet, so there should be no metrics
        assert_eq!(meter_provider().registered_instruments(), 0);

        // Call the metrics, it will be registered
        test();
        assert_counter!("test", 1, "attr" = "val");
        assert_eq!(meter_provider().registered_instruments(), 1);

        // Call the metrics again, but the second call will not register a new metric because it will have be retrieved from the static
        test();
        assert_counter!("test", 2, "attr" = "val");
        assert_eq!(meter_provider().registered_instruments(), 1);

        // Force invalidation of instruments
        meter_provider().set(MeterProviderType::PublicPrometheus, None);
        assert_eq!(meter_provider().registered_instruments(), 0);

        // Slow path
        test();
        assert_eq!(meter_provider().registered_instruments(), 1);

        // Fast path
        test();
        assert_eq!(meter_provider().registered_instruments(), 1);
    }
}
