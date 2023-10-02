use std::collections::HashMap;
use std::time::Duration;

use ::serde::Deserialize;
use access_json::JSONQuery;
use http::header::HeaderName;
use http::response::Parts;
use http::HeaderMap;
use multimap::MultiMap;
use opentelemetry::sdk::metrics::reader::AggregationSelector;
use opentelemetry::sdk::metrics::Aggregation;
use opentelemetry::sdk::metrics::InstrumentKind;
use opentelemetry::sdk::resource::ResourceDetector;
use opentelemetry::sdk::resource::SdkProvidedResourceDetector;
use opentelemetry::sdk::Resource;
use opentelemetry_api::KeyValue;
use regex::Regex;
use schemars::JsonSchema;
use serde::Serialize;
use tower::BoxError;

use crate::error::FetchError;
use crate::graphql;
use crate::graphql::Request;
use crate::plugin::serde::deserialize_header_name;
use crate::plugin::serde::deserialize_json_query;
use crate::plugin::serde::deserialize_regex;
use crate::plugins::telemetry::apollo_exporter::Sender;
use crate::plugins::telemetry::config::AttributeValue;
use crate::plugins::telemetry::config::Conf;
use crate::plugins::telemetry::config::MetricsCommon;
use crate::router_factory::Endpoint;
use crate::Context;
use crate::ListenAddr;

pub(crate) mod apollo;
pub(crate) mod otlp;
pub(crate) mod prometheus;
pub(crate) mod span_metrics_exporter;
static UNKNOWN_SERVICE: &str = "unknown_service";

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
/// Configuration to add custom attributes/labels on metrics
pub(crate) struct MetricsAttributesConf {
    /// Configuration to forward header values or body values from router request/response in metric attributes/labels
    pub(crate) supergraph: Option<AttributesForwardConf>,
    /// Configuration to forward header values or body values from subgraph request/response in metric attributes/labels
    pub(crate) subgraph: Option<SubgraphAttributesConf>,
}

/// Configuration to add custom attributes/labels on metrics to subgraphs
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct SubgraphAttributesConf {
    /// Attributes for all subgraphs
    pub(crate) all: Option<AttributesForwardConf>,
    /// Attributes per subgraph
    pub(crate) subgraphs: Option<HashMap<String, AttributesForwardConf>>,
}

/// Configuration to add custom attributes/labels on metrics to subgraphs
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct AttributesForwardConf {
    /// Configuration to insert custom attributes/labels in metrics
    #[serde(rename = "static")]
    pub(crate) insert: Option<Vec<Insert>>,
    /// Configuration to forward headers or body values from the request to custom attributes/labels in metrics
    pub(crate) request: Option<Forward>,
    /// Configuration to forward headers or body values from the response to custom attributes/labels in metrics
    pub(crate) response: Option<Forward>,
    /// Configuration to forward values from the context to custom attributes/labels in metrics
    pub(crate) context: Option<Vec<ContextForward>>,
    /// Configuration to forward values from the error to custom attributes/labels in metrics
    pub(crate) errors: Option<ErrorsForward>,
}

#[derive(Clone, JsonSchema, Deserialize, Debug)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
/// Configuration to insert custom attributes/labels in metrics
pub(crate) struct Insert {
    /// The name of the attribute to insert
    pub(crate) name: String,
    /// The value of the attribute to insert
    pub(crate) value: AttributeValue,
}

/// Configuration to forward from headers/body
#[derive(Debug, Clone, Deserialize, JsonSchema, Default)]
#[serde(deny_unknown_fields)]
pub(crate) struct Forward {
    /// Forward header values as custom attributes/labels in metrics
    pub(crate) header: Option<Vec<HeaderForward>>,
    /// Forward body values as custom attributes/labels in metrics
    pub(crate) body: Option<Vec<BodyForward>>,
}

#[derive(Debug, Clone, Deserialize, JsonSchema, Default)]
#[serde(deny_unknown_fields, default)]
pub(crate) struct ErrorsForward {
    /// Will include the error message in a "message" attribute
    pub(crate) include_messages: bool,
    /// Forward extensions values as custom attributes/labels in metrics
    pub(crate) extensions: Option<Vec<BodyForward>>,
}

schemar_fn!(
    forward_header_matching,
    String,
    "Using a regex on the header name"
);

