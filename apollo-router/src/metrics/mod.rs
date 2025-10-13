//! APIs for integrating with the router's metrics.
//!
//! The macros contained here are a replacement for the telemetry crate's `MetricsLayer`. We will
//! eventually convert all metrics to use these macros and deprecate the `MetricsLayer`.
//! The reason for this is that the `MetricsLayer` has:
//!
//! * No support for dynamic attributes
//! * No support dynamic metrics.
//! * Imperfect mapping to metrics API that can only be checked at runtime.
//!
//! New metrics should be added using these macros.
//!
//! Prefer using `_with_unit` types for all new macros. Units should conform to the
//! [OpenTelemetry semantic conventions](https://opentelemetry.io/docs/specs/semconv/general/metrics/#units),
//! some of which has been copied here for reference:
//! * Instruments that measure a count of something should only use annotations with curly braces to
//!   give additional meaning. For example, use `{packet}`, `{error}`, `{fault}`, etc., not `packet`,
//!   `error`, `fault`, etc.
//! * Other instrument units should be specified using the UCUM case sensitive (c/s) variant. For
//!   example, Cel for the unit with full name degree Celsius.
//! * When instruments are measuring durations, seconds (i.e. s) should be used.
//! * Instruments should use non-prefixed units (i.e. By instead of MiBy) unless there is good
//!   technical reason to not do so.
//!
//! NB: we have not yet modified the existing metrics because some metric exporters (notably
//! Prometheus) include the unit in the metric name, and changing the metric name will be a breaking
//! change for customers.
//!
//! ## Compatibility
//! This module uses types from the [opentelemetry] crates. Since OpenTelemetry for Rust is not yet
//! API-stable, we may update it in a minor version, which may require code changes to plugins.
//!
//!
//! # Examples
//! ```ignore
//! // Count a thing:
//! u64_counter!(
//!     "apollo.router.operations.frobbles",
//!     "The amount of frobbles we've operated on",
//!     1
//! );
//! // Count a thing with attributes:
//! u64_counter!(
//!     "apollo.router.operations.frobbles",
//!     "The amount of frobbles we've operated on",
//!     1,
//!     frobbles.color = "blue"
//! );
//! // Count a thing with dynamic attributes:
//! let attributes = vec![];
//! if (frobbled) {
//!     attributes.push(opentelemetry::KeyValue::new("frobbles.color".to_string(), "blue".into()));
//! }
//! u64_counter!(
//!     "apollo.router.operations.frobbles",
//!     "The amount of frobbles we've operated on",
//!     1,
//!     attributes
//! );
//! // Measure a thing with units:
//! f64_histogram_with_unit!(
//!     "apollo.router.operation.frobbles.time",
//!     "Duration to operate on frobbles",
//!     "s",
//!     1.0,
//!     frobbles.color = "red"
//! );
//! ```

#[cfg(test)]
use std::future::Future;
use std::marker::PhantomData;
#[cfg(test)]
use std::pin::Pin;
use std::sync::OnceLock;

#[cfg(test)]
use futures::FutureExt;

use crate::metrics::aggregation::AggregateMeterProvider;

pub(crate) mod aggregation;
pub(crate) mod filter;

/// A RAII guard for an up-down counter that automatically decrements on drop.
///
/// This guard implements the RAII (Resource Acquisition Is Initialization) pattern
/// to ensure that up-down counters are properly decremented when the guard goes out
/// of scope. This is particularly useful for tracking active operations, connections,
/// or other resources where the counter should reflect the current state.
///
/// It is essential that the same instrument is used for the decrement as was used for an increment
/// otherwise drift can occur.
#[derive(Debug)]
#[doc(hidden)]
#[must_use = "without holding the guard updown counters will immediately zero out"]
pub struct UpDownCounterGuard<T>
where
    T: std::ops::Neg<Output = T> + Copy,
{
    counter: opentelemetry::metrics::UpDownCounter<T>,
    value: T,
    attributes: Vec<opentelemetry::KeyValue>,
}

impl<T> UpDownCounterGuard<T>
where
    T: std::ops::Neg<Output = T> + Copy,
{
    /// Creates a new guard.
    #[doc(hidden)]
    pub fn new(
        counter: std::sync::Arc<opentelemetry::metrics::UpDownCounter<T>>,
        value: T,
        attributes: &[opentelemetry::KeyValue],
    ) -> Self {
        // Note that increment will already have been called via the macro, we only deal with drops
        // It is essential that we take the counter out of the arc otherwise it will break reload.
        // Instruments rely on weak references to allow callsite invalidation.
        // Therefore, if we hold onto the Arc then callsite invalidation won't work.
        Self {
            counter: (*counter).clone(),
            value,
            attributes: attributes.to_vec(),
        }
    }
}

impl<T> Drop for UpDownCounterGuard<T>
where
    T: std::ops::Neg<Output = T> + Copy,
{
    /// Decrements the counter when the guard is dropped.
    ///
    /// This automatically subtracts the original value from the counter,
    /// ensuring the metric accurately reflects the current state.
    fn drop(&mut self) {
        self.counter.add(-self.value, &self.attributes);
    }
}

/// Noop guard won't do anything. it serves to unify the logic UpDownCounterGuard
#[doc(hidden)]
pub struct NoopGuard<I, T> {
    _phantom: PhantomData<(I, T)>,
}
impl<I, T> NoopGuard<I, T> {
    /// Noop guard won't do anything. it serves to unify the logic UpDownCounterGuard
    #[doc(hidden)]
    pub fn new(_instrument: I, _value: T, _attributes: &[opentelemetry::KeyValue]) -> Self {
        NoopGuard {
            _phantom: Default::default(),
        }
    }
}

#[cfg(test)]
pub(crate) mod test_utils {
    use std::cmp::Ordering;
    use std::collections::BTreeMap;
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
    use opentelemetry::StringValue;
    use opentelemetry::Value;
    use opentelemetry_sdk::metrics::Aggregation;
    use opentelemetry_sdk::metrics::AttributeSet;
    use opentelemetry_sdk::metrics::InstrumentKind;
    use opentelemetry_sdk::metrics::ManualReader;
    use opentelemetry_sdk::metrics::MeterProviderBuilder;
    use opentelemetry_sdk::metrics::Pipeline;
    use opentelemetry_sdk::metrics::data::DataPoint;
    use opentelemetry_sdk::metrics::data::Gauge;
    use opentelemetry_sdk::metrics::data::Histogram;
    use opentelemetry_sdk::metrics::data::HistogramDataPoint;
    use opentelemetry_sdk::metrics::data::Metric;
    use opentelemetry_sdk::metrics::data::ResourceMetrics;
    use opentelemetry_sdk::metrics::data::Sum;
    use opentelemetry_sdk::metrics::data::Temporality;
    use opentelemetry_sdk::metrics::reader::AggregationSelector;
    use opentelemetry_sdk::metrics::reader::MetricReader;
    use opentelemetry_sdk::metrics::reader::TemporalitySelector;
    use serde::Serialize;
    use tokio::task_local;

    use crate::metrics::aggregation::AggregateMeterProvider;
    use crate::metrics::aggregation::MeterProviderType;
    use crate::metrics::filter::FilterMeterProvider;
    task_local! {
        pub(crate) static AGGREGATE_METER_PROVIDER_ASYNC: OnceLock<(AggregateMeterProvider, ClonableManualReader)>;
    }
    thread_local! {
        pub(crate) static AGGREGATE_METER_PROVIDER: OnceLock<(AggregateMeterProvider, ClonableManualReader)> = const { OnceLock::new() };
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
                FilterMeterProvider::all(
                    MeterProviderBuilder::default()
                        .with_reader(reader.clone())
                        .build(),
                ),
            );

