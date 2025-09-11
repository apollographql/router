use std::fmt::Display;
use std::fmt::Formatter;
use std::time::Duration;

use opentelemetry::Context;
use opentelemetry::trace::TraceResult;
use opentelemetry_sdk::Resource;
use opentelemetry_sdk::export::trace::SpanData;
use opentelemetry_sdk::trace::BatchConfig;
use opentelemetry_sdk::trace::BatchConfigBuilder;
use opentelemetry_sdk::trace::Builder;
use opentelemetry_sdk::trace::Span;
use opentelemetry_sdk::trace::SpanProcessor;
use schemars::JsonSchema;
use serde::Deserialize;
use tower::BoxError;

use super::config_new::spans::Spans;
use super::formatters::APOLLO_CONNECTOR_PREFIX;
use super::formatters::APOLLO_PRIVATE_PREFIX;
use crate::plugins::telemetry::config::TracingCommon;
use crate::plugins::telemetry::tracing::datadog::DatadogSpanProcessor;

pub(crate) mod apollo;
pub(crate) mod apollo_telemetry;
pub(crate) mod datadog;
#[allow(unreachable_pub, dead_code)]
pub(crate) mod datadog_exporter;
pub(crate) mod otlp;
pub(crate) mod reload;
pub(crate) mod zipkin;

pub(crate) trait TracingConfigurator {
    fn enabled(&self) -> bool;
    fn apply(
        &self,
        builder: Builder,
        common: &TracingCommon,
        spans: &Spans,
    ) -> Result<Builder, BoxError>;
}

#[derive(Debug)]
struct ApolloFilterSpanProcessor<T: SpanProcessor> {
    delegate: T,
}

impl<T: SpanProcessor> SpanProcessor for ApolloFilterSpanProcessor<T> {
    fn on_start(&self, span: &mut Span, cx: &Context) {
        self.delegate.on_start(span, cx);
    }

    fn on_end(&self, span: SpanData) {
        if span.attributes.iter().any(|kv| {
            kv.key.as_str().starts_with(APOLLO_PRIVATE_PREFIX)
                || kv.key.as_str().starts_with(APOLLO_CONNECTOR_PREFIX)
        }) {
            let span = SpanData {
                attributes: span
                    .attributes
                    .into_iter()
                    .filter(|kv| {
                        !kv.key.as_str().starts_with(APOLLO_PRIVATE_PREFIX)
                            && !kv.key.as_str().starts_with(APOLLO_CONNECTOR_PREFIX)
                    })
                    .collect(),
                ..span
            };

            self.delegate.on_end(span);
        } else {
            self.delegate.on_end(span);
        }
    }

    fn force_flush(&self) -> TraceResult<()> {
        self.delegate.force_flush()
    }

    fn shutdown(&self) -> TraceResult<()> {
        self.delegate.shutdown()
    }

    fn set_resource(&mut self, resource: &Resource) {
        self.delegate.set_resource(resource)
    }
}

trait SpanProcessorExt
where
    Self: Sized + SpanProcessor,
{
    fn filtered(self) -> ApolloFilterSpanProcessor<Self>;
    fn always_sampled(self) -> DatadogSpanProcessor<Self>;
}

impl<T: SpanProcessor> SpanProcessorExt for T
where
    Self: Sized,
{
    fn filtered(self) -> ApolloFilterSpanProcessor<Self> {
        ApolloFilterSpanProcessor { delegate: self }
    }

    /// This span processor will always send spans to the exporter even if they are not sampled. This is useful for the datadog agent which
    /// uses spans for metrics.
    fn always_sampled(self) -> DatadogSpanProcessor<Self> {
        DatadogSpanProcessor::new(self)
    }
}

/// Batch processor configuration
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(default)]
pub(crate) struct BatchProcessorConfig {
    #[serde(deserialize_with = "humantime_serde::deserialize")]
    #[schemars(with = "String")]
    /// The delay interval in milliseconds between two consecutive processing
    /// of batches. The default value is 5 seconds.
    pub(crate) scheduled_delay: Duration,

    /// The maximum queue size to buffer spans for delayed processing. If the
    /// queue gets full it drops the spans. The default value of is 2048.
    pub(crate) max_queue_size: usize,

    /// The maximum number of spans to process in a single batch. If there are
    /// more than one batch worth of spans then it processes multiple batches
    /// of spans one batch after the other without any delay. The default value
    /// is 512.
    pub(crate) max_export_batch_size: usize,

    /// The maximum duration to export a batch of data.
    /// The default value is 30 seconds.
    #[serde(deserialize_with = "humantime_serde::deserialize")]
    #[schemars(with = "String")]
    pub(crate) max_export_timeout: Duration,

    /// Maximum number of concurrent exports
    ///
    /// Limits the number of spawned tasks for exports and thus memory consumed
    /// by an exporter. A value of 1 will cause exports to be performed
    /// synchronously on the BatchSpanProcessor task.
    /// The default is 1.
    pub(crate) max_concurrent_exports: usize,
}

pub(crate) fn scheduled_delay_default() -> Duration {
    Duration::from_secs(5)
}

pub(crate) fn max_queue_size_default() -> usize {
    2048
}

fn max_export_batch_size_default() -> usize {
    512
}

pub(crate) fn max_export_timeout_default() -> Duration {
    Duration::from_secs(30)
}

fn max_concurrent_exports_default() -> usize {
    1
}

impl From<BatchProcessorConfig> for BatchConfig {
    fn from(config: BatchProcessorConfig) -> Self {
        BatchConfigBuilder::default()
            .with_scheduled_delay(config.scheduled_delay)
            .with_max_queue_size(config.max_queue_size)
            .with_max_export_batch_size(config.max_export_batch_size)
            .with_max_export_timeout(config.max_export_timeout)
            .with_max_concurrent_exports(config.max_concurrent_exports)
            .build()
    }
}

impl Display for BatchProcessorConfig {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str(&format!("BatchConfig {{ scheduled_delay={}, max_queue_size={}, max_export_batch_size={}, max_export_timeout={}, max_concurrent_exports={} }}",
                             humantime::format_duration(self.scheduled_delay),
                             self.max_queue_size,
                             self.max_export_batch_size,
                             humantime::format_duration(self.max_export_timeout),
                             self.max_concurrent_exports))
    }
}

impl Default for BatchProcessorConfig {
    fn default() -> Self {
        BatchProcessorConfig {
            scheduled_delay: scheduled_delay_default(),
            max_queue_size: max_queue_size_default(),
            max_export_batch_size: max_export_batch_size_default(),
            max_export_timeout: max_export_timeout_default(),
            max_concurrent_exports: max_concurrent_exports_default(),
        }
    }
}