#[derive(Clone, JsonSchema, Deserialize, Debug)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
#[serde(untagged)]
/// Configuration to forward header values in metric labels
pub(crate) enum HeaderForward {
    /// Match via header name
    Named {
        /// The name of the header
        #[schemars(with = "String")]
        #[serde(deserialize_with = "deserialize_header_name")]
        named: HeaderName,
        /// The optional output name
        rename: Option<String>,
        /// The optional default value
        default: Option<AttributeValue>,
    },

    /// Match via rgex
    Matching {
        /// Using a regex on the header name
        #[schemars(schema_with = "forward_header_matching")]
        #[serde(deserialize_with = "deserialize_regex")]
        matching: Regex,
    },
}

#[derive(Clone, JsonSchema, Deserialize, Debug)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
/// Configuration to forward body values in metric attributes/labels
pub(crate) struct BodyForward {
    /// The path in the body
    #[schemars(with = "String")]
    #[serde(deserialize_with = "deserialize_json_query")]
    pub(crate) path: JSONQuery,
    /// The name of the attribute
    pub(crate) name: String,
    /// The optional default value
    pub(crate) default: Option<AttributeValue>,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
/// Configuration to forward context values in metric attributes/labels
pub(crate) struct ContextForward {
    /// The name of the value in the context
    pub(crate) named: String,
    /// The optional output name
    pub(crate) rename: Option<String>,
    /// The optional default value
    pub(crate) default: Option<AttributeValue>,
}

impl HeaderForward {
    pub(crate) fn get_attributes_from_headers(
        &self,
        headers: &HeaderMap,
    ) -> HashMap<String, AttributeValue> {
        let mut attributes = HashMap::new();
        match self {
            HeaderForward::Named {
                named,
                rename,
                default,
            } => {
                let value = headers.get(named);
                if let Some(value) = value
                    .and_then(|v| {
                        v.to_str()
                            .ok()
                            .map(|v| AttributeValue::String(v.to_string()))
                    })
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
                            attributes.insert(
                                name.to_string(),
                                AttributeValue::String(value.to_string()),
                            );
                        }
                    });
            }
        }

        attributes
    }
}

impl Forward {
    pub(crate) fn merge(&mut self, to_merge: Self) {
        match (&mut self.body, to_merge.body) {
            (Some(body), Some(body_to_merge)) => {
                body.extend(body_to_merge);
            }
            (None, Some(body_to_merge)) => {
                self.body = Some(body_to_merge);
            }
            _ => {}
        }
        match (&mut self.header, to_merge.header) {
            (Some(header), Some(header_to_merge)) => {
                header.extend(header_to_merge);
            }
            (None, Some(header_to_merge)) => {
                self.header = Some(header_to_merge);
            }
            _ => {}
        }
    }
}

impl ErrorsForward {
    pub(crate) fn merge(&mut self, to_merge: Self) {
        match (&mut self.extensions, to_merge.extensions) {
            (Some(extensions), Some(extensions_to_merge)) => {
                extensions.extend(extensions_to_merge);
            }
            (None, Some(extensions_to_merge)) => {
                self.extensions = Some(extensions_to_merge);
            }
            _ => {}
        }
        self.include_messages = to_merge.include_messages;
    }

    pub(crate) fn get_attributes_from_error(
        &self,
        err: &BoxError,
    ) -> HashMap<String, AttributeValue> {
        let mut attributes = HashMap::new();
        if let Some(fetch_error) = err
            .source()
            .and_then(|e| e.downcast_ref::<FetchError>())
            .or_else(|| err.downcast_ref::<FetchError>())
        {
            let gql_error = fetch_error.to_graphql_error(None);
            // Include error message
            if self.include_messages {
                attributes.insert(
                    "message".to_string(),
                    AttributeValue::String(gql_error.message),
                );
            }
            // Extract data from extensions
            if let Some(extensions_fw) = &self.extensions {
                for ext_fw in extensions_fw {
                    let output = ext_fw.path.execute(&gql_error.extensions).unwrap();
                    if let Some(val) = output {
                        if let Ok(val) = AttributeValue::try_from(val) {
                            attributes.insert(ext_fw.name.clone(), val);
                        }
                    } else if let Some(default_val) = &ext_fw.default {
                        attributes.insert(ext_fw.name.clone(), default_val.clone());
                    }
                }
            }
        } else if self.include_messages {
            attributes.insert(
                "message".to_string(),
                AttributeValue::String(err.to_string()),
            );
        }

        attributes
    }
}