            (meter_provider, reader)
        }
    }
    pub(crate) fn meter_provider_and_readers() -> (AggregateMeterProvider, ClonableManualReader) {
        if tokio::runtime::Handle::try_current().is_ok() {
            AGGREGATE_METER_PROVIDER_ASYNC
                .try_with(|cell| cell.get_or_init(create_test_meter_provider).clone())
                // We need to silently fail here.
                // Otherwise we fail every multi-threaded test that touches metrics
                .unwrap_or_default()
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
        reader
            .collect(&mut metrics.resource_metrics)
            .expect("Failed to collect metrics. Did you forget to use `async{}.with_metrics()`? See dev-docs/metrics.md");
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
            // Useful for histogram to check the count and not the sum
            count: bool,
            attributes: &[KeyValue],
        ) -> bool {
            let attributes = AttributeSet::from(attributes);
            if let Some(value) = value.to_u64()
                && self.metric_matches(name, &ty, value, count, &attributes)
            {
                return true;
            }

            if let Some(value) = value.to_i64()
                && self.metric_matches(name, &ty, value, count, &attributes)
            {
                return true;
            }

            if let Some(value) = value.to_f64()
                && self.metric_matches(name, &ty, value, count, &attributes)
            {
                return true;
            }

            false
        }

        pub(crate) fn metric_matches<T: Debug + PartialEq + Display + ToPrimitive + 'static>(
            &self,
            name: &str,
            ty: &MetricType,
            value: T,
            count: bool,
            attributes: &AttributeSet,
        ) -> bool {
            if let Some(metric) = self.find(name) {
                // Try to downcast the metric to each type of aggregation and assert that the value is correct.
                if let Some(gauge) = metric.data.as_any().downcast_ref::<Gauge<T>>() {
                    // Find the datapoint with the correct attributes.
                    if matches!(ty, MetricType::Gauge) {
                        return gauge.data_points.iter().any(|datapoint| {
                            datapoint.value == value
                                && Self::equal_attributes(attributes, &datapoint.attributes)
                        });
                    }
                } else if let Some(sum) = metric.data.as_any().downcast_ref::<Sum<T>>() {
                    // Note that we can't actually tell if the sum is monotonic or not, so we just check if it's a sum.
                    if matches!(ty, MetricType::Counter | MetricType::UpDownCounter) {
                        return sum.data_points.iter().any(|datapoint| {
                            datapoint.value == value
                                && Self::equal_attributes(attributes, &datapoint.attributes)
                        });
                    }
                } else if let Some(histogram) = metric.data.as_any().downcast_ref::<Histogram<T>>()
                    && matches!(ty, MetricType::Histogram)
                {
                    if count {
                        return histogram.data_points.iter().any(|datapoint| {
                            datapoint.count == value.to_u64().unwrap()
                                && Self::equal_attributes(attributes, &datapoint.attributes)
                        });
                    } else {
                        return histogram.data_points.iter().any(|datapoint| {
                            datapoint.sum == value
                                && Self::equal_attributes(attributes, &datapoint.attributes)
                        });
                    }
                }
            }
            false
        }

        pub(crate) fn metric_exists<T: Debug + PartialEq + Display + ToPrimitive + 'static>(
            &self,
            name: &str,
            ty: MetricType,
            attributes: &[KeyValue],
        ) -> bool {
            let attributes = AttributeSet::from(attributes);
            if let Some(metric) = self.find(name) {
                // Try to downcast the metric to each type of aggregation and assert that the value is correct.
                if let Some(gauge) = metric.data.as_any().downcast_ref::<Gauge<T>>() {
                    // Find the datapoint with the correct attributes.
                    if matches!(ty, MetricType::Gauge) {
                        return gauge.data_points.iter().any(|datapoint| {
                            Self::equal_attributes(&attributes, &datapoint.attributes)
                        });
                    }
                } else if let Some(sum) = metric.data.as_any().downcast_ref::<Sum<T>>() {
                    // Note that we can't actually tell if the sum is monotonic or not, so we just check if it's a sum.
                    if matches!(ty, MetricType::Counter | MetricType::UpDownCounter) {
                        return sum.data_points.iter().any(|datapoint| {
                            Self::equal_attributes(&attributes, &datapoint.attributes)
                        });
                    }
                } else if let Some(histogram) = metric.data.as_any().downcast_ref::<Histogram<T>>()
                    && matches!(ty, MetricType::Histogram)
                {
                    return histogram.data_points.iter().any(|datapoint| {
                        Self::equal_attributes(&attributes, &datapoint.attributes)
                    });
                }
            }
            false
        }

        #[allow(dead_code)]
        pub(crate) fn all(self) -> Vec<SerdeMetric> {
            self.resource_metrics
                .scope_metrics
                .into_iter()
                .flat_map(|scope_metrics| {
                    scope_metrics.metrics.into_iter().map(|metric| {
                        let serde_metric: SerdeMetric = metric.into();
                        serde_metric
                    })
                })
                .sorted()
                .collect()
        }

        #[allow(dead_code)]
        pub(crate) fn non_zero(self) -> Vec<SerdeMetric> {
            self.all()
                .into_iter()
                .filter(|m| {
                    m.data.datapoints.iter().any(|d| {
                        d.value
                            .as_ref()
                            .map(|v| v.as_f64().unwrap_or_default() > 0.0)
                            .unwrap_or_default()
                            || d.sum
                                .as_ref()
                                .map(|v| v.as_f64().unwrap_or_default() > 0.0)
                                .unwrap_or_default()
                    })
                })
                .collect()
        }

        fn equal_attributes(expected: &AttributeSet, actual: &[KeyValue]) -> bool {
            // If lengths are different, we can short circuit. This also accounts for a bug where
            // an empty attributes list would always be considered "equal" due to zip capping at
            // the shortest iter's length
            if expected.iter().count() != actual.len() {
                return false;
            }
            // This works because the attributes are always sorted
            expected.iter().zip(actual.iter()).all(|((k, v), kv)| {
                kv.key == *k
                    && (kv.value == *v || kv.value == Value::String(StringValue::from("<any>")))
            })
        }
    }

    #[derive(Serialize, Eq, PartialEq)]
    pub(crate) struct SerdeMetric {
        pub(crate) name: String,
        #[serde(skip_serializing_if = "String::is_empty")]
        pub(crate) description: String,
        #[serde(skip_serializing_if = "String::is_empty")]
        pub(crate) unit: String,
        pub(crate) data: SerdeMetricData,
    }

    impl Ord for SerdeMetric {
        fn cmp(&self, other: &Self) -> Ordering {
            self.name.cmp(&other.name)
        }
    }

    impl PartialOrd for SerdeMetric {
        fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
            Some(self.cmp(other))
        }
    }

    #[derive(Clone, Serialize, Eq, PartialEq, Default)]
    pub(crate) struct SerdeMetricData {
        pub(crate) datapoints: Vec<SerdeMetricDataPoint>,
    }

    #[derive(Clone, Serialize, Eq, PartialEq)]
    pub(crate) struct SerdeMetricDataPoint {
        #[serde(skip_serializing_if = "Option::is_none")]
        pub(crate) value: Option<serde_json::Value>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub(crate) sum: Option<serde_json::Value>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub(crate) count: Option<u64>,
        pub(crate) attributes: BTreeMap<String, serde_json::Value>,
    }

    impl Ord for SerdeMetricDataPoint {
        fn cmp(&self, other: &Self) -> Ordering {
            //Horribly inefficient, but it's just for testing
            let self_string = serde_json::to_string(&self.attributes).expect("serde failed");
            let other_string = serde_json::to_string(&other.attributes).expect("serde failed");
            self_string.cmp(&other_string)
        }
    }

    impl PartialOrd for SerdeMetricDataPoint {
        fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
            Some(self.cmp(other))
        }
    }

    impl SerdeMetricData {
        fn extract_datapoints<T: Into<serde_json::Value> + Clone + 'static>(
            metric_data: &mut SerdeMetricData,
            value: &dyn opentelemetry_sdk::metrics::data::Aggregation,
        ) {
            if let Some(gauge) = value.as_any().downcast_ref::<Gauge<T>>() {
                gauge.data_points.iter().for_each(|datapoint| {
                    metric_data.datapoints.push(datapoint.into());
                });
            }
            if let Some(sum) = value.as_any().downcast_ref::<Sum<T>>() {
                sum.data_points.iter().for_each(|datapoint| {
                    metric_data.datapoints.push(datapoint.into());
                });
            }
            if let Some(histogram) = value.as_any().downcast_ref::<Histogram<T>>() {
                histogram.data_points.iter().for_each(|datapoint| {
                    metric_data.datapoints.push(datapoint.into());
                });
            }
        }
    }

    impl From<Metric> for SerdeMetric {
        fn from(value: Metric) -> Self {
            let mut serde_metric = SerdeMetric {
                name: value.name.into_owned(),
                description: value.description.into_owned(),
                unit: value.unit.to_string(),
                data: value.data.into(),
            };
            // Sort the datapoints so that we can compare them
            serde_metric.data.datapoints.sort();

            // Redact duration metrics;
            if serde_metric.name.ends_with(".duration") {
                serde_metric
                    .data
                    .datapoints
                    .iter_mut()
                    .for_each(|datapoint| {
                        if let Some(sum) = &datapoint.sum
                            && sum.as_f64().unwrap_or_default() > 0.0
                        {
                            datapoint.sum = Some(0.1.into());
                        }
                    });
            }
            serde_metric
        }
    }

    impl<T> From<&DataPoint<T>> for SerdeMetricDataPoint
    where
        T: Into<serde_json::Value> + Clone,
    {
        fn from(value: &DataPoint<T>) -> Self {
            SerdeMetricDataPoint {
                value: Some(value.value.clone().into()),
                sum: None,
                count: None,
                attributes: value
                    .attributes
                    .iter()
                    .map(|kv| (kv.key.to_string(), Self::convert(&kv.value)))
                    .collect(),
            }
        }
    }

    impl SerdeMetricDataPoint {
        pub(crate) fn convert(v: &Value) -> serde_json::Value {
            match v.clone() {
                Value::Bool(v) => v.into(),
                Value::I64(v) => v.into(),
                Value::F64(v) => v.into(),
                Value::String(v) => v.to_string().into(),
                Value::Array(v) => match v {
                    Array::Bool(v) => v.into(),
                    Array::I64(v) => v.into(),
                    Array::F64(v) => v.into(),
                    Array::String(v) => v.iter().map(|v| v.to_string()).collect::<Vec<_>>().into(),
                },
            }
        }
    }

    impl<T> From<&HistogramDataPoint<T>> for SerdeMetricDataPoint
    where
        T: Into<serde_json::Value> + Clone,
    {
        fn from(value: &HistogramDataPoint<T>) -> Self {
            SerdeMetricDataPoint {
                sum: Some(value.sum.clone().into()),
                value: None,
                count: Some(value.count),
                attributes: value
                    .attributes
                    .iter()
                    .map(|kv| (kv.key.to_string(), Self::convert(&kv.value)))
                    .collect(),
            }
        }
    }

    impl From<Box<dyn opentelemetry_sdk::metrics::data::Aggregation>> for SerdeMetricData {
        fn from(value: Box<dyn opentelemetry_sdk::metrics::data::Aggregation>) -> Self {
            let mut metric_data = SerdeMetricData::default();
            Self::extract_datapoints::<u64>(&mut metric_data, value.as_ref());
            Self::extract_datapoints::<f64>(&mut metric_data, value.as_ref());
            Self::extract_datapoints::<i64>(&mut metric_data, value.as_ref());
            metric_data
        }
    }

    pub(crate) enum MetricType {
        Counter,
        UpDownCounter,
        Histogram,
        Gauge,
    }
}

/// Returns a MeterProvider, as a concrete type so we can use our own extensions.
///
/// During tests this is a task local so that we can test metrics without having to worry about other tests interfering.
#[cfg(test)]
pub(crate) fn meter_provider_internal() -> AggregateMeterProvider {
    test_utils::meter_provider_and_readers().0
}

