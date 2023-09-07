use std::collections::HashMap;
use std::fmt;
use std::sync::RwLock;

use opentelemetry::metrics::Counter;
use opentelemetry::metrics::Histogram;
use opentelemetry::metrics::Meter;
use opentelemetry::metrics::MeterProvider;
use opentelemetry::metrics::ObservableGauge;
use opentelemetry::metrics::UpDownCounter;
use opentelemetry::Context as OtelContext;
use opentelemetry::Key;
use opentelemetry::KeyValue;
use opentelemetry::Value;
use tracing::field::Visit;
use tracing::Subscriber;
use tracing_core::Field;
use tracing_subscriber::layer::Context;
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::Layer;

use super::METRIC_PREFIX_COUNTER;
use super::METRIC_PREFIX_HISTOGRAM;
use super::METRIC_PREFIX_MONOTONIC_COUNTER;
use super::METRIC_PREFIX_VALUE;

macro_rules! log_and_panic_in_debug_build {
    ($($tokens:tt)+) => {{
        tracing::debug!($($tokens)+);
        #[cfg(debug_assertions)]
        panic!("metric type error, see DEBUG log for details. Release builds will not panic but will still emit a debug log message");
    }};
}

#[derive(Default)]
pub(crate) struct Instruments {
    u64_counter: MetricsMap<Counter<u64>>,
    f64_counter: MetricsMap<Counter<f64>>,
    i64_up_down_counter: MetricsMap<UpDownCounter<i64>>,
    f64_up_down_counter: MetricsMap<UpDownCounter<f64>>,
    u64_histogram: MetricsMap<Histogram<u64>>,
    i64_histogram: MetricsMap<Histogram<i64>>,
    f64_histogram: MetricsMap<Histogram<f64>>,
    u64_gauge: MetricsMap<ObservableGauge<u64>>,
}

type MetricsMap<T> = RwLock<HashMap<&'static str, T>>;

#[derive(Copy, Clone, Debug)]
pub(crate) enum InstrumentType {
    CounterU64(u64),
    CounterF64(f64),
    UpDownCounterI64(i64),
    UpDownCounterF64(f64),
    HistogramU64(u64),
    HistogramI64(i64),
    HistogramF64(f64),
    GaugeU64(u64),
}

impl Instruments {
    pub(crate) fn update_metric(
        &self,
        cx: &OtelContext,
        meter: &Meter,
        instrument_type: InstrumentType,
        metric_name: &'static str,
        custom_attributes: &[KeyValue],
    ) {
        fn update_or_insert<T>(
            map: &MetricsMap<T>,
            name: &'static str,
            insert: impl FnOnce() -> T,
            update: impl FnOnce(&T),
        ) {
            {
                let lock = map.read().unwrap();
                if let Some(metric) = lock.get(name) {
                    update(metric);
                    return;
                }
            }

            // that metric did not already exist, so we have to acquire a write lock to
            // create it.
            let mut lock = map.write().unwrap();

            // handle the case where the entry was created while we were waiting to
            // acquire the write lock
            let metric = lock.entry(name).or_insert_with(insert);
            update(metric)
        }

        match instrument_type {
            InstrumentType::CounterU64(value) => {
                update_or_insert(
                    &self.u64_counter,
                    metric_name,
                    || meter.u64_counter(metric_name).init(),
                    |ctr| ctr.add(cx, value, custom_attributes),
                );
            }
            InstrumentType::CounterF64(value) => {
                update_or_insert(
                    &self.f64_counter,
                    metric_name,
                    || meter.f64_counter(metric_name).init(),
                    |ctr| ctr.add(cx, value, custom_attributes),
                );
            }
            InstrumentType::UpDownCounterI64(value) => {
                update_or_insert(
                    &self.i64_up_down_counter,
                    metric_name,
                    || meter.i64_up_down_counter(metric_name).init(),
                    |ctr| ctr.add(cx, value, custom_attributes),
                );
            }
            InstrumentType::UpDownCounterF64(value) => {
                update_or_insert(
                    &self.f64_up_down_counter,
                    metric_name,
                    || meter.f64_up_down_counter(metric_name).init(),
                    |ctr| ctr.add(cx, value, custom_attributes),
                );
            }
            InstrumentType::HistogramU64(value) => {
                update_or_insert(
                    &self.u64_histogram,
                    metric_name,
                    || meter.u64_histogram(metric_name).init(),
                    |rec| rec.record(cx, value, custom_attributes),
                );
            }
            InstrumentType::HistogramI64(value) => {
                update_or_insert(
                    &self.i64_histogram,
                    metric_name,
                    || meter.i64_histogram(metric_name).init(),
                    |rec| rec.record(cx, value, custom_attributes),
                );
            }
            InstrumentType::HistogramF64(value) => {
                update_or_insert(
                    &self.f64_histogram,
                    metric_name,
                    || meter.f64_histogram(metric_name).init(),
                    |rec| rec.record(cx, value, custom_attributes),
                );
            }
            InstrumentType::GaugeU64(value) => {
                update_or_insert(
                    &self.u64_gauge,
                    metric_name,
                    || meter.u64_observable_gauge(metric_name).init(),
                    |gauge| gauge.observe(cx, value, custom_attributes),
                );
            }
        };
    }
}

