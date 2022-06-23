use crate::plugin::serde::{deserialize_header_name, deserialize_regex};
use crate::plugin::Handler;
use crate::plugins::telemetry::config::MetricsCommon;
use crate::plugins::telemetry::metrics::apollo::Sender;
use crate::services::RouterResponse;
use crate::{http_compat, Context, ResponseBody};
use ::serde::Deserialize;
use bytes::Bytes;
use futures::stream::BoxStream;
use http::header::HeaderName;
use http::HeaderMap;
use opentelemetry::metrics::{Counter, Meter, MeterProvider, Number, ValueRecorder};
use opentelemetry::KeyValue;
use regex::Regex;
use schemars::JsonSchema;
use std::any::Any;
use std::collections::HashMap;
use std::sync::Arc;
use tower::util::BoxService;
use tower::BoxError;

pub(crate) mod apollo;
pub(crate) mod otlp;
pub(crate) mod prometheus;

pub(crate) type MetricsExporterHandle = Box<dyn Any + Send + Sync + 'static>;
pub(crate) type CustomEndpoint =
    BoxService<http_compat::Request<Bytes>, http_compat::Response<ResponseBody>, BoxError>;

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
/// Configuration to add custom attributes/labels on metrics
pub struct MetricsAttributesConf {
    /// Configuration to forward header values or body values from router request/response in metric attributes/labels
    pub(crate) router: Option<AttributesForwardConf>,
    /// Configuration to forward header values or body values from subgraph request/response in metric attributes/labels
    pub(crate) subgraph: Option<SubgraphAttributesConf>,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SubgraphAttributesConf {
    // Apply to all subgraphs
    pub(crate) all: Option<AttributesForwardConf>,
    // Apply to specific subgraph
    pub(crate) subgraphs: HashMap<String, AttributesForwardConf>,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AttributesForwardConf {
    /// Configuration to insert custom attributes/labels in metrics
    #[serde(rename = "static")]
    pub(crate) insert: Option<Vec<Insert>>,
    /// Configuration to forward headers or body values from the request custom attributes/labels in metrics
    pub(crate) request: Option<Vec<Forward>>,
    /// Configuration to forward headers or body values from the response custom attributes/labels in metrics
    pub(crate) response: Option<Vec<Forward>>,
    /// Configuration to forward values from the context custom attributes/labels in metrics
    pub(crate) context: Option<Vec<ContextForward>>,
}

#[derive(Clone, JsonSchema, Deserialize, Debug)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
/// Configuration to insert custom attributes/labels in metrics
pub(crate) struct Insert {
    pub(crate) name: String,
    pub(crate) value: String,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) enum Forward {
    /// Forward header values as custom attributes/labels in metrics
    Header(Vec<HeaderForward>),
    /// Forward body values as custom attributes/labels in metrics
    Body(Vec<BodyForward>),
}

#[derive(Clone, JsonSchema, Deserialize, Debug)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
#[serde(untagged)]
/// Configuration to forward header values in metric labels
pub(crate) enum HeaderForward {
    /// Using a named header
    Named {
        #[schemars(schema_with = "string_schema")]
        #[serde(deserialize_with = "deserialize_header_name")]
        named: HeaderName,
        rename: Option<String>,
        default: Option<String>,
    },
    /// Using a regex on the header name
    Matching {
        #[schemars(schema_with = "string_schema")]
        #[serde(deserialize_with = "deserialize_regex")]
        matching: Regex,
    },
}

#[derive(Clone, JsonSchema, Deserialize, Debug)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
/// Configuration to forward body values in metric attributes/labels
pub(crate) struct BodyForward {
    pub(crate) path: String,
    pub(crate) rename: Option<String>,
    pub(crate) default: Option<String>,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
/// Configuration to forward context values in metric attributes/labels
pub struct ContextForward {
    pub(crate) named: String,
    pub(crate) rename: Option<String>,
    pub(crate) default: Option<String>,
}

impl HeaderForward {
    pub(crate) fn from_headers(&self, headers: &HeaderMap) -> HashMap<String, String> {
        let mut attributes = HashMap::new();
        match self {
            HeaderForward::Named {
                named,
                rename,
                default,
            } => {
                let value = headers.get(named);
                if let Some(value) = value
                    .and_then(|v| v.to_str().ok()?.to_string().into())
                    .or_else(|| default.clone())
                {
                    attributes.insert(rename.clone().unwrap_or_else(|| named.to_string()), value);
                }
            }
            HeaderForward::Matching { matching } => {
                headers
                    .iter()
                    .filter(|(name, _)| matching.is_match(name.as_str()))
                    .for_each(|(name, value)| {
                        if let Ok(value) = value.to_str() {
                            attributes.insert(name.to_string(), value.to_string());
                        }
                    });
            }
        }

        attributes
    }
}

impl AttributesForwardConf {
    pub(crate) fn from_router_response(
        &self,
        response: &RouterResponse<BoxStream<'static, ResponseBody>>,
    ) -> HashMap<String, String> {
        let mut attributes = HashMap::new();

        // Fill from static
        if let Some(to_insert) = &self.insert {
            for Insert { name, value } in to_insert {
                attributes.insert(name.clone(), value.clone());
            }
        }
        let headers = response.response.headers();
        // Fill from response
        if let Some(from_response) = &self.response {
            for from_resp in from_response {
                match from_resp {
                    Forward::Header(header_forward) => attributes.extend(
                        header_forward
                            .iter()
                            .fold(HashMap::new(), |mut acc, current| {
                                acc.extend(current.from_headers(headers));
                                acc
                            }),
                    ),
                    Forward::Body(body_forward) => {
                        // TODO fetch parsed queries
                        // execute it on response.response.body...
                        // If there is something then we push in metric_attrs
                        // If not we skip
                        todo!()
                    }
                }
            }
        }
        // Fill from context
        if let Some(from_context) = &self.context {
            for ContextForward {
                named,
                default,
                rename,
            } in from_context
            {
                match response.context.get::<_, String>(named) {
                    Ok(Some(value)) => {
                        attributes.insert(rename.as_ref().unwrap_or(named).clone(), value);
                    }
                    _ => {
                        if let Some(default_val) = default {
                            attributes.insert(
                                rename.as_ref().unwrap_or(named).clone(),
                                default_val.clone(),
                            );
                        }
                    }
                };
            }
        }

        attributes
    }
    pub(crate) fn get_attributes(
        &self,
        headers: &HeaderMap,
        body: &ResponseBody,
        context: &Context,
    ) -> HashMap<String, String> {
        let mut attributes = HashMap::new();

        // Fill from static
        if let Some(to_insert) = &self.insert {
            for Insert { name, value } in to_insert {
                attributes.insert(name.clone(), value.clone());
            }
        }
        // Fill from response
        if let Some(from_response) = &self.response {
            for from_resp in from_response {
                match from_resp {
                    Forward::Header(header_forward) => attributes.extend(
                        header_forward
                            .iter()
                            .fold(HashMap::new(), |mut acc, current| {
                                acc.extend(current.from_headers(headers));
                                acc
                            }),
                    ),
                    Forward::Body(body_forward) => {
                        // TODO fetch parsed queries
                        // execute it on response.response.body...
                        // If there is something then we push in metric_attrs
                        // If not we skip
                        todo!()
                    }
                }
            }
        }
        // Fill from context
        if let Some(from_context) = &self.context {
            for ContextForward {
                named,
                default,
                rename,
            } in from_context
            {
                match context.get::<_, String>(named) {
                    Ok(Some(value)) => {
                        attributes.insert(rename.as_ref().unwrap_or(named).clone(), value);
                    }
                    _ => {
                        if let Some(default_val) = default {
                            attributes.insert(
                                rename.as_ref().unwrap_or(named).clone(),
                                default_val.clone(),
                            );
                        }
                    }
                };
            }
        }

        attributes
    }
}

impl ContextForward {
    pub(crate) fn from_context(&self, context: &Context) -> HashMap<String, String> {
        let mut attributes = HashMap::new();
        // Fill from context
        match context.get::<_, String>(&self.named) {
            Ok(Some(value)) => {
                attributes.insert(self.rename.as_ref().unwrap_or(&self.named).clone(), value);
            }
            _ => {
                if let Some(default_val) = &self.default {
                    attributes.insert(
                        self.rename.as_ref().unwrap_or(&self.named).clone(),
                        default_val.clone(),
                    );
                }
            }
        };
        attributes
    }
}

fn string_schema(gen: &mut schemars::gen::SchemaGenerator) -> schemars::schema::Schema {
    String::json_schema(gen)
}

#[derive(Default)]
pub(crate) struct MetricsBuilder {
    exporters: Vec<MetricsExporterHandle>,
    meter_providers: Vec<Arc<dyn MeterProvider + Send + Sync + 'static>>,
    custom_endpoints: HashMap<String, Handler>,
    apollo_metrics: Sender,
}

impl MetricsBuilder {
    pub(crate) fn exporters(&mut self) -> Vec<MetricsExporterHandle> {
        std::mem::take(&mut self.exporters)
    }
    pub(crate) fn meter_provider(&mut self) -> AggregateMeterProvider {
        AggregateMeterProvider::new(std::mem::take(&mut self.meter_providers))
    }
    pub(crate) fn custom_endpoints(&mut self) -> HashMap<String, Handler> {
        std::mem::take(&mut self.custom_endpoints)
    }

    pub(crate) fn apollo_metrics_provider(&mut self) -> Sender {
        std::mem::take(&mut self.apollo_metrics)
    }
}

impl MetricsBuilder {
    fn with_exporter<T: Send + Sync + 'static>(mut self, handle: T) -> Self {
        self.exporters.push(Box::new(handle));
        self
    }

    fn with_meter_provider<T: MeterProvider + Send + Sync + 'static>(
        mut self,
        meter_provider: T,
    ) -> Self {
        self.meter_providers.push(Arc::new(meter_provider));
        self
    }

    fn with_custom_endpoint(mut self, path: &str, endpoint: CustomEndpoint) -> Self {
        self.custom_endpoints
            .insert(path.to_string(), Handler::new(endpoint));
        self
    }

    fn with_apollo_metrics_collector(mut self, apollo_metrics: Sender) -> Self {
        self.apollo_metrics = apollo_metrics;
        self
    }
}

pub(crate) trait MetricsConfigurator {
    fn apply(
        &self,
        builder: MetricsBuilder,
        metrics_config: &MetricsCommon,
    ) -> Result<MetricsBuilder, BoxError>;
}

#[derive(Clone)]
pub(crate) struct BasicMetrics {
    pub(crate) http_requests_total: AggregateCounter<u64>,
    pub(crate) http_requests_error_total: AggregateCounter<u64>,
    pub(crate) http_requests_duration: AggregateValueRecorder<f64>,
}

impl BasicMetrics {
    pub(crate) fn new(meter_provider: &AggregateMeterProvider) -> BasicMetrics {
        let meter = meter_provider.meter("apollo/router", None);
        BasicMetrics {
            http_requests_total: meter.build_counter(|m| {
                m.u64_counter("http_requests_total")
                    .with_description("Total number of HTTP requests made.")
                    .init()
            }),
            http_requests_error_total: meter.build_counter(|m| {
                m.u64_counter("http_requests_error_total")
                    .with_description("Total number of HTTP requests in error made.")
                    .init()
            }),
            http_requests_duration: meter.build_value_recorder(|m| {
                m.f64_value_recorder("http_request_duration_seconds")
                    .with_description("Total number of HTTP requests made.")
                    .init()
            }),
        }
    }
}

#[derive(Clone, Default)]
pub(crate) struct AggregateMeterProvider(Vec<Arc<dyn MeterProvider + Send + Sync + 'static>>);
impl AggregateMeterProvider {
    pub(crate) fn new(
        meters: Vec<Arc<dyn MeterProvider + Send + Sync + 'static>>,
    ) -> AggregateMeterProvider {
        AggregateMeterProvider(meters)
    }

    pub(crate) fn meter(
        &self,
        instrumentation_name: &'static str,
        instrumentation_version: Option<&'static str>,
    ) -> AggregateMeter {
        AggregateMeter(
            self.0
                .iter()
                .map(|p| Arc::new(p.meter(instrumentation_name, instrumentation_version)))
                .collect(),
        )
    }
}

#[derive(Clone)]
pub(crate) struct AggregateMeter(Vec<Arc<Meter>>);
impl AggregateMeter {
    pub(crate) fn build_counter<T: Into<Number> + Copy>(
        &self,
        build: fn(&Meter) -> Counter<T>,
    ) -> AggregateCounter<T> {
        AggregateCounter(self.0.iter().map(|m| build(m)).collect())
    }

    pub(crate) fn build_value_recorder<T: Into<Number> + Copy>(
        &self,
        build: fn(&Meter) -> ValueRecorder<T>,
    ) -> AggregateValueRecorder<T> {
        AggregateValueRecorder(self.0.iter().map(|m| build(m)).collect())
    }
}

#[derive(Clone)]
pub(crate) struct AggregateCounter<T: Into<Number> + Copy>(Vec<Counter<T>>);
impl<T> AggregateCounter<T>
where
    T: Into<Number> + Copy,
{
    pub(crate) fn add(&self, value: T, attributes: &[KeyValue]) {
        for counter in &self.0 {
            counter.add(value, attributes)
        }
    }
}

#[derive(Clone)]
pub(crate) struct AggregateValueRecorder<T: Into<Number> + Copy>(Vec<ValueRecorder<T>>);
impl<T> AggregateValueRecorder<T>
where
    T: Into<Number> + Copy,
{
    pub(crate) fn record(&self, value: T, attributes: &[KeyValue]) {
        for value_recorder in &self.0 {
            value_recorder.record(value, attributes)
        }
    }
}