#[cfg(test)]
pub(crate) use test_utils::collect_metrics;

#[cfg(not(test))]
static AGGREGATE_METER_PROVIDER: OnceLock<AggregateMeterProvider> = OnceLock::new();

/// Returns the currently configured global MeterProvider, as a concrete type
/// so we can use our own extensions.
#[cfg(not(test))]
pub(crate) fn meter_provider_internal() -> AggregateMeterProvider {
    AGGREGATE_METER_PROVIDER
        .get_or_init(Default::default)
        .clone()
}

/// Returns the currently configured global [`MeterProvider`].
///
/// See the [module-level documentation] for important details on the semver-compatibility guarantees of this API.
///
/// [`MeterProvider`]: opentelemetry::metrics::MeterProvider
/// [module-level documentation]: crate::metrics
pub fn meter_provider() -> impl opentelemetry::metrics::MeterProvider {
    meter_provider_internal()
}

/// Parse key/value attributes into `opentelemetry::KeyValue` structs. Should only be used within
/// this module, as a helper for the various metric macros (ie `u64_counter!`).
macro_rules! parse_attributes {
    ($($attr_key:literal = $attr_value:expr),+) => {[$(opentelemetry::KeyValue::new($attr_key, $attr_value)),+]};
    ($($($attr_key:ident).+ = $attr_value:expr),+) => {[$(opentelemetry::KeyValue::new(stringify!($($attr_key).+), $attr_value)),+]};
    ($attrs:expr) => {$attrs};
}

/// Get or create a `u64` monotonic counter metric and add a value to it.
/// The metric must include a description.
///
/// See the [module-level documentation](crate::metrics) for examples and details on the reasoning
/// behind this API.
#[allow(unused_macros)]
#[deprecated(since = "TBD", note = "use `u64_counter_with_unit` instead")]
macro_rules! u64_counter {
    ($($name:ident).+, $description:literal, $value: expr, $($attrs:tt)*) => {
        metric!(u64, counter, crate::metrics::NoopGuard, add, stringify!($($name).+), $description, $value, parse_attributes!($($attrs)*));
    };

    ($name:literal, $description:literal, $value: expr, $($attrs:tt)*) => {
        metric!(u64, counter, crate::metrics::NoopGuard, add, $name, $description, $value, parse_attributes!($($attrs)*));
    };

    ($name:literal, $description:literal, $value: expr) => {
        metric!(u64, counter, crate::metrics::NoopGuard, add, $name, $description, $value, []);
    }
}

/// Get or create a u64 monotonic counter metric and add a value to it.
/// The metric must include a description and a unit.
///
/// The units should conform to the [OpenTelemetry semantic conventions](https://opentelemetry.io/docs/specs/semconv/general/metrics/#units).
///
/// See the [module-level documentation](crate::metrics) for examples and details on the reasoning
/// behind this API.
#[allow(unused_macros)]
macro_rules! u64_counter_with_unit {
    ($($name:ident).+, $description:literal, $unit:literal, $value: expr, $($attrs:tt)*) => {
        metric!(u64, counter, crate::metrics::NoopGuard, add, stringify!($($name).+), $description, $unit, $value, parse_attributes!($($attrs)*));
    };

    ($name:literal, $description:literal, $unit:literal, $value: expr, $($attrs:tt)*) => {
        metric!(u64, counter, crate::metrics::NoopGuard, add, $name, $description, $unit, $value, parse_attributes!($($attrs)*));
    };

    ($name:literal, $description:literal, $unit:literal, $value: expr) => {
        metric!(u64, counter, crate::metrics::NoopGuard, add, $name, $description, $unit, $value, []);
    }
}

/// Get or create a f64 monotonic counter metric and add a value to it.
/// The metric must include a description.
///
/// See the [module-level documentation](crate::metrics) for examples and details on the reasoning
/// behind this API.
#[allow(unused_macros)]
#[deprecated(since = "TBD", note = "use `f64_counter_with_unit` instead")]
macro_rules! f64_counter {
    ($($name:ident).+, $description:literal, $value: expr, $($attrs:tt)*) => {
        metric!(f64, counter, crate::metrics::NoopGuard, add, stringify!($($name).+), $description, $value, parse_attributes!($($attrs)*));
    };

    ($name:literal, $description:literal, $value: expr, $($attrs:tt)*) => {
        metric!(f64, counter, crate::metrics::NoopGuard, add, $name, $description, $value, parse_attributes!($($attrs)*));
    };

    ($name:literal, $description:literal, $value: expr) => {
        metric!(f64, counter, crate::metrics::NoopGuard, add, $name, $description, $value, []);
    }
}

/// Get or create an f64 monotonic counter metric and add a value to it.
/// The metric must include a description and a unit.
///
/// The units should conform to the [OpenTelemetry semantic conventions](https://opentelemetry.io/docs/specs/semconv/general/metrics/#units).
///
/// See the [module-level documentation](crate::metrics) for examples and details on the reasoning
/// behind this API.
#[allow(unused_macros)]
macro_rules! f64_counter_with_unit {
    ($($name:ident).+, $description:literal, $unit:literal, $value: expr, $($attrs:tt)*) => {
        metric!(f64, counter, crate::metrics::NoopGuard, add, stringify!($($name).+), $description, $unit, $value, parse_attributes!($($attrs)*));
    };

    ($name:literal, $description:literal, $unit:literal, $value: expr, $($attrs:tt)*) => {
        metric!(f64, counter, crate::metrics::NoopGuard, add, $name, $description, $unit, $value, parse_attributes!($($attrs)*));
    };

    ($name:literal, $description:literal, $unit:literal, $value: expr) => {
        metric!(f64, counter, crate::metrics::NoopGuard, add, $name, $description, $unit, $value, []);
    }
}

/// Creates or retrieves an i64 up-down counter and returns a RAII guard.
///
/// This macro increments the counter immediately and returns an [`I64UpDownCounterGuard`]
/// that automatically decrements the counter when dropped. This ensures accurate tracking
/// of active resources, operations, or connections.
///
/// **Important:** The returned guard must be stored in a variable to keep the counter
/// incremented. If the guard is immediately dropped, the counter will be decremented.
///
/// # Returns
///
/// An [`I64UpDownCounterGuard`] that decrements the counter on drop.
///
/// # Examples
///
/// ```ignore
/// // Counter is incremented to 1
/// let _guard = i64_up_down_counter!(
///     "active_connections",
///     "Number of active connections",
///     1,
///     connection.type = "websocket"
/// );
/// // Counter remains at 1 while _guard is in scope
///
/// // When _guard is dropped, counter is automatically decremented back to 0
/// ```
///
/// See the [module-level documentation](crate::metrics) for more details.
#[allow(unused_macros)]
#[deprecated(since = "TBD", note = "use `i64_up_down_counter_with_unit` instead")]
macro_rules! i64_up_down_counter {
    ($($name:ident).+, $description:literal, $value: expr, $($attrs:tt)*) => {
        metric!(i64, up_down_counter, crate::metrics::UpDownCounterGuard::<i64>, add, stringify!($($name).+), $description, $value, parse_attributes!($($attrs)*))
    };

    ($name:literal, $description:literal, $value: expr, $($attrs:tt)*) => {
        metric!(i64, up_down_counter, crate::metrics::UpDownCounterGuard::<i64>, add, $name, $description, $value, parse_attributes!($($attrs)*))
    };

    ($name:literal, $description:literal, $value: expr) => {
        metric!(i64, up_down_counter, crate::metrics::UpDownCounterGuard::<i64>, add, $name, $description, $value, [])
    };
}

/// Creates or retrieves an i64 up-down counter with a unit and returns a RAII guard.
///
/// This macro increments the counter immediately and returns an [`UpDownCounterGuard<i64>`]
/// that automatically decrements the counter when dropped. This ensures accurate tracking
/// of active resources, operations, or connections.
///
/// The units should conform to the [OpenTelemetry semantic conventions](https://opentelemetry.io/docs/specs/semconv/general/metrics/#units).
///
/// **Important:** The returned guard must be stored in a variable to keep the counter
/// incremented. If the guard is immediately dropped, the counter will be decremented.
///
/// # Returns
///
/// An [`UpDownCounterGuard<i64>`] that decrements the counter on drop.
///
/// # Examples
///
/// ```ignore
/// // Counter is incremented to 1
/// let _active_job = i64_up_down_counter_with_unit!(
///     "compute.active_jobs",
///     "Number of active computation jobs",
///     "{job}",
///     1,
///     job.type = "query_planning"
/// );
/// // Counter remains at 1 while _active_job is in scope
///
/// // When _active_job is dropped, counter is automatically decremented back to 0
/// ```
///
/// See the [module-level documentation](crate::metrics) for more details.
#[allow(unused_macros)]
macro_rules! i64_up_down_counter_with_unit {
    ($($name:ident).+, $description:literal, $unit:literal, $value: expr, $($attrs:tt)*) => {
        metric!(i64, up_down_counter, crate::metrics::UpDownCounterGuard::<i64>, add, stringify!($($name).+), $description, $unit, $value, parse_attributes!($($attrs)*))
    };

    ($name:literal, $description:literal, $unit:literal, $value: expr, $($attrs:tt)*) => {
        metric!(i64, up_down_counter, crate::metrics::UpDownCounterGuard::<i64>, add, $name, $description, $unit, $value, parse_attributes!($($attrs)*))
    };

    ($name:literal, $description:literal, $unit:literal, $value: expr) => {
        metric!(i64, up_down_counter, crate::metrics::UpDownCounterGuard::<i64>, add, $name, $description, $unit, $value, [])
    }
}