pub(crate) struct MetricVisitor<'a> {
    pub(crate) instruments: &'a Instruments,
    pub(crate) metric: Option<(&'static str, InstrumentType)>,
    pub(crate) custom_attributes: Vec<KeyValue>,
    pub(crate) meter: &'a Meter,
    attributes_ignored: bool,
}

impl<'a> MetricVisitor<'a> {
    fn set_metric(&mut self, name: &'static str, instrument_type: InstrumentType) {
        self.metric = Some((name, instrument_type));
        if self.attributes_ignored {
            log_and_panic_in_debug_build!(
                metric_name = name,
                "metric attributes must be declared after the metric value. Some attributes have been ignored"
            );
        }
    }
}

impl<'a> Visit for MetricVisitor<'a> {
    fn record_f64(&mut self, field: &Field, value: f64) {
        if let Some(metric_name) = field.name().strip_prefix(METRIC_PREFIX_MONOTONIC_COUNTER) {
            self.set_metric(metric_name, InstrumentType::CounterF64(value));
        } else if let Some(metric_name) = field.name().strip_prefix(METRIC_PREFIX_COUNTER) {
            self.set_metric(metric_name, InstrumentType::UpDownCounterF64(value));
        } else if let Some(metric_name) = field.name().strip_prefix(METRIC_PREFIX_HISTOGRAM) {
            self.set_metric(metric_name, InstrumentType::HistogramF64(value));
        } else if let Some(metric_name) = field.name().strip_prefix(METRIC_PREFIX_VALUE) {
            log_and_panic_in_debug_build!(
                metric_name,
                "gauge must be u64. This metric will be ignored"
            );
        } else if self.metric.is_some() {
            self.custom_attributes.push(KeyValue::new(
                Key::from_static_str(field.name()),
                Value::from(value),
            ));
        } else {
            self.attributes_ignored = true
        }
    }

    fn record_i64(&mut self, field: &Field, value: i64) {
        if let Some(metric_name) = field.name().strip_prefix(METRIC_PREFIX_MONOTONIC_COUNTER) {
            log_and_panic_in_debug_build!(
                metric_name,
                "monotonic counter must be u64 or f64. This metric will be ignored"
            );
        } else if let Some(metric_name) = field.name().strip_prefix(METRIC_PREFIX_COUNTER) {
            self.set_metric(metric_name, InstrumentType::UpDownCounterI64(value));
        } else if let Some(metric_name) = field.name().strip_prefix(METRIC_PREFIX_HISTOGRAM) {
            self.set_metric(metric_name, InstrumentType::HistogramI64(value));
        } else if let Some(metric_name) = field.name().strip_prefix(METRIC_PREFIX_VALUE) {
            log_and_panic_in_debug_build!(
                metric_name,
                "gauge must be u64. This metric will be ignored"
            );
        } else if self.metric.is_some() {
            self.custom_attributes.push(KeyValue::new(
                Key::from_static_str(field.name()),
                Value::from(value),
            ));
        } else {
            self.attributes_ignored = true
        }
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        if let Some(metric_name) = field.name().strip_prefix(METRIC_PREFIX_MONOTONIC_COUNTER) {
            self.set_metric(metric_name, InstrumentType::CounterU64(value));
        } else if let Some(metric_name) = field.name().strip_prefix(METRIC_PREFIX_COUNTER) {
            log_and_panic_in_debug_build!(
                metric_name,
                "counter must be i64. This metric will be ignored"
            );
        } else if let Some(metric_name) = field.name().strip_prefix(METRIC_PREFIX_HISTOGRAM) {
            self.set_metric(metric_name, InstrumentType::HistogramU64(value));
        } else if let Some(metric_name) = field.name().strip_prefix(METRIC_PREFIX_VALUE) {
            self.set_metric(metric_name, InstrumentType::GaugeU64(value));
        } else if self.metric.is_some() {
            log_and_panic_in_debug_build!(
                name = field.name(),
                "metric attribute must be i64, f64, string or bool. This attribute will be ignored"
            );
        } else {
            self.attributes_ignored = true
        }
    }