impl AttributesForwardConf {
    pub(crate) fn get_attributes_from_router_response(
        &self,
        parts: &Parts,
        context: &Context,
        first_response: &Option<graphql::Response>,
    ) -> HashMap<String, AttributeValue> {
        let mut attributes = HashMap::new();

        // Fill from static
        if let Some(to_insert) = &self.insert {
            for Insert { name, value } in to_insert {
                attributes.insert(name.clone(), value.clone());
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
                match context.get::<_, AttributeValue>(named) {
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

        // Fill from response
        if let Some(from_response) = &self.response {
            if let Some(header_forward) = &from_response.header {
                attributes.extend(header_forward.iter().fold(
                    HashMap::new(),
                    |mut acc, current| {
                        acc.extend(current.get_attributes_from_headers(&parts.headers));
                        acc
                    },
                ));
            }

            if let Some(body_forward) = &from_response.body {
                if let Some(body) = &first_response {
                    for body_fw in body_forward {
                        let output = body_fw.path.execute(body).unwrap();
                        if let Some(val) = output {
                            if let Ok(val) = AttributeValue::try_from(val) {
                                attributes.insert(body_fw.name.clone(), val);
                            }
                        } else if let Some(default_val) = &body_fw.default {
                            attributes.insert(body_fw.name.clone(), default_val.clone());
                        }
                    }
                }
            }
        }

        attributes
    }

    /// Get attributes from context
    pub(crate) fn get_attributes_from_context(
        &self,
        context: &Context,
    ) -> HashMap<String, AttributeValue> {
        let mut attributes = HashMap::new();

        if let Some(from_context) = &self.context {
            for ContextForward {
                named,
                default,
                rename,
            } in from_context
            {
                match context.get::<_, AttributeValue>(named) {
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

    pub(crate) fn get_attributes_from_response<T: Serialize>(
        &self,
        headers: &HeaderMap,
        body: &T,
    ) -> HashMap<String, AttributeValue> {
        let mut attributes = HashMap::new();

        // Fill from static
        if let Some(to_insert) = &self.insert {
            for Insert { name, value } in to_insert {
                attributes.insert(name.clone(), value.clone());
            }
        }
        // Fill from response
        if let Some(from_response) = &self.response {
            if let Some(headers_forward) = &from_response.header {
                attributes.extend(headers_forward.iter().fold(
                    HashMap::new(),
                    |mut acc, current| {
                        acc.extend(current.get_attributes_from_headers(headers));
                        acc
                    },
                ));
            }
            if let Some(body_forward) = &from_response.body {
                for body_fw in body_forward {
                    let output = body_fw.path.execute(body).unwrap();
                    if let Some(val) = output {
                        if let Ok(val) = AttributeValue::try_from(val) {
                            attributes.insert(body_fw.name.clone(), val);
                        }
                    } else if let Some(default_val) = &body_fw.default {
                        attributes.insert(body_fw.name.clone(), default_val.clone());
                    }
                }
            }
        }

        attributes
    }

    pub(crate) fn get_attributes_from_request(
        &self,
        headers: &HeaderMap,
        body: &Request,
    ) -> HashMap<String, AttributeValue> {
        let mut attributes = HashMap::new();

        // Fill from static
        if let Some(to_insert) = &self.insert {
            for Insert { name, value } in to_insert {
                attributes.insert(name.clone(), value.clone());
            }
        }
        // Fill from response
        if let Some(from_request) = &self.request {
            if let Some(headers_forward) = &from_request.header {
                attributes.extend(headers_forward.iter().fold(
                    HashMap::new(),
                    |mut acc, current| {
                        acc.extend(current.get_attributes_from_headers(headers));
                        acc
                    },
                ));
            }
            if let Some(body_forward) = &from_request.body {
                for body_fw in body_forward {
                    let output = body_fw.path.execute(body).ok().flatten();
                    if let Some(val) = output {
                        if let Ok(val) = AttributeValue::try_from(val) {
                            attributes.insert(body_fw.name.clone(), val);
                        }
                    } else if let Some(default_val) = &body_fw.default {
                        attributes.insert(body_fw.name.clone(), default_val.clone());
                    }
                }
            }
        }

        attributes
    }

    pub(crate) fn get_attributes_from_error(
        &self,
        err: &BoxError,
    ) -> HashMap<String, AttributeValue> {
        self.errors
            .as_ref()
            .map(|e| e.get_attributes_from_error(err))
            .unwrap_or_default()
    }
}

pub(crate) struct MetricsBuilder {
    pub(crate) public_meter_provider_builder: opentelemetry::sdk::metrics::MeterProviderBuilder,
    pub(crate) apollo_meter_provider_builder: opentelemetry::sdk::metrics::MeterProviderBuilder,
    pub(crate) prometheus_meter_provider: Option<opentelemetry::sdk::metrics::MeterProvider>,
    pub(crate) custom_endpoints: MultiMap<ListenAddr, Endpoint>,
    pub(crate) apollo_metrics_sender: Sender,
    pub(crate) resource: Resource,
}

struct ConfigResourceDetector(MetricsCommon);

impl ResourceDetector for ConfigResourceDetector {
    fn detect(&self, _timeout: Duration) -> Resource {
        let mut resource = Resource::new(
            vec![
                self.0.service_name.clone().map(|service_name| {
                    KeyValue::new(
                        opentelemetry_semantic_conventions::resource::SERVICE_NAME,
                        service_name,
                    )
                }),
                self.0.service_namespace.clone().map(|service_namespace| {
                    KeyValue::new(
                        opentelemetry_semantic_conventions::resource::SERVICE_NAMESPACE,
                        service_namespace,
                    )
                }),
            ]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>(),
        );
        resource = resource.merge(&mut Resource::new(
            self.0
                .resources
                .clone()
                .into_iter()
                .map(|(k, v)| KeyValue::new(k, v)),
        ));
        resource
    }
}

impl MetricsBuilder {
    pub(crate) fn new(config: &Conf) -> Self {
        let metrics_common_config = config
            .metrics
            .clone()
            .and_then(|m| m.common)
            .unwrap_or_default();

        let mut resource = Resource::from_detectors(
            Duration::from_secs(0),
            vec![
                Box::new(ConfigResourceDetector(metrics_common_config.clone())),
                Box::new(SdkProvidedResourceDetector),
                Box::new(opentelemetry::sdk::resource::EnvResourceDetector::new()),
            ],
        );

        // Otel resources can be initialized from env variables, there is an override mechanism, but it's broken for service name as it will always override service.name
        // If the service name is set to unknown service then override it from the config
        if resource.get(opentelemetry_semantic_conventions::resource::SERVICE_NAME)
            == Some(UNKNOWN_SERVICE.into())
        {
            if let Some(service_name) = Resource::from_detectors(
                Duration::from_secs(0),
                vec![Box::new(ConfigResourceDetector(
                    metrics_common_config.clone(),
                ))],
            )
            .get(opentelemetry_semantic_conventions::resource::SERVICE_NAME)
            {
                resource = resource.merge(&mut Resource::new(vec![KeyValue::new(
                    opentelemetry_semantic_conventions::resource::SERVICE_NAME,
                    service_name,
                )]));
            }
        }

        Self {
            resource: resource.clone(),
            public_meter_provider_builder: opentelemetry::sdk::metrics::MeterProvider::builder()
                .with_resource(resource.clone()),
            apollo_meter_provider_builder: opentelemetry::sdk::metrics::MeterProvider::builder(),
            prometheus_meter_provider: None,
            custom_endpoints: MultiMap::new(),
            apollo_metrics_sender: Sender::default(),
        }
    }
}

pub(crate) trait MetricsConfigurator {
    fn apply(
        &self,
        builder: MetricsBuilder,
        metrics_config: &MetricsCommon,
    ) -> Result<MetricsBuilder, BoxError>;
}

#[derive(Clone, Default, Debug)]
pub(crate) struct CustomAggregationSelector {
    boundaries: Vec<f64>,
    record_min_max: bool,
}

#[buildstructor::buildstructor]
impl CustomAggregationSelector {
    #[builder]
    pub(crate) fn new(
        boundaries: Vec<f64>,
        record_min_max: Option<bool>,
    ) -> CustomAggregationSelector {
        Self {
            boundaries,
            record_min_max: record_min_max.unwrap_or(true),
        }
    }
}

impl AggregationSelector for CustomAggregationSelector {
    fn aggregation(&self, kind: InstrumentKind) -> Aggregation {
        match kind {
            InstrumentKind::Counter
            | InstrumentKind::UpDownCounter
            | InstrumentKind::ObservableCounter
            | InstrumentKind::ObservableUpDownCounter => Aggregation::Sum,
            InstrumentKind::ObservableGauge => Aggregation::LastValue,
            InstrumentKind::Histogram => Aggregation::ExplicitBucketHistogram {
                boundaries: self.boundaries.clone(),
                record_min_max: self.record_min_max,
            },
        }
    }
}