/// Creates or retrieves an f64 up-down counter and returns a RAII guard.
///
/// This macro increments the counter immediately and returns an [`UpDownCounterGuard<f64>`]
/// that automatically decrements the counter when dropped. This ensures accurate tracking
/// of active resources, operations, or connections.
///
/// **Important:** The returned guard must be stored in a variable to keep the counter
/// incremented. If the guard is immediately dropped, the counter will be decremented.
///
/// # Returns
///
/// An [`UpDownCounterGuard<f64>`] that decrements the counter on drop.
///
/// # Examples
///
/// ```ignore
/// // Counter is incremented by 1.5
/// let _guard = f64_up_down_counter!(
///     "active_load",
///     "Current system load",
///     1.5,
///     load.type = "cpu"
/// );
/// // Counter remains at 1.5 while _guard is in scope
///
/// // When _guard is dropped, counter is automatically decremented by 1.5
/// ```
///
/// See the [module-level documentation](crate::metrics) for more details.
#[allow(unused_macros)]
#[deprecated(since = "TBD", note = "use `f64_up_down_counter_with_unit` instead")]
macro_rules! f64_up_down_counter {
    ($($name:ident).+, $description:literal, $value: expr, $($attrs:tt)*) => {
        metric!(f64, up_down_counter, crate::metrics::UpDownCounterGuard::<f64>, add, stringify!($($name).+), $description, $value, parse_attributes!($($attrs)*))
    };

    ($name:literal, $description:literal, $value: expr, $($attrs:tt)*) => {
        metric!(f64, up_down_counter, crate::metrics::UpDownCounterGuard::<f64>, add, $name, $description, $value, parse_attributes!($($attrs)*))
    };

    ($name:literal, $description:literal, $value: expr) => {
        metric!(f64, up_down_counter, crate::metrics::UpDownCounterGuard::<f64>, add, $name, $description, $value, [])
    };
}

/// Creates or retrieves an f64 up-down counter with a unit and returns a RAII guard.
///
/// This macro increments the counter immediately and returns an [`UpDownCounterGuard<f64>`]
/// that automatically decrements the counter when dropped. This ensures accurate tracking
/// of active resources, operations, or connections.
///
/// The units should conform to the [OpenTelemetry semantic conventions](https://opentelemetry.io/docs/specs/semconv/general/metrics/#units).
///
/// **Important:** The returned guard must be stored in a variable to keep the counter
/// incremented. If the guard is immediately dropped, the counter will be decremented.
///
/// # Returns
///
/// An [`UpDownCounterGuard<f64>`] that decrements the counter on drop.
///
/// # Examples
///
/// ```ignore
/// // Counter is incremented by 2.5
/// let _memory_usage = f64_up_down_counter_with_unit!(
///     "memory.active_usage",
///     "Active memory usage",
///     "MB",
///     2.5,
///     memory.type = "heap"
/// );
/// // Counter remains at 2.5 while _memory_usage is in scope
///
/// // When _memory_usage is dropped, counter is automatically decremented by 2.5
/// ```
///
/// See the [module-level documentation](crate::metrics) for more details.
#[allow(unused_macros)]
macro_rules! f64_up_down_counter_with_unit {
    ($($name:ident).+, $description:literal, $unit:literal, $value: expr, $($attrs:tt)*) => {
        metric!(f64, up_down_counter, crate::metrics::UpDownCounterGuard::<f64>, add, stringify!($($name).+), $description, $unit, $value, parse_attributes!($($attrs)*))
    };

    ($name:literal, $description:literal, $unit:literal, $value: expr, $($attrs:tt)*) => {
        metric!(f64, up_down_counter, crate::metrics::UpDownCounterGuard::<f64>, add, $name, $description, $unit, $value, parse_attributes!($($attrs)*))
    };

    ($name:literal, $description:literal, $unit:literal, $value: expr) => {
        metric!(f64, up_down_counter, crate::metrics::UpDownCounterGuard::<f64>, add, $name, $description, $unit, $value, [])
    }
}

/// Get or create an f64 histogram metric and add a value to it.
/// The metric must include a description.
///
/// See the [module-level documentation](crate::metrics) for examples and details on the reasoning
/// behind this API.
#[allow(unused_macros)]
#[deprecated(since = "TBD", note = "use `f64_histogram_with_unit` instead")]
macro_rules! f64_histogram {
    ($($name:ident).+, $description:literal, $value: expr, $($attrs:tt)*) => {
        metric!(f64, histogram, crate::metrics::NoopGuard, record, stringify!($($name).+), $description, $value, parse_attributes!($($attrs)*));
    };

    ($name:literal, $description:literal, $value: expr, $($attrs:tt)*) => {
        metric!(f64, histogram, crate::metrics::NoopGuard,record, $name, $description, $value, parse_attributes!($($attrs)*));
    };

    ($name:literal, $description:literal, $value: expr) => {
        metric!(f64, histogram, crate::metrics::NoopGuard,record, $name, $description, $value, []);
    };
}

/// Get or create an f64 histogram metric and add a value to it.
/// The metric must include a description and a unit.
///
/// The units should conform to the [OpenTelemetry semantic conventions](https://opentelemetry.io/docs/specs/semconv/general/metrics/#units).
///
/// See the [module-level documentation](crate::metrics) for examples and details on the reasoning
/// behind this API.
///
/// ## Caveat
///
/// Two metrics with the same name but different descriptions and/or units will be created as
/// _separate_ metrics.
///
/// ```ignore
/// f64_histogram_with_unit!("test", "test description", "s", 1.0, "attr" = "val");
/// assert_histogram_sum!("test", 1, "attr" = "val");
///
/// f64_histogram_with_unit!("test", "test description", "Hz", 1.0, "attr" = "val");
/// assert_histogram_sum!("test", 1, "attr" = "val");
/// ```
#[allow(unused_macros)]
macro_rules! f64_histogram_with_unit {
    ($($name:ident).+, $description:literal, $unit:literal, $value: expr, $($attrs:tt)*) => {
        metric!(f64, histogram, crate::metrics::NoopGuard, record, stringify!($($name).+), $description, $unit, $value, parse_attributes!($($attrs)*));
    };

    ($name:literal, $description:literal, $unit:literal, $value: expr, $($attrs:tt)*) => {
        metric!(f64, histogram, crate::metrics::NoopGuard, record, $name, $description, $unit, $value, parse_attributes!($($attrs)*));
    };

    ($name:literal, $description:literal, $unit:literal, $value: expr) => {
        metric!(f64, histogram, crate::metrics::NoopGuard, record, $name, $description, $unit, $value, []);
    };
}

/// Get or create a u64 histogram metric and add a value to it.
/// The metric must include a description.
///
/// See the [module-level documentation](crate::metrics) for examples and details on the reasoning
/// behind this API.
#[allow(unused_macros)]
#[deprecated(since = "TBD", note = "use `u64_histogram_with_unit` instead")]
macro_rules! u64_histogram {
    ($($name:ident).+, $description:literal, $value: expr, $($attrs:tt)*) => {
        metric!(u64, histogram, crate::metrics::NoopGuard, record, stringify!($($name).+), $description, $value, parse_attributes!($($attrs)*));
    };

    ($name:literal, $description:literal, $value: expr, $($attrs:tt)*) => {
        metric!(u64, histogram, crate::metrics::NoopGuard, record, $name, $description, $value, parse_attributes!($($attrs)*));
    };

    ($name:literal, $description:literal, $value: expr) => {
        metric!(u64, histogram, crate::metrics::NoopGuard, record, $name, $description, $value, []);
    };
}

/// Get or create a u64 histogram metric and add a value to it.
/// The metric must include a description and a unit.
///
/// The units should conform to the [OpenTelemetry semantic conventions](https://opentelemetry.io/docs/specs/semconv/general/metrics/#units).
///
/// See the [module-level documentation](crate::metrics) for examples and details on the reasoning
/// behind this API.
#[allow(unused_macros)]
macro_rules! u64_histogram_with_unit {
    ($($name:ident).+, $description:literal, $unit:literal, $value: expr, $($attrs:tt)*) => {
        metric!(u64, histogram, crate::metrics::NoopGuard, record, stringify!($($name).+), $description, $unit, $value, parse_attributes!($($attrs)*));
    };

    ($name:literal, $description:literal, $unit:literal, $value: expr, $($attrs:tt)*) => {
        metric!(u64, histogram, crate::metrics::NoopGuard, record, $name, $description, $unit, $value, parse_attributes!($($attrs)*));
    };

    ($name:literal, $description:literal, $unit:literal, $value: expr) => {
        metric!(u64, histogram, crate::metrics::NoopGuard, record, $name, $description, $unit, $value, []);
    };
}