    fn record_i128(&mut self, field: &Field, _value: i128) {
        if let Some(metric_name) = field.name().strip_prefix(METRIC_PREFIX_MONOTONIC_COUNTER) {
            log_and_panic_in_debug_build!(
                metric_name,
                "monotonic counter must be u64 or f64. This metric will be ignored"
            );
        } else if let Some(metric_name) = field.name().strip_prefix(METRIC_PREFIX_COUNTER) {
            log_and_panic_in_debug_build!(
                metric_name,
                "counter must be i64. This metric will be ignored"
            );
        } else if let Some(metric_name) = field.name().strip_prefix(METRIC_PREFIX_HISTOGRAM) {
            log_and_panic_in_debug_build!(
                metric_name,
                "histogram must be u64, i64 or f64. This metric will be ignored"
            );
        } else if let Some(metric_name) = field.name().strip_prefix(METRIC_PREFIX_VALUE) {
            log_and_panic_in_debug_build!(
                metric_name,
                "gauge must be u64. This metric will be ignored"
            );
        } else if self.metric.is_some() {
            log_and_panic_in_debug_build!(
                name = field.name(),
                "metric attribute must be i64, f64, string or bool. This attribute will be ignored"
            );
        } else {
            self.attributes_ignored = true
        }
    }

    fn record_u128(&mut self, field: &Field, _value: u128) {
        if let Some(metric_name) = field.name().strip_prefix(METRIC_PREFIX_MONOTONIC_COUNTER) {
            log_and_panic_in_debug_build!(
                metric_name,
                "monotonic counter must be u64 or f64. This metric will be ignored"
            );
        } else if let Some(metric_name) = field.name().strip_prefix(METRIC_PREFIX_COUNTER) {
            log_and_panic_in_debug_build!(
                metric_name,
                "counter must be i64. This metric will be ignored"
            );
        } else if let Some(metric_name) = field.name().strip_prefix(METRIC_PREFIX_HISTOGRAM) {
            log_and_panic_in_debug_build!(
                metric_name,
                "histogram must be u64, i64 or f64. This metric will be ignored"
            );
        } else if let Some(metric_name) = field.name().strip_prefix(METRIC_PREFIX_VALUE) {
            log_and_panic_in_debug_build!(
                metric_name,
                "gauge must be u64. This metric will be ignored"
            );
        } else if self.metric.is_some() {
            log_and_panic_in_debug_build!(
                name = field.name(),
                "metric attribute must be i64, f64, string or bool. This attribute will be ignored"
            );
        } else {
            self.attributes_ignored = true
        }
    }

