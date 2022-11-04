use opentelemetry::sdk::export::trace::SpanData;
use opentelemetry::sdk::export::trace::SpanExporter;
use opentelemetry::sdk::trace::Builder;
use opentelemetry::sdk::trace::EvictedHashMap;
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

pub(crate) static APOLLO_PRIVATE_PREFIX: &str = "apollo_private.";

#[derive(Debug)]
struct ApolloFilterSpanExporter<T: SpanExporter> {
    delegate: T,
}

impl<T: SpanExporter> SpanExporter for ApolloFilterSpanExporter<T> {
    fn export<'life0, 'async_trait>(
        &'life0 mut self,
        mut batch: Vec<SpanData>,
    ) -> core::pin::Pin<
        Box<
            dyn core::future::Future<Output = opentelemetry::sdk::export::trace::ExportResult>
                + core::marker::Send
                + 'async_trait,
        >,
    >
    where
        'life0: 'async_trait,
        Self: 'async_trait,
    {
        let batch = batch
            .drain(..)
            .map(|span| {
                if span
                    .attributes
                    .iter()
                    .any(|(key, _)| key.as_str().starts_with(APOLLO_PRIVATE_PREFIX))
                {
                    let attributes_len = span.attributes.len();
                    SpanData {
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
                    }
                } else {
                    span
                }
            })
            .collect();

        self.delegate.export(batch)
    }

    fn shutdown(&mut self) {
        self.delegate.shutdown()
    }
}

trait SpanExporterExt
where
    Self: Sized + SpanExporter,
{
    fn filtered(self) -> ApolloFilterSpanExporter<Self>;
}

impl<T: SpanExporter> SpanExporterExt for T
where
    Self: Sized,
{
    fn filtered(self) -> ApolloFilterSpanExporter<Self> {
        ApolloFilterSpanExporter { delegate: self }
    }
}