thread_local! {
    // This is used exactly once in testing callsite caching.
    #[cfg(test)]
    pub(crate) static CACHE_CALLSITE: std::sync::atomic::AtomicBool = const {std::sync::atomic::AtomicBool::new(false)};
}
macro_rules! metric {
    ($ty:ident, $instrument:ident, $guard: ty, $mutation:ident, $name:expr, $description:literal, $unit:literal, $value:expr, $attrs:expr) => {
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

                let create_instrument_fn = |meter: opentelemetry::metrics::Meter| {
                    let mut builder = meter.[<$ty _ $instrument>]($name);
                    builder = builder.with_description($description);

                    if !$unit.is_empty() {
                        builder = builder.with_unit($unit);
                    }

                    builder.init()
                };

                if cache_callsite {
                    static INSTRUMENT_CACHE: std::sync::OnceLock<parking_lot::Mutex<std::sync::Weak<opentelemetry::metrics::[<$instrument:camel>]<$ty>>>> = std::sync::OnceLock::new();

                    let mut instrument_guard = INSTRUMENT_CACHE
                        .get_or_init(|| {
                            let meter_provider = crate::metrics::meter_provider_internal();
                            let instrument_ref = meter_provider.create_registered_instrument(|p| create_instrument_fn(p.meter("apollo/router")));
                            parking_lot::Mutex::new(std::sync::Arc::downgrade(&instrument_ref))
                        })
                        .lock();
                    let instrument = if let Some(instrument) = instrument_guard.upgrade() {
                        // Fast path, we got the instrument, drop the mutex guard immediately.
                        drop(instrument_guard);
                        instrument
                    } else {
                        // Slow path, we need to obtain the instrument again.
                        let meter_provider = crate::metrics::meter_provider_internal();
                        let instrument_ref = meter_provider.create_registered_instrument(|p| create_instrument_fn(p.meter("apollo/router")));
                        *instrument_guard = std::sync::Arc::downgrade(&instrument_ref);
                        // We've updated the instrument and got a strong reference to it. We can drop the mutex guard now.
                        drop(instrument_guard);
                        instrument_ref
                    };
                    let attrs : &[opentelemetry::KeyValue] = &$attrs;
                    instrument.$mutation($value, attrs);
                    $guard::new(instrument.clone(), $value, attrs)
                }
                else {
                    let meter_provider = crate::metrics::meter_provider();
                    let meter = opentelemetry::metrics::MeterProvider::meter(&meter_provider, "apollo/router");
                    let instrument = create_instrument_fn(meter);
                    let attrs : &[opentelemetry::KeyValue] = &$attrs;
                    instrument.$mutation($value, attrs);
                    $guard::new(std::sync::Arc::new(instrument.clone()), $value, attrs)
                }
            }
        }
    };

    ($ty:ident, $instrument:ident, $guard: ty, $mutation:ident, $name:expr, $description:literal, $value: expr, $attrs: expr) => {
        metric!($ty, $instrument, $guard, $mutation, $name, $description, "", $value, $attrs)
    }
}

#[cfg(test)]
macro_rules! assert_metric {
    ($result:expr, $name:expr, $value:expr, $sum:expr, $count:expr, $attrs:expr) => {
        if !$result {
            let metric = crate::metrics::test_utils::SerdeMetric {
                name: $name.to_string(),
                description: "".to_string(),
                unit: "".to_string(),
                data: crate::metrics::test_utils::SerdeMetricData {
                    datapoints: [crate::metrics::test_utils::SerdeMetricDataPoint {
                        value: $value,
                        sum: $sum,
                        count: $count,
                        attributes: $attrs
                            .iter()
                            .map(|kv: &opentelemetry::KeyValue| {
                                (
                                    kv.key.to_string(),
                                    crate::metrics::test_utils::SerdeMetricDataPoint::convert(
                                        &kv.value,
                                    ),
                                )
                            })
                            .collect::<std::collections::BTreeMap<_, _>>(),
                    }]
                    .to_vec(),
                },
            };
            panic!(
                "metric not found:\n{}\nmetrics present:\n{}",
                serde_yaml::to_string(&metric).unwrap(),
                serde_yaml::to_string(&crate::metrics::collect_metrics().all()).unwrap()
            )
        }
    };
}

#[cfg(test)]
macro_rules! assert_no_metric {
    ($result:expr, $name:expr, $value:expr, $sum:expr, $count:expr, $attrs:expr) => {
        if $result {
            let metric = crate::metrics::test_utils::SerdeMetric {
                name: $name.to_string(),
                description: "".to_string(),
                unit: "".to_string(),
                data: crate::metrics::test_utils::SerdeMetricData {
                    datapoints: [crate::metrics::test_utils::SerdeMetricDataPoint {
                        value: $value,
                        sum: $sum,
                        count: $count,
                        attributes: $attrs
                            .iter()
                            .map(|kv: &opentelemetry::KeyValue| {
                                (
                                    kv.key.to_string(),
                                    crate::metrics::test_utils::SerdeMetricDataPoint::convert(
                                        &kv.value,
                                    ),
                                )
                            })
                            .collect::<std::collections::BTreeMap<_, _>>(),
                    }]
                    .to_vec(),
                },
            };
            panic!(
                "unexpected metric found:\n{}\nmetrics present:\n{}",
                serde_yaml::to_string(&metric).unwrap(),
                serde_yaml::to_string(&crate::metrics::collect_metrics().all()).unwrap()
            )
        }
    };
}

/// Assert the value of a counter metric that has the given name and attributes.
///
/// In asynchronous tests, you must use [`FutureMetricsExt::with_metrics`]. See dev-docs/metrics.md
/// for details: <https://github.com/apollographql/router/blob/4fc63d55104c81c77e6e0a3cca615eac28e39dc3/dev-docs/metrics.md#testing>
#[cfg(test)]
macro_rules! assert_counter {
    ($($name:ident).+, $value: expr, $($attr_key:literal = $attr_value:expr),+) => {
        let name = stringify!($($name).+);
        let attributes = &[$(opentelemetry::KeyValue::new($attr_key, $attr_value)),+];
        let result = crate::metrics::collect_metrics().assert(name, crate::metrics::test_utils::MetricType::Counter, $value, false, attributes);
        assert_metric!(result, name, Some($value.into()), None, None, &attributes);
    };

    ($($name:ident).+, $value: expr, $($($attr_key:ident).+ = $attr_value:expr),+) => {
        let name = stringify!($($name).+);
        let attributes = &[$(opentelemetry::KeyValue::new(stringify!($($attr_key).+), $attr_value)),+];
        let result = crate::metrics::collect_metrics().assert(name, crate::metrics::test_utils::MetricType::Counter, $value, false, attributes);
        assert_metric!(result, name, Some($value.into()), None, None, &attributes);
    };

    ($name:literal, $value: expr, $($attr_key:literal = $attr_value:expr),+) => {
        let attributes = &[$(opentelemetry::KeyValue::new($attr_key, $attr_value)),+];
        let result = crate::metrics::collect_metrics().assert($name, crate::metrics::test_utils::MetricType::Counter, $value, false, attributes);
        assert_metric!(result, $name, Some($value.into()), None, None, &attributes);
    };

    ($name:literal, $value: expr, $($($attr_key:ident).+ = $attr_value:expr),+) => {
        let attributes = &[$(opentelemetry::KeyValue::new(stringify!($($attr_key).+), $attr_value)),+];
        let result = crate::metrics::collect_metrics().assert($name, crate::metrics::test_utils::MetricType::Counter, $value, false, attributes);
        assert_metric!(result, $name, Some($value.into()), None, None, &attributes);
    };

    ($name:literal, $value: expr, $attributes: expr) => {
        let result = crate::metrics::collect_metrics().assert($name, crate::metrics::test_utils::MetricType::Counter, $value, false, $attributes);
        assert_metric!(result, $name, Some($value.into()), None, None, &$attributes);
    };

    ($name:literal, $value: expr) => {
        let result = crate::metrics::collect_metrics().assert($name, crate::metrics::test_utils::MetricType::Counter, $value, false, &[]);
        assert_metric!(result, $name, Some($value.into()), None, None, &[]);
    };
}

/// Assert that a counter metric does not exist with the given name and attributes.
///
/// In asynchronous tests, you must use [`FutureMetricsExt::with_metrics`]. See dev-docs/metrics.md
/// for details: <https://github.com/apollographql/router/blob/4fc63d55104c81c77e6e0a3cca615eac28e39dc3/dev-docs/metrics.md#testing>
#[cfg(test)]
macro_rules! assert_counter_not_exists {

    ($($name:ident).+, $value: ty, $($attr_key:literal = $attr_value:expr),+) => {
        let attributes = &[$(opentelemetry::KeyValue::new($attr_key, $attr_value)),+];
        let result = crate::metrics::collect_metrics().metric_exists::<$value>(stringify!($($name).+), crate::metrics::test_utils::MetricType::Counter, attributes);
        assert_no_metric!(result, $name, None, None, None, attributes);
    };

    ($($name:ident).+, $value: ty, $($($attr_key:ident).+ = $attr_value:expr),+) => {
        let attributes = &[$(opentelemetry::KeyValue::new(stringify!($($attr_key).+), $attr_value)),+];
        let result = crate::metrics::collect_metrics().metric_exists::<$value>(stringify!($($name).+), crate::metrics::test_utils::MetricType::Counter, attributes);
        assert_no_metric!(result, $name, None, None, None, attributes);
    };

    ($name:literal, $value: ty, $($attr_key:literal = $attr_value:expr),+) => {
        let attributes = &[$(opentelemetry::KeyValue::new($attr_key, $attr_value)),+];
        let result = crate::metrics::collect_metrics().metric_exists::<$value>($name, crate::metrics::test_utils::MetricType::Counter, attributes);
        assert_no_metric!(result, $name, None, None, None, attributes);
    };

    ($name:literal, $value: ty, $($($attr_key:ident).+ = $attr_value:expr),+) => {
        let attributes = &[$(opentelemetry::KeyValue::new(stringify!($($attr_key).+), $attr_value)),+];
        let result = crate::metrics::collect_metrics().metric_exists::<$value>($name, crate::metrics::test_utils::MetricType::Counter, attributes);
        assert_no_metric!(result, $name, None, None, None, attributes);
    };


    ($name:literal, $value: ty, $attributes: expr) => {
        let result = crate::metrics::collect_metrics().metric_exists::<$value>($name, crate::metrics::test_utils::MetricType::Counter, $attributes);
        assert_no_metric!(result, $name, None, None, None, &$attributes);
    };

    ($name:literal, $value: ty) => {
        let result = crate::metrics::collect_metrics().metric_exists::<$value>($name, crate::metrics::test_utils::MetricType::Counter, &[]);
        assert_no_metric!(result, $name, None, None, None, &[]);
    };
}