    fn record_bool(&mut self, field: &Field, value: bool) {
        if let Some(metric_name) = field.name().strip_prefix(METRIC_PREFIX_MONOTONIC_COUNTER) {
            log_and_panic_in_debug_build!(
                metric_name,
                "monotonic counter must be u64 or f64. This metric will be ignored"
            );
        } else if let Some(metric_name) = field.name().strip_prefix(METRIC_PREFIX_COUNTER) {
            log_and_panic_in_debug_build!(
                metric_name,
                "counter must be i64. This metric will be ignored"
            );
        } else if let Some(metric_name) = field.name().strip_prefix(METRIC_PREFIX_HISTOGRAM) {
            log_and_panic_in_debug_build!(
                metric_name,
                "histogram must be u64, i64 or f64. This metric will be ignored"
            );
        } else if let Some(metric_name) = field.name().strip_prefix(METRIC_PREFIX_VALUE) {
            log_and_panic_in_debug_build!(
                metric_name,
                "gauge must be u64. This metric will be ignored"
            );
        } else if self.metric.is_some() {
            self.custom_attributes.push(KeyValue::new(
                Key::from_static_str(field.name()),
                Value::from(value),
            ));
        } else {
            self.attributes_ignored = true
        }
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() != "message" {
            if let Some(metric_name) = field.name().strip_prefix(METRIC_PREFIX_MONOTONIC_COUNTER) {
                log_and_panic_in_debug_build!(
                    metric_name,
                    "monotonic counter must be u64 or f64. This metric will be ignored"
                );
            } else if let Some(metric_name) = field.name().strip_prefix(METRIC_PREFIX_COUNTER) {
                log_and_panic_in_debug_build!(
                    metric_name,
                    "counter must be i64. This metric will be ignored"
                );
            } else if let Some(metric_name) = field.name().strip_prefix(METRIC_PREFIX_HISTOGRAM) {
                log_and_panic_in_debug_build!(
                    metric_name,
                    "histogram must be u64, i64 or f64. This metric will be ignored"
                );
            } else if let Some(metric_name) = field.name().strip_prefix(METRIC_PREFIX_VALUE) {
                log_and_panic_in_debug_build!(
                    metric_name,
                    "gauge must be u64. This metric will be ignored"
                );
            } else if self.metric.is_some() {
                self.custom_attributes.push(KeyValue::new(
                    Key::from_static_str(field.name()),
                    Value::from(value.to_string()),
                ));
            } else {
                self.attributes_ignored = true
            }
        }
    }

    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        if field.name() != "message" {
            if let Some(metric_name) = field.name().strip_prefix(METRIC_PREFIX_MONOTONIC_COUNTER) {
                log_and_panic_in_debug_build!(
                    metric_name,
                    "monotonic counter must be u64 or f64. This metric will be ignored"
                );
            } else if let Some(metric_name) = field.name().strip_prefix(METRIC_PREFIX_COUNTER) {
                log_and_panic_in_debug_build!(
                    metric_name,
                    "counter must be i64. This metric will be ignored"
                );
            } else if let Some(metric_name) = field.name().strip_prefix(METRIC_PREFIX_HISTOGRAM) {
                log_and_panic_in_debug_build!(
                    metric_name,
                    "histogram must be u64, i64 or f64. This metric will be ignored"
                );
            } else if let Some(metric_name) = field.name().strip_prefix(METRIC_PREFIX_VALUE) {
                log_and_panic_in_debug_build!(
                    metric_name,
                    "gauge must be u64. This metric will be ignored"
                );
            } else if self.metric.is_some() {
                self.custom_attributes.push(KeyValue::new(
                    Key::from_static_str(field.name()),
                    Value::from(format!("{value:?}")),
                ));
            } else {
                self.attributes_ignored = true
            }
        }
    }
}

impl<'a> MetricVisitor<'a> {
    fn finish(self) {
        if let Some((metric_name, instrument_type)) = self.metric {
            let cx = OtelContext::current();
            self.instruments.update_metric(
                &cx,
                self.meter,
                instrument_type,
                metric_name,
                &self.custom_attributes,
            );
        }
    }
}

pub(crate) struct MetricsLayer {
    meter: Meter,
    instruments: Instruments,
}

impl MetricsLayer {
    pub(crate) fn new(meter_provider: &impl MeterProvider) -> Self {
        Self {
            meter: meter_provider.meter("apollo/router"),
            instruments: Default::default(),
        }
    }
}

impl<S> Layer<S> for MetricsLayer
where
    S: Subscriber + for<'span> LookupSpan<'span>,
{
    fn on_event(&self, event: &tracing::Event<'_>, _ctx: Context<'_, S>) {
        let mut metric_visitor = MetricVisitor {
            instruments: &self.instruments,
            meter: &self.meter,
            metric: None,
            custom_attributes: Vec::new(),
            attributes_ignored: false,
        };
        event.record(&mut metric_visitor);
        metric_visitor.finish();
    }
}
