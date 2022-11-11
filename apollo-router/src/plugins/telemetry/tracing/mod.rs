use std::fmt::Display;
use std::fmt::Formatter;
use std::time::Duration;

use opentelemetry::sdk::export::trace::SpanData;
use opentelemetry::sdk::trace::BatchConfig;
use opentelemetry::sdk::trace::Builder;
use opentelemetry::sdk::trace::EvictedHashMap;
use opentelemetry::sdk::trace::Span;
use opentelemetry::sdk::trace::SpanProcessor;
use opentelemetry::trace::TraceResult;
use opentelemetry::Context;
use opentelemetry::KeyValue;
use reqwest::Url;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Deserializer;
use serde::Serialize;
use tower::BoxError;
use url::ParseError;

use crate::plugins::telemetry::config::Trace;

pub(crate) mod apollo;
pub(crate) mod apollo_telemetry;
pub(crate) mod datadog;
pub(crate) mod jaeger;
pub(crate) mod otlp;
pub(crate) mod zipkin;

pub(crate) trait TracingConfigurator {
    fn apply(&self, builder: Builder, trace_config: &Trace) -> Result<Builder, BoxError>;
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "snake_case", untagged)]
pub(crate) enum AgentEndpoint {
    Default(AgentDefault),
    Url(Url),
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub(crate) enum AgentDefault {
    Default,
}

pub(crate) fn parse_url_for_endpoint(mut s: String) -> Result<Url, ParseError> {
    match Url::parse(&s) {
        Ok(url) => {
            // support the case of 'collector:4317' where url parses 'collector'
            // as the scheme instead of the host
            if url.host().is_none() && (url.scheme() != "http" || url.scheme() != "https") {
                s = format!("http://{}", s);
                Url::parse(&s)
            } else {
                Ok(url)
            }
        }
        Err(err) => {
            match err {
                // support the case of '127.0.0.1:4317' where url is interpreted
                // as a relative url without a base
                ParseError::RelativeUrlWithoutBase => {
                    s = format!("http://{}", s);
                    Url::parse(&s)
                }
                _ => Err(err),
            }
        }
    }
}

pub(crate) fn deser_endpoint<'de, D>(deserializer: D) -> Result<AgentEndpoint, D::Error>
where
    D: Deserializer<'de>,
{
    let s = String::deserialize(deserializer)?;
    if s == "default" {
        return Ok(AgentEndpoint::Default(AgentDefault::Default));
    }
    let url = parse_url_for_endpoint(s).map_err(serde::de::Error::custom)?;
    Ok(AgentEndpoint::Url(url))
}

#[derive(Debug)]
struct ApolloFilterSpanProcessor<T: SpanProcessor> {
    delegate: T,
}

pub(crate) static APOLLO_PRIVATE_PREFIX: &str = "apollo_private.";

impl<T: SpanProcessor> SpanProcessor for ApolloFilterSpanProcessor<T> {
    fn on_start(&self, span: &mut Span, cx: &Context) {
        self.delegate.on_start(span, cx);
    }

    fn on_end(&self, span: SpanData) {
        if span
            .attributes
            .iter()
            .any(|(key, _)| key.as_str().starts_with(APOLLO_PRIVATE_PREFIX))
        {
            let attributes_len = span.attributes.len();
            let span = SpanData {
                attributes: span
                    .attributes
                    .into_iter()
                    .filter(|(k, _)| !k.as_str().starts_with(APOLLO_PRIVATE_PREFIX))
                    .fold(
                        EvictedHashMap::new(attributes_len as u32, attributes_len),
                        |mut m, (k, v)| {
                            m.insert(KeyValue::new(k, v));
                            m
                        },
                    ),
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

    fn shutdown(&mut self) -> TraceResult<()> {
        self.delegate.shutdown()
    }
}

trait SpanProcessorExt
where
    Self: Sized + SpanProcessor,
{
    fn filtered(self) -> ApolloFilterSpanProcessor<Self>;
}

impl<T: SpanProcessor> SpanProcessorExt for T
where
    Self: Sized,
{
    fn filtered(self) -> ApolloFilterSpanProcessor<Self> {
        ApolloFilterSpanProcessor { delegate: self }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, Default)]
pub(crate) struct BatchProcessorConfig {
    #[serde(deserialize_with = "humantime_serde::deserialize", default)]
    #[schemars(with = "String", default)]
    /// The delay interval in milliseconds between two consecutive processing
    /// of batches. The default value is 5 seconds.
    scheduled_delay: Option<Duration>,

    /// The maximum queue size to buffer spans for delayed processing. If the
    /// queue gets full it drops the spans. The default value of is 2048.
    #[schemars(default)]
    #[serde(default)]
    max_queue_size: Option<usize>,

    /// The maximum number of spans to process in a single batch. If there are
    /// more than one batch worth of spans then it processes multiple batches
    /// of spans one batch after the other without any delay. The default value
    /// is 512.
    #[schemars(default)]
    #[serde(default)]
    max_export_batch_size: Option<usize>,

    #[serde(deserialize_with = "humantime_serde::deserialize", default)]
    #[schemars(with = "String", default)]
    /// The maximum duration to export a batch of data.
    max_export_timeout: Option<Duration>,

    /// Maximum number of concurrent exports
    ///
    /// Limits the number of spawned tasks for exports and thus memory consumed
    /// by an exporter. A value of 1 will cause exports to be performed
    /// synchronously on the BatchSpanProcessor task.
    #[schemars(default)]
    #[serde(default)]
    max_concurrent_exports: Option<usize>,
}

impl From<BatchProcessorConfig> for BatchConfig {
    fn from(config: BatchProcessorConfig) -> Self {
        let mut default = BatchConfig::default();
        if let Some(scheduled_delay) = config.scheduled_delay {
            default = default.with_scheduled_delay(scheduled_delay);
        }
        if let Some(max_queue_size) = config.max_queue_size {
            default = default.with_max_queue_size(max_queue_size);
        }
        if let Some(max_export_batch_size) = config.max_export_batch_size {
            default = default.with_max_export_batch_size(max_export_batch_size);
        }
        if let Some(max_export_timeout) = config.max_export_timeout {
            default = default.with_max_export_timeout(max_export_timeout);
        }
        if let Some(max_concurrent_exports) = config.max_concurrent_exports {
            default = default.with_max_concurrent_exports(max_concurrent_exports);
        }
        default
    }
}

impl Display for BatchProcessorConfig {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        let batch_config: BatchConfig = self.clone().into();
        let debug_str = format!("{:?}", batch_config);
        // Yes horrible, but there is no other way to get at the actual configured values.
        f.write_str(&debug_str["BatchConfig { ".len()..debug_str.len() - 1])
    }
}