/// Assert the value of a counter metric that has the given name and attributes.
///
/// In asynchronous tests, you must use [`FutureMetricsExt::with_metrics`]. See dev-docs/metrics.md
/// for details: <https://github.com/apollographql/router/blob/4fc63d55104c81c77e6e0a3cca615eac28e39dc3/dev-docs/metrics.md#testing>
#[cfg(test)]
macro_rules! assert_up_down_counter {

    ($($name:ident).+, $value: expr, $($attr_key:literal = $attr_value:expr),+) => {
        let attributes = &[$(opentelemetry::KeyValue::new($attr_key, $attr_value)),+];
        let result = crate::metrics::collect_metrics().assert(stringify!($($name).+), crate::metrics::test_utils::MetricType::UpDownCounter, $value, false, attributes);
        assert_metric!(result, $name, Some($value.into()), None, None, attributes);
    };

    ($($name:ident).+, $value: expr, $($($attr_key:ident).+ = $attr_value:expr),+) => {
        let attributes = &[$(opentelemetry::KeyValue::new(stringify!($($attr_key).+), $attr_value)),+];
        let result = crate::metrics::collect_metrics().assert(stringify!($($name).+), crate::metrics::test_utils::MetricType::UpDownCounter, $value, false, attributes);
        assert_metric!(result, $name, Some($value.into()), None, None, attributes);
    };

    ($name:literal, $value: expr, $($attr_key:literal = $attr_value:expr),+) => {
        let attributes = &[$(opentelemetry::KeyValue::new($attr_key, $attr_value)),+];
        let result = crate::metrics::collect_metrics().assert($name, crate::metrics::test_utils::MetricType::UpDownCounter, $value, false, attributes);
        assert_metric!(result, $name, Some($value.into()), None, None, attributes);
    };

    ($name:literal, $value: expr, $($($attr_key:ident).+ = $attr_value:expr),+) => {
        let attributes = &[$(opentelemetry::KeyValue::new(stringify!($($attr_key).+), $attr_value)),+];
        let result = crate::metrics::collect_metrics().assert($name, crate::metrics::test_utils::MetricType::UpDownCounter, $value, false, attributes);
        assert_metric!(result, $name, Some($value.into()), None, None, attributes);
    };

    ($name:literal, $value: expr) => {
        let result = crate::metrics::collect_metrics().assert($name, crate::metrics::test_utils::MetricType::UpDownCounter, $value, false, &[]);
        assert_metric!(result, $name, Some($value.into()), None, None, &[]);
    };
}

/// Assert the value of a gauge metric that has the given name and attributes.
///
/// In asynchronous tests, you must use [`FutureMetricsExt::with_metrics`]. See dev-docs/metrics.md
/// for details: <https://github.com/apollographql/router/blob/4fc63d55104c81c77e6e0a3cca615eac28e39dc3/dev-docs/metrics.md#testing>
#[cfg(test)]
macro_rules! assert_gauge {

    ($($name:ident).+, $value: expr, $($attr_key:literal = $attr_value:expr),+) => {
        let attributes = &[$(opentelemetry::KeyValue::new($attr_key, $attr_value)),+];
        let result = crate::metrics::collect_metrics().assert(stringify!($($name).+), crate::metrics::test_utils::MetricType::Gauge, $value, false, attributes);
        assert_metric!(result, $name, Some($value.into()), None, None, attributes);
    };

    ($($name:ident).+, $value: expr, $($($attr_key:ident).+ = $attr_value:expr),+) => {
        let attributes = &[$(opentelemetry::KeyValue::new(stringify!($($attr_key).+), $attr_value)),+];
        let result = crate::metrics::collect_metrics().assert(stringify!($($name).+), crate::metrics::test_utils::MetricType::Gauge, $value, false, attributes);
        assert_metric!(result, $name, Some($value.into()), None, None, attributes);
    };

    ($name:literal, $value: expr, $($attr_key:literal = $attr_value:expr),+) => {
        let attributes = &[$(opentelemetry::KeyValue::new($attr_key, $attr_value)),+];
        let result = crate::metrics::collect_metrics().assert($name, crate::metrics::test_utils::MetricType::Gauge, $value, false, attributes);
        assert_metric!(result, $name, Some($value.into()), None, None, attributes);
    };

    ($name:literal, $value: expr, $($($attr_key:ident).+ = $attr_value:expr),+) => {
        let attributes = &[$(opentelemetry::KeyValue::new(stringify!($($attr_key).+), $attr_value)),+];
        let result = crate::metrics::collect_metrics().assert($name, crate::metrics::test_utils::MetricType::Gauge, $value, false, attributes);
        assert_metric!(result, $name, Some($value.into()), None, None, attributes);
    };

    ($name:literal, $value: expr) => {
        let result = crate::metrics::collect_metrics().assert($name, crate::metrics::test_utils::MetricType::Gauge, $value, false, &[]);
        assert_metric!(result, $name, Some($value.into()), None, None, &[]);
    };
}

#[cfg(test)]
macro_rules! assert_histogram_count {

    ($($name:ident).+, $value: expr, $($attr_key:literal = $attr_value:expr),+) => {
        let attributes = &[$(opentelemetry::KeyValue::new($attr_key, $attr_value)),+];
        let result = crate::metrics::collect_metrics().assert(stringify!($($name).+), crate::metrics::test_utils::MetricType::Histogram, $value, true, attributes);
        assert_metric!(result, $name, None, Some($value.into()), Some(num_traits::ToPrimitive::to_u64(&$value).expect("count should be convertible to u64")), attributes);
    };

    ($($name:ident).+, $value: expr, $($($attr_key:ident).+ = $attr_value:expr),+) => {
        let attributes = &[$(opentelemetry::KeyValue::new(stringify!($($attr_key).+), $attr_value)),+];
        let result = crate::metrics::collect_metrics().assert(stringify!($($name).+), crate::metrics::test_utils::MetricType::Histogram, $value, true, attributes);
        assert_metric!(result, $name, None, Some($value.into()), Some(num_traits::ToPrimitive::to_u64(&$value).expect("count should be convertible to u64")), attributes);
    };

    ($name:literal, $value: expr, $($attr_key:literal = $attr_value:expr),+) => {
        let attributes = &[$(opentelemetry::KeyValue::new($attr_key, $attr_value)),+];
        let result = crate::metrics::collect_metrics().assert($name, crate::metrics::test_utils::MetricType::Histogram, $value, true, attributes);
        assert_metric!(result, $name, None, Some($value.into()), Some(num_traits::ToPrimitive::to_u64(&$value).expect("count should be convertible to u64")), attributes);
    };

    ($name:literal, $value: expr, $($($attr_key:ident).+ = $attr_value:expr),+) => {
        let attributes = &[$(opentelemetry::KeyValue::new(stringify!($($attr_key).+), $attr_value)),+];
        let result = crate::metrics::collect_metrics().assert($name, crate::metrics::test_utils::MetricType::Histogram, $value, attributes);
        assert_metric!(result, $name, None, Some($value.into()), Some(num_traits::ToPrimitive::to_u64(&$value).expect("count should be convertible to u64")), attributes);
    };

    ($name:literal, $value: expr) => {
        let result = crate::metrics::collect_metrics().assert($name, crate::metrics::test_utils::MetricType::Histogram, $value, true, &[]);
        assert_metric!(result, $name, None, Some($value.into()), Some(num_traits::ToPrimitive::to_u64(&$value).expect("count should be convertible to u64")), &[]);
    };
}

/// Assert the sum value of a histogram metric with the given name and attributes.
///
/// In asynchronous tests, you must use [`FutureMetricsExt::with_metrics`]. See dev-docs/metrics.md
/// for details: <https://github.com/apollographql/router/blob/4fc63d55104c81c77e6e0a3cca615eac28e39dc3/dev-docs/metrics.md#testing>
#[cfg(test)]
macro_rules! assert_histogram_sum {

    ($($name:ident).+, $value: expr, $($attr_key:literal = $attr_value:expr),+) => {
        let attributes = &[$(opentelemetry::KeyValue::new($attr_key, $attr_value)),+];
        let result = crate::metrics::collect_metrics().assert(stringify!($($name).+), crate::metrics::test_utils::MetricType::Histogram, $value, false, attributes);
        assert_metric!(result, $name, None, Some($value.into()), None, attributes);
    };

    ($($name:ident).+, $value: expr, $($($attr_key:ident).+ = $attr_value:expr),+) => {
        let attributes = &[$(opentelemetry::KeyValue::new(stringify!($($attr_key).+), $attr_value)),+];
        let result = crate::metrics::collect_metrics().assert(stringify!($($name).+), crate::metrics::test_utils::MetricType::Histogram, $value, false, attributes);
        assert_metric!(result, $name, None, Some($value.into()), None, attributes);
    };

    ($name:literal, $value: expr, $($attr_key:literal = $attr_value:expr),+) => {
        let attributes = &[$(opentelemetry::KeyValue::new($attr_key, $attr_value)),+];
        let result = crate::metrics::collect_metrics().assert($name, crate::metrics::test_utils::MetricType::Histogram, $value, false, attributes);
        assert_metric!(result, $name, None, Some($value.into()), None, attributes);
    };

    ($name:literal, $value: expr, $($($attr_key:ident).+ = $attr_value:expr),+) => {
        let attributes = &[$(opentelemetry::KeyValue::new(stringify!($($attr_key).+), $attr_value)),+];
        let result = crate::metrics::collect_metrics().assert($name, crate::metrics::test_utils::MetricType::Histogram, $value, false, attributes);
        assert_metric!(result, $name, None, Some($value.into()), None, attributes);
    };

    ($name:literal, $value: expr) => {
        let result = crate::metrics::collect_metrics().assert($name, crate::metrics::test_utils::MetricType::Histogram, $value, false, &[]);
        assert_metric!(result, $name, None, Some($value.into()), None, &[]);
    };
}

/// Assert that a histogram metric exists with the given name and attributes.
///
/// In asynchronous tests, you must use [`FutureMetricsExt::with_metrics`]. See dev-docs/metrics.md
/// for details: <https://github.com/apollographql/router/blob/4fc63d55104c81c77e6e0a3cca615eac28e39dc3/dev-docs/metrics.md#testing>
#[cfg(test)]
macro_rules! assert_histogram_exists {

    ($($name:ident).+, $value: ty, $($attr_key:literal = $attr_value:expr),+) => {
        let attributes = &[$(opentelemetry::KeyValue::new($attr_key, $attr_value)),+];
        let result = crate::metrics::collect_metrics().metric_exists::<$value>(stringify!($($name).+), crate::metrics::test_utils::MetricType::Histogram, attributes);
        assert_metric!(result, $name, None, None, None, attributes);
    };

    ($($name:ident).+, $value: ty, $($($attr_key:ident).+ = $attr_value:expr),+) => {
        let attributes = &[$(opentelemetry::KeyValue::new(stringify!($($attr_key).+), $attr_value)),+];
        let result = crate::metrics::collect_metrics().metric_exists::<$value>(stringify!($($name).+), crate::metrics::test_utils::MetricType::Histogram, attributes);
        assert_metric!(result, $name, None, None, None, attributes);
    };

    ($name:literal, $value: ty, $($attr_key:literal = $attr_value:expr),+) => {
        let attributes = &[$(opentelemetry::KeyValue::new($attr_key, $attr_value)),+];
        let result = crate::metrics::collect_metrics().metric_exists::<$value>($name, crate::metrics::test_utils::MetricType::Histogram, attributes);
        assert_metric!(result, $name, None, None, None, attributes);
    };

    ($name:literal, $value: ty, $($($attr_key:ident).+ = $attr_value:expr),+) => {
        let attributes = &[$(opentelemetry::KeyValue::new(stringify!($($attr_key).+), $attr_value)),+];
        let result = crate::metrics::collect_metrics().metric_exists::<$value>($name, crate::metrics::test_utils::MetricType::Histogram, attributes);
        assert_metric!(result, $name, None, None, None, attributes);
    };

    ($name:literal, $value: ty) => {
        let result = crate::metrics::collect_metrics().metric_exists::<$value>($name, crate::metrics::test_utils::MetricType::Histogram, &[]);
        assert_metric!(result, $name, None, None, None, &[]);
    };
}

/// Assert that a histogram metric does not exist with the given name and attributes.
///
/// In asynchronous tests, you must use [`FutureMetricsExt::with_metrics`]. See dev-docs/metrics.md
/// for details: <https://github.com/apollographql/router/blob/4fc63d55104c81c77e6e0a3cca615eac28e39dc3/dev-docs/metrics.md#testing>
#[cfg(test)]
macro_rules! assert_histogram_not_exists {

    ($($name:ident).+, $value: ty, $($attr_key:literal = $attr_value:expr),+) => {
        let attributes = &[$(opentelemetry::KeyValue::new($attr_key, $attr_value)),+];
        let result = crate::metrics::collect_metrics().metric_exists::<$value>(stringify!($($name).+), crate::metrics::test_utils::MetricType::Histogram, attributes);
        assert_no_metric!(result, $name, None, None, None, attributes);
    };

    ($($name:ident).+, $value: ty, $($($attr_key:ident).+ = $attr_value:expr),+) => {
        let attributes = &[$(opentelemetry::KeyValue::new(stringify!($($attr_key).+), $attr_value)),+];
        let result = crate::metrics::collect_metrics().metric_exists::<$value>(stringify!($($name).+), crate::metrics::test_utils::MetricType::Histogram, attributes);
        assert_no_metric!(result, $name, None, None, None, attributes);
    };

    ($name:literal, $value: ty, $($attr_key:literal = $attr_value:expr),+) => {
        let attributes = &[$(opentelemetry::KeyValue::new($attr_key, $attr_value)),+];
        let result = crate::metrics::collect_metrics().metric_exists::<$value>($name, crate::metrics::test_utils::MetricType::Histogram, attributes);
        assert_no_metric!(result, $name, None, None, None, attributes);
    };

    ($name:literal, $value: ty, $($($attr_key:ident).+ = $attr_value:expr),+) => {
        let attributes = &[$(opentelemetry::KeyValue::new(stringify!($($attr_key).+), $attr_value)),+];
        let result = crate::metrics::collect_metrics().metric_exists::<$value>($name, crate::metrics::test_utils::MetricType::Histogram, attributes);
        assert_no_metric!(result, $name, None, None, None, attributes);
    };

    ($name:literal, $value: ty) => {
        let result = crate::metrics::collect_metrics().metric_exists::<$value>($name, crate::metrics::test_utils::MetricType::Histogram, &[]);
        assert_no_metric!(result, $name, None, None, None, &[]);
    };
}

/// Assert that all metrics match an [insta] snapshot.
///
/// Consider using [assert_non_zero_metrics_snapshot] to produce more grokkable snapshots if
/// zero-valued metrics are not relevant to your test.
///
/// In asynchronous tests, you must use [`FutureMetricsExt::with_metrics`]. See dev-docs/metrics.md
/// for details: <https://github.com/apollographql/router/blob/4fc63d55104c81c77e6e0a3cca615eac28e39dc3/dev-docs/metrics.md#testing>
#[cfg(test)]
#[allow(unused_macros)]
macro_rules! assert_metrics_snapshot {
    ($file_name: expr) => {
        insta::with_settings!({sort_maps => true, snapshot_suffix => $file_name}, {
            let metrics = crate::metrics::collect_metrics();
            insta::assert_yaml_snapshot!(&metrics.all());
        });

    };
    () => {
        insta::with_settings!({sort_maps => true}, {
            let metrics = crate::metrics::collect_metrics();
            insta::assert_yaml_snapshot!(&metrics.all());
        });
    };
}

/// Assert that all metrics with a non-zero value match an [insta] snapshot.
///
/// In asynchronous tests, you must use [`FutureMetricsExt::with_metrics`]. See dev-docs/metrics.md
/// for details: <https://github.com/apollographql/router/blob/4fc63d55104c81c77e6e0a3cca615eac28e39dc3/dev-docs/metrics.md#testing>
#[cfg(test)]
#[allow(unused_macros)]
macro_rules! assert_non_zero_metrics_snapshot {
    ($file_name: expr) => {
        insta::with_settings!({sort_maps => true, snapshot_suffix => $file_name}, {
            let metrics = crate::metrics::collect_metrics();
            insta::assert_yaml_snapshot!(&metrics.non_zero());
        });
    };
    () => {
        insta::with_settings!({sort_maps => true}, {
            let metrics = crate::metrics::collect_metrics();
            insta::assert_yaml_snapshot!(&metrics.non_zero());
        });
    };
}

#[cfg(test)]
pub(crate) type MetricFuture<T> = Pin<Box<dyn Future<Output = <T as Future>::Output>>>;

/// Extension trait for Futures that wish to test metrics.
pub(crate) trait FutureMetricsExt<T> {
    /// Wraps a Future with metrics collection capabilities.
    ///
    /// This method creates a new Future that will:
    /// 1. Initialize the meter provider before executing the Future
    /// 2. Execute the original Future
    /// 3. Shutdown the meter provider after completion
    ///
    /// This is useful for testing scenarios where you need to ensure metrics are properly
    /// collected throughout the entire Future's execution.
    ///
    /// # Example
    /// ```rust
    /// # use apollo_router::metrics::FutureMetricsExt;
    /// # async fn example() {
    /// let future = async { /* your async code that produces metrics */ };
    /// let result = future.with_metrics().await;
    /// # }
    /// ```
    #[cfg(test)]
    fn with_metrics(
        self,
    ) -> tokio::task::futures::TaskLocalFuture<
        OnceLock<(AggregateMeterProvider, test_utils::ClonableManualReader)>,
        MetricFuture<Self>,
    >
    where
        Self: Sized + Future + 'static,
        <Self as Future>::Output: 'static,
    {
        test_utils::AGGREGATE_METER_PROVIDER_ASYNC.scope(
            Default::default(),
            async move {
                // We want to eagerly create the meter provider, the reason is that this will be shared among subtasks that use `with_current_meter_provider`.
                let _ = meter_provider_internal();
                let result = self.await;
                let _ = tokio::task::spawn_blocking(|| {
                    meter_provider_internal().shutdown();
                })
                .await;
                result
            }
            .boxed_local(),
        )
    }

    /// Propagates the current meter provider to child tasks during test execution.
    ///
    /// This method ensures that the meter provider is properly shared across tasks
    /// during test scenarios. In non-test contexts, it returns the original Future
    /// unchanged.
    ///
    /// # Example
    /// ```rust
    /// # use apollo_router::metrics::FutureMetricsExt;
    /// # async fn example() {
    /// let result = tokio::task::spawn(async { /* your async code that produces metrics */ }.with_current_meter_provider()).await;
    /// # }
    /// ```
    #[cfg(test)]
    fn with_current_meter_provider(
        self,
    ) -> tokio::task::futures::TaskLocalFuture<
        OnceLock<(AggregateMeterProvider, test_utils::ClonableManualReader)>,
        Self,
    >
    where
        Self: Sized + Future + 'static,
        <Self as Future>::Output: 'static,
    {
        // We need to determine if the meter was set. If not then we can use default provider which is empty
        let meter_provider_set = test_utils::AGGREGATE_METER_PROVIDER_ASYNC
            .try_with(|_| {})
            .is_ok();
        if meter_provider_set {
            test_utils::AGGREGATE_METER_PROVIDER_ASYNC
                .scope(test_utils::AGGREGATE_METER_PROVIDER_ASYNC.get(), self)
        } else {
            test_utils::AGGREGATE_METER_PROVIDER_ASYNC.scope(Default::default(), self)
        }
    }

    #[cfg(not(test))]
    fn with_current_meter_provider(self) -> Self
    where
        Self: Sized + Future + 'static,
    {
        // This is intentionally a noop. In the real world meter provider is a global variable.
        self
    }
}

impl<T> FutureMetricsExt<T> for T where T: Future {}

#[cfg(test)]
mod test {
    use opentelemetry::KeyValue;
    use opentelemetry::metrics::MeterProvider;

    use crate::metrics::FutureMetricsExt;
    use crate::metrics::meter_provider;
    use crate::metrics::meter_provider_internal;

    fn assert_unit(name: &str, unit: &str) {
        let collected_metrics = crate::metrics::collect_metrics();
        let metric = collected_metrics.find(name).unwrap();
        assert_eq!(metric.unit, unit);
    }

    #[test]
    fn test_gauge() {
        // Observables are cleaned up when they dropped, so keep this around.
        let _gauge = meter_provider()
            .meter("test")
            .u64_observable_gauge("test")
            .with_callback(|m| m.observe(5, &[]))
            .init();
        assert_gauge!("test", 5);
    }

    #[test]
    fn test_gauge_record() {
        let gauge = meter_provider().meter("test").u64_gauge("test").init();
        gauge.record(5, &[]);
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
        assert_counter!("test", 1, &attributes);
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
            let _guard = i64_up_down_counter!("test", "test description", 1, "attr" = "val");
            assert_up_down_counter!("test", 1, "attr" = "val");
        }
        .with_metrics()
        .await;
    }

    #[tokio::test]
    async fn test_f64_up_down_counter() {
        async {
            let _guard = f64_up_down_counter!("test", "test description", 1.5, "attr" = "val");
            assert_up_down_counter!("test", 1.5, "attr" = "val");
        }
        .with_metrics()
        .await;
    }

    #[tokio::test]
    async fn test_i64_up_down_counter_guard_auto_decrement() {
        async {
            // Test that dropping the guard decrements the counter
            {
                let _guard =
                    i64_up_down_counter!("test_guard", "test description", 1, "attr" = "val");
                assert_up_down_counter!("test_guard", 1, "attr" = "val");
            }
            // After guard is dropped, counter should be back to 0
            assert_up_down_counter!("test_guard", 0, "attr" = "val");
        }
        .with_metrics()
        .await;
    }

    #[tokio::test]
    async fn test_i64_up_down_counter_guard_multiple() {
        async {
            // Test multiple guards with the same metric
            let _guard1 = i64_up_down_counter!("test_multi", "test description", 1, "attr" = "val");
            assert_up_down_counter!("test_multi", 1, "attr" = "val");

            let _guard2 = i64_up_down_counter!("test_multi", "test description", 1, "attr" = "val");
            assert_up_down_counter!("test_multi", 2, "attr" = "val");

            let _guard3 = i64_up_down_counter!("test_multi", "test description", 1, "attr" = "val");
            assert_up_down_counter!("test_multi", 3, "attr" = "val");

            drop(_guard2);
            assert_up_down_counter!("test_multi", 2, "attr" = "val");

            drop(_guard1);
            assert_up_down_counter!("test_multi", 1, "attr" = "val");

            drop(_guard3);
            assert_up_down_counter!("test_multi", 0, "attr" = "val");
        }
        .with_metrics()
        .await;
    }

    #[tokio::test]
    async fn test_i64_up_down_counter_guard_different_attributes() {
        async {
            // Test guards with different attributes
            let _guard1 =
                i64_up_down_counter!("test_attrs", "test description", 1, "attr" = "val1");
            let _guard2 =
                i64_up_down_counter!("test_attrs", "test description", 1, "attr" = "val2");

            assert_up_down_counter!("test_attrs", 1, "attr" = "val1");
            assert_up_down_counter!("test_attrs", 1, "attr" = "val2");

            drop(_guard1);
            assert_up_down_counter!("test_attrs", 0, "attr" = "val1");
            assert_up_down_counter!("test_attrs", 1, "attr" = "val2");

            drop(_guard2);
            assert_up_down_counter!("test_attrs", 0, "attr" = "val2");
        }
        .with_metrics()
        .await;
    }

    #[tokio::test]
    async fn test_f64_up_down_counter_guard_auto_decrement() {
        async {
            // Test that dropping the guard decrements the counter
            {
                let _guard =
                    f64_up_down_counter!("test_f64_guard", "test description", 2.5, "attr" = "val");
                assert_up_down_counter!("test_f64_guard", 2.5, "attr" = "val");
            }
            // After guard is dropped, counter should be back to 0
            assert_up_down_counter!("test_f64_guard", 0.0, "attr" = "val");
        }
        .with_metrics()
        .await;
    }

    #[tokio::test]
    async fn test_u64_histogram() {
        async {
            u64_histogram!("test", "test description", 1, "attr" = "val");
            assert_histogram_sum!("test", 1, "attr" = "val");
        }
        .with_metrics()
        .await;
    }

    #[tokio::test]
    async fn test_f64_histogram() {
        async {
            f64_histogram!("test", "test description", 1.0, "attr" = "val");
            assert_histogram_sum!("test", 1, "attr" = "val");
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
            assert_histogram_sum!("test", 1, "attr" = "val");
        }
        .with_metrics()
        .await;
    }

    #[tokio::test]
    #[should_panic]
    async fn test_type_up_down_counter() {
        async {
            f64_up_down_counter!("test", "test description", 1.0, "attr" = "val");
            assert_histogram_sum!("test", 1, "attr" = "val");
        }
        .with_metrics()
        .await;
    }

    #[tokio::test]
    #[should_panic]
    async fn test_type_gauge() {
        async {
            let _gauge = meter_provider()
                .meter("test")
                .u64_observable_gauge("test")
                .with_callback(|m| m.observe(5, &[]))
                .init();
            assert_histogram_sum!("test", 1, "attr" = "val");
        }
        .with_metrics()
        .await;
    }

    #[test]
    fn parse_attributes_should_handle_multiple_input_types() {
        let variable = 123;
        let parsed_idents = parse_attributes!(hello = "world", my.variable = variable);
        let parsed_literals = parse_attributes!("hello" = "world", "my.variable" = variable);
        let parsed_provided = parse_attributes!(vec![
            KeyValue::new("hello", "world"),
            KeyValue::new("my.variable", variable)
        ]);

        assert_eq!(parsed_idents, parsed_literals);
        assert_eq!(parsed_idents.as_slice(), parsed_provided.as_slice());
        assert_eq!(parsed_literals.as_slice(), parsed_provided.as_slice());
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
        assert_eq!(meter_provider_internal().registered_instruments(), 0);

        // Call the metrics, it will be registered
        test();
        assert_counter!("test", 1, "attr" = "val");
        assert_eq!(meter_provider_internal().registered_instruments(), 1);

        // Call the metrics again, but the second call will not register a new metric because it will have be retrieved from the static
        test();
        assert_counter!("test", 2, "attr" = "val");
        assert_eq!(meter_provider_internal().registered_instruments(), 1);

        // Force invalidation of instruments
        meter_provider_internal().invalidate();
        assert_eq!(meter_provider_internal().registered_instruments(), 0);

        // Slow path
        test();
        assert_eq!(meter_provider_internal().registered_instruments(), 1);

        // Fast path
        test();
        assert_eq!(meter_provider_internal().registered_instruments(), 1);
    }

    #[tokio::test]
    async fn test_f64_histogram_with_unit() {
        async {
            f64_histogram_with_unit!("test", "test description", "m/s", 1.0, "attr" = "val");
            assert_histogram_sum!("test", 1, "attr" = "val");
            assert_unit("test", "m/s");
        }
        .with_metrics()
        .await;
    }

    #[tokio::test]
    async fn test_u64_counter_with_unit() {
        async {
            u64_counter_with_unit!("test", "test description", "Hz", 1, attr = "val");
            assert_counter!("test", 1, "attr" = "val");
            assert_unit("test", "Hz");
        }
        .with_metrics()
        .await;
    }

    #[tokio::test]
    async fn test_i64_up_down_counter_with_unit() {
        async {
            let _guard = i64_up_down_counter_with_unit!(
                "test",
                "test description",
                "{request}",
                1,
                attr = "val"
            );
            assert_up_down_counter!("test", 1, "attr" = "val");
            assert_unit("test", "{request}");
        }
        .with_metrics()
        .await;
    }

    #[tokio::test]
    async fn test_f64_up_down_counter_with_unit() {
        async {
            let _guard = f64_up_down_counter_with_unit!(
                "test",
                "test description",
                "kg",
                1.5,
                "attr" = "val"
            );
            assert_up_down_counter!("test", 1.5, "attr" = "val");
            assert_unit("test", "kg");
        }
        .with_metrics()
        .await;
    }

    #[tokio::test]
    async fn test_u64_histogram_with_unit() {
        async {
            u64_histogram_with_unit!("test", "test description", "{packet}", 1, "attr" = "val");
            assert_histogram_sum!("test", 1, "attr" = "val");
            assert_unit("test", "{packet}");
        }
        .with_metrics()
        .await;
    }

    #[tokio::test]
    async fn test_f64_counter_with_unit() {
        async {
            f64_counter_with_unit!("test", "test description", "s", 1.5, "attr" = "val");
            assert_counter!("test", 1.5, "attr" = "val");
            assert_unit("test", "s");
        }
        .with_metrics()
        .await;
    }

    #[tokio::test]
    async fn test_metrics_across_tasks() {
        async {
            // Initial metric in the main task
            u64_counter!("apollo.router.test", "metric", 1);
            assert_counter!("apollo.router.test", 1);

            // Spawn a task that also records metrics
            let handle = tokio::spawn(
                async move {
                    u64_counter!("apollo.router.test", "metric", 2);
                }
                .with_current_meter_provider(),
            );

            // Wait for the spawned task to complete
            handle.await.unwrap();

            // The metric should now be 3 since both tasks contributed
            assert_counter!("apollo.router.test", 3);
        }
        .with_metrics()
        .await;
    }
}
