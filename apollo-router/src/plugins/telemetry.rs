#[cfg(any(feature = "otlp-grpc", feature = "otlp-http"))]
mod otlp;

use crate::apollo_telemetry::SpaceportConfig;
use crate::apollo_telemetry::StudioGraph;
use crate::apollo_telemetry::{new_pipeline, PipelineBuilder};
use crate::configuration::{default_service_name, default_service_namespace};
use crate::layers::opentracing::OpenTracingConfig;
use crate::layers::opentracing::OpenTracingLayer;
use crate::subscriber::{replace_layer, BaseLayer, BoxedLayer};
use apollo_router_core::SubgraphRequest;
use apollo_router_core::SubgraphResponse;
use apollo_router_core::{register_plugin, Plugin};
use apollo_spaceport::server::ReportSpaceport;
use derivative::Derivative;
use futures::Future;
use opentelemetry::sdk::trace::{BatchSpanProcessor, Sampler};
use opentelemetry::sdk::Resource;
use opentelemetry::trace::TracerProvider;
use opentelemetry::{Array, KeyValue, Value};
#[cfg(any(feature = "otlp-grpc", feature = "otlp-http"))]
use otlp::Tracing;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::collections::BTreeMap;
use std::error::Error;
use std::fmt;
use std::net::SocketAddr;
use std::pin::Pin;
use std::str::FromStr;
use tower::util::BoxService;
use tower::Layer;
use tower::{BoxError, ServiceExt};
use url::Url;

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
#[allow(clippy::large_enum_variant)]
pub enum OpenTelemetry {
    Jaeger(Option<Jaeger>),
    #[cfg(any(feature = "otlp-grpc", feature = "otlp-http"))]
    Otlp(otlp::Otlp),
}

#[derive(Debug, Clone, Derivative, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[derivative(Default)]
pub struct Jaeger {
    pub endpoint: Option<JaegerEndpoint>,
    #[serde(default = "default_service_name")]
    #[derivative(Default(value = "default_service_name()"))]
    pub service_name: String,
    #[serde(skip, default = "default_jaeger_username")]
    #[derivative(Default(value = "default_jaeger_username()"))]
    pub username: Option<String>,
    #[serde(skip, default = "default_jaeger_password")]
    #[derivative(Default(value = "default_jaeger_password()"))]
    pub password: Option<String>,
    pub trace_config: Option<TraceConfig>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub enum JaegerEndpoint {
    Agent(SocketAddr),
    Collector(Url),
}

fn default_jaeger_username() -> Option<String> {
    std::env::var("JAEGER_USERNAME").ok()
}

fn default_jaeger_password() -> Option<String> {
    std::env::var("JAEGER_PASSWORD").ok()
}

#[derive(Debug, Default, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TraceConfig {
    #[schemars(schema_with = "option_sampler_schema", default)]
    pub sampler: Option<Sampler>,
    pub max_events_per_span: Option<u32>,
    pub max_attributes_per_span: Option<u32>,
    pub max_links_per_span: Option<u32>,
    pub max_attributes_per_event: Option<u32>,
    pub max_attributes_per_link: Option<u32>,
    pub attributes: Option<BTreeMap<String, AttributeValue>>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(untagged, deny_unknown_fields)]
pub enum AttributeValue {
    /// bool values
    Bool(bool),
    /// i64 values
    I64(i64),
    /// f64 values
    F64(f64),
    /// String values
    String(String),
    /// Array of homogeneous values
    Array(AttributeArray),
}

impl From<AttributeValue> for opentelemetry::Value {
    fn from(value: AttributeValue) -> Self {
        match value {
            AttributeValue::Bool(v) => Value::Bool(v),
            AttributeValue::I64(v) => Value::I64(v),
            AttributeValue::F64(v) => Value::F64(v),
            AttributeValue::String(v) => Value::String(Cow::from(v)),
            AttributeValue::Array(v) => Value::Array(v.into()),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(untagged, deny_unknown_fields)]
pub enum AttributeArray {
    /// Array of bools
    Bool(Vec<bool>),
    /// Array of integers
    I64(Vec<i64>),
    /// Array of floats
    F64(Vec<f64>),
    /// Array of strings
    String(Vec<Cow<'static, str>>),
}

impl From<AttributeArray> for opentelemetry::Array {
    fn from(array: AttributeArray) -> Self {
        match array {
            AttributeArray::Bool(v) => Array::Bool(v),
            AttributeArray::I64(v) => Array::I64(v),
            AttributeArray::F64(v) => Array::F64(v),
            AttributeArray::String(v) => Array::String(v),
        }
    }
}

fn option_sampler_schema(gen: &mut schemars::gen::SchemaGenerator) -> schemars::schema::Schema {
    Option::<SamplerMirror>::json_schema(gen)
}

#[derive(JsonSchema)]
#[allow(dead_code)]
pub enum SamplerMirror {
    /// Always sample the trace
    AlwaysOn,
    /// Never sample the trace
    AlwaysOff,
    /// Respects the parent span's sampling decision or delegates a delegate sampler for root spans.
    /// Not supported via yaml config
    //ParentBased(Box<Sampler>),
    /// Sample a given fraction of traces. Fractions >= 1 will always sample. If the parent span is
    /// sampled, then it's child spans will automatically be sampled. Fractions < 0 are treated as
    /// zero, but spans may still be sampled if their parent is.
    TraceIdRatioBased(f64),
}

impl TraceConfig {
    pub fn trace_config(&self) -> opentelemetry::sdk::trace::Config {
        let mut trace_config = opentelemetry::sdk::trace::config();
        if let Some(sampler) = self.sampler.clone() {
            let sampler: opentelemetry::sdk::trace::Sampler = sampler;
            trace_config = trace_config.with_sampler(sampler);
        }
        if let Some(n) = self.max_events_per_span {
            trace_config = trace_config.with_max_events_per_span(n);
        }
        if let Some(n) = self.max_attributes_per_span {
            trace_config = trace_config.with_max_attributes_per_span(n);
        }
        if let Some(n) = self.max_links_per_span {
            trace_config = trace_config.with_max_links_per_span(n);
        }
        if let Some(n) = self.max_attributes_per_event {
            trace_config = trace_config.with_max_attributes_per_event(n);
        }
        if let Some(n) = self.max_attributes_per_link {
            trace_config = trace_config.with_max_attributes_per_link(n);
        }

        let resource = Resource::new(vec![
            KeyValue::new("service.name", default_service_name()),
            KeyValue::new("service.namespace", default_service_namespace()),
        ])
        .merge(&mut Resource::new(
            self.attributes
                .clone()
                .unwrap_or_default()
                .iter()
                .map(|(k, v)| {
                    KeyValue::new(
                        opentelemetry::Key::from(k.clone()),
                        opentelemetry::Value::from(v.clone()),
                    )
                })
                .collect::<Vec<KeyValue>>(),
        ));

        trace_config = trace_config.with_resource(resource);

        trace_config
    }
}

#[derive(Debug)]
struct ReportingError;

impl fmt::Display for ReportingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ReportingError")
    }
}

impl std::error::Error for ReportingError {}

#[derive(Debug)]
struct Telemetry {
    config: Conf,
    tx: tokio::sync::mpsc::Sender<SpaceportConfig>,
    opentracing_layer: Option<OpenTracingLayer>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
struct Conf {
    pub spaceport: Option<SpaceportConfig>,

    #[serde(skip, default)]
    pub graph: Option<StudioGraph>,

    pub opentelemetry: Option<OpenTelemetry>,

    pub opentracing: Option<OpenTracingConfig>,
}

fn studio_graph() -> Option<StudioGraph> {
    if let Ok(apollo_key) = std::env::var("APOLLO_KEY") {
        let apollo_graph_ref = std::env::var("APOLLO_GRAPH_REF").expect(
            "cannot set up usage reporting if the APOLLO_GRAPH_REF environment variable is not set",
        );

        Some(StudioGraph {
            reference: apollo_graph_ref,
            key: apollo_key,
        })
    } else {
        None
    }
}

#[async_trait::async_trait]
impl Plugin for Telemetry {
    type Config = Conf;

    async fn startup(&mut self) -> Result<(), BoxError> {
        replace_layer(self.try_build_layer()?)?;

        // Only check for notify if we have graph configuration
        if self.config.graph.is_some() {
            self.tx
                .send(self.config.spaceport.clone().unwrap_or_default())
                .await?;
        }
        Ok(())
    }

    async fn shutdown(&mut self) -> Result<(), BoxError> {
        Ok(())
    }

    fn new(mut configuration: Self::Config) -> Result<Self, BoxError> {
        // Graph can only be set via env variables.
        configuration.graph = studio_graph();
        tracing::debug!("Apollo graph configuration: {:?}", configuration.graph);
        // Studio Agent Spaceport listener
        let (tx, mut rx) = tokio::sync::mpsc::channel::<SpaceportConfig>(1);

        tokio::spawn(async move {
            let mut current_listener = "".to_string();
            let mut current_operation: fn(
                msg: String,
            )
                -> Pin<Box<dyn Future<Output = bool> + Send>> = |msg| Box::pin(do_nothing(msg));

            loop {
                tokio::select! {
                    biased;
                    mopt = rx.recv() => {
                        match mopt {
                            Some(msg) => {
                                tracing::debug!(?msg);
                                // Save our target listener for later use
                                current_listener = msg.listener.clone();
                                // Configure which function to call
                                if msg.external {
                                    current_operation = |msg| Box::pin(do_nothing(msg));
                                } else {
                                    current_operation = |msg| Box::pin(do_listen(msg));
                                }
                            },
                            None => break
                        }
                    },
                    x = current_operation(current_listener.clone()) => {
                        // current_operation will only return if there is
                        // something wrong in our configuration. We don't
                        // want to terminate, so wait for a while and
                        // then try again. At some point, re-configuration
                        // will fix this.
                        tracing::debug!(%x, "current_operation");
                        tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
                    }
                };
            }
            tracing::debug!("terminating spaceport loop");
        });

        let mut opentracing_layer = None;
        if let Some(opentracing_conf) = &configuration.opentracing {
            opentracing_layer = OpenTracingLayer::new(opentracing_conf.clone()).into();
        }

        Ok(Telemetry {
            config: configuration,
            tx,
            opentracing_layer,
        })
    }

    fn subgraph_service(
        &mut self,
        _name: &str,
        service: BoxService<SubgraphRequest, SubgraphResponse, BoxError>,
    ) -> BoxService<SubgraphRequest, SubgraphResponse, BoxError> {
        match &self.opentracing_layer {
            Some(opentracing_layer) => opentracing_layer.layer(service).boxed(),
            None => service,
        }
    }
}

impl Telemetry {
    fn try_build_layer(&self) -> Result<BoxedLayer, BoxError> {
        let spaceport_config = &self.config.spaceport;
        let graph_config = &self.config.graph;

        match self.config.opentelemetry.as_ref() {
            Some(OpenTelemetry::Jaeger(config)) => {
                Self::setup_jaeger(spaceport_config, graph_config, config)
            }
            #[cfg(any(feature = "otlp-grpc", feature = "otlp-http"))]
            Some(OpenTelemetry::Otlp(otlp::Otlp {
                tracing: Some(tracing),
            })) => Self::setup_otlp(spaceport_config, graph_config, tracing),
            _ => Self::setup_spaceport(spaceport_config, graph_config),
        }
    }

    fn setup_spaceport(
        spaceport_config: &Option<SpaceportConfig>,
        graph_config: &Option<StudioGraph>,
    ) -> Result<BoxedLayer, BoxError> {
        if graph_config.is_some() {
            // Add spaceport agent as an OT pipeline
            let apollo_exporter =
                Self::apollo_exporter_pipeline(spaceport_config, graph_config).install_batch()?;
            let agent = tracing_opentelemetry::layer().with_tracer(apollo_exporter);
            tracing::debug!("adding agent telemetry");
            Ok(Box::new(agent))
        } else {
            // If we don't have any reporting to do, just put in place our BaseLayer
            // (which does nothing)
            Ok(Box::new(BaseLayer {}))
        }
    }

    fn apollo_exporter_pipeline(
        spaceport_config: &Option<SpaceportConfig>,
        graph_config: &Option<StudioGraph>,
    ) -> PipelineBuilder {
        new_pipeline()
            .with_spaceport_config(spaceport_config)
            .with_graph_config(graph_config)
    }

    #[cfg(any(feature = "otlp-grpc", feature = "otlp-http"))]
    fn setup_otlp(
        spaceport_config: &Option<SpaceportConfig>,
        graph_config: &Option<StudioGraph>,
        config: &Tracing,
    ) -> Result<BoxedLayer, BoxError> {
        let batch_size = std::env::var("OTEL_BSP_MAX_EXPORT_BATCH_SIZE")
            .ok()
            .and_then(|batch_size| usize::from_str(&batch_size).ok());

        let batch = BatchSpanProcessor::builder(
            config.exporter.exporter()?.build_span_exporter()?,
            opentelemetry::runtime::Tokio,
        )
        .with_scheduled_delay(std::time::Duration::from_secs(1));
        let batch = if let Some(size) = batch_size {
            batch.with_max_export_batch_size(size)
        } else {
            batch
        }
        .build();

        let mut builder = opentelemetry::sdk::trace::TracerProvider::builder();
        builder = builder.with_config(
            config
                .trace_config
                .clone()
                .unwrap_or_default()
                .trace_config(),
        );

        // If we have apollo graph configuration, then we can export statistics
        // to the apollo ingress. If we don't, we can't and so no point configuring the
        // exporter.
        if graph_config.is_some() {
            let apollo_exporter =
                Self::apollo_exporter_pipeline(spaceport_config, graph_config).get_exporter()?;
            builder = builder.with_batch_exporter(apollo_exporter, opentelemetry::runtime::Tokio)
        }

        let provider = builder.with_span_processor(batch).build();

        let tracer =
            provider.versioned_tracer("opentelemetry-otlp", Some(env!("CARGO_PKG_VERSION")), None);

        // This code will hang unless we execute from a separate
        // thread.  See:
        // https://github.com/apollographql/router/issues/331
        // https://github.com/open-telemetry/opentelemetry-rust/issues/536
        // for more details and description.
        let jh = tokio::task::spawn_blocking(|| {
            opentelemetry::global::force_flush_tracer_provider();
            opentelemetry::global::set_tracer_provider(provider);
        });
        futures::executor::block_on(jh)?;

        let telemetry = tracing_opentelemetry::layer().with_tracer(tracer);

        opentelemetry::global::set_error_handler(handle_error)?;

        Ok(Box::new(telemetry))
    }

    fn setup_jaeger(
        spaceport_config: &Option<SpaceportConfig>,
        graph_config: &Option<StudioGraph>,
        config: &Option<Jaeger>,
    ) -> Result<BoxedLayer, BoxError> {
        let default_config = Default::default();
        let config = config.as_ref().unwrap_or(&default_config);
        let mut pipeline =
            opentelemetry_jaeger::new_pipeline().with_service_name(&config.service_name);
        match config.endpoint.as_ref() {
            Some(JaegerEndpoint::Agent(address)) => {
                pipeline = pipeline.with_agent_endpoint(address)
            }
            Some(JaegerEndpoint::Collector(url)) => {
                pipeline = pipeline.with_collector_endpoint(url.as_str());

                if let Some(username) = config.username.as_ref() {
                    pipeline = pipeline.with_collector_username(username);
                }
                if let Some(password) = config.password.as_ref() {
                    pipeline = pipeline.with_collector_password(password);
                }
            }
            _ => {}
        }

        let batch_size = std::env::var("OTEL_BSP_MAX_EXPORT_BATCH_SIZE")
            .ok()
            .and_then(|batch_size| usize::from_str(&batch_size).ok());

        let exporter = pipeline.init_async_exporter(opentelemetry::runtime::Tokio)?;

        let batch = BatchSpanProcessor::builder(exporter, opentelemetry::runtime::Tokio)
            .with_scheduled_delay(std::time::Duration::from_secs(1));
        let batch = if let Some(size) = batch_size {
            batch.with_max_export_batch_size(size)
        } else {
            batch
        }
        .build();

        let mut builder = opentelemetry::sdk::trace::TracerProvider::builder();
        if let Some(trace_config) = &config.trace_config {
            builder = builder.with_config(trace_config.trace_config());
        }
        // If we have apollo graph configuration, then we can export statistics
        // to the apollo ingress. If we don't, we can't and so no point configuring the
        // exporter.
        if graph_config.is_some() {
            let apollo_exporter =
                Self::apollo_exporter_pipeline(spaceport_config, graph_config).get_exporter()?;
            builder = builder.with_batch_exporter(apollo_exporter, opentelemetry::runtime::Tokio)
        }

        let provider = builder.with_span_processor(batch).build();

        let tracer = provider.versioned_tracer(
            "opentelemetry-jaeger",
            Some(env!("CARGO_PKG_VERSION")),
            None,
        );

        // This code will hang unless we execute from a separate
        // thread.  See:
        // https://github.com/apollographql/router/issues/331
        // https://github.com/open-telemetry/opentelemetry-rust/issues/536
        // for more details and description.
        let jh = tokio::task::spawn_blocking(|| {
            opentelemetry::global::force_flush_tracer_provider();
            opentelemetry::global::set_tracer_provider(provider);
        });
        futures::executor::block_on(jh)?;

        let telemetry = tracing_opentelemetry::layer().with_tracer(tracer);

        opentelemetry::global::set_error_handler(handle_error)?;

        Ok(Box::new(telemetry))
    }
}

fn handle_error<T: Into<opentelemetry::global::Error>>(err: T) {
    match err.into() {
        opentelemetry::global::Error::Trace(err) => {
            tracing::error!("OpenTelemetry trace error occurred: {}", err)
        }
        opentelemetry::global::Error::Other(err_msg) => {
            tracing::error!("OpenTelemetry error occurred: {}", err_msg)
        }
        other => {
            tracing::error!("OpenTelemetry error occurred: {:?}", other)
        }
    }
}

// For use when we have an external collector. Makes selecting over
// events simpler
async fn do_nothing(_addr_str: String) -> bool {
    loop {
        tokio::time::sleep(tokio::time::Duration::from_secs(3600)).await;
    }
    #[allow(unreachable_code)]
    false
}

// For use when we have an internal collector.
async fn do_listen(addr_str: String) -> bool {
    tracing::debug!("spawning an internal spaceport");
    // Spawn a spaceport server to handle statistics
    let addr = match addr_str.parse() {
        Ok(a) => a,
        Err(e) => {
            tracing::warn!("could not parse spaceport address: {}", e);
            return false;
        }
    };

    let spaceport = ReportSpaceport::new(addr);

    if let Err(e) = spaceport.serve().await {
        match e.source() {
            Some(source) => {
                tracing::warn!("spaceport did not terminate normally: {}", source);
            }
            None => {
                tracing::warn!("spaceport did not terminate normally: {}", e);
            }
        }
        return false;
    }
    true
}

register_plugin!("apollo", "telemetry", Telemetry);

#[cfg(test)]
mod tests {

    #[tokio::test]
    async fn plugin_registered() {
        apollo_router_core::plugins()
            .get("apollo.telemetry")
            .expect("Plugin not found")
            .create_instance(&serde_json::json!({ "opentelemetry": null }))
            .unwrap();
    }

    #[tokio::test]
    #[cfg(any(feature = "otlp-grpc"))]
    async fn attribute_serialization() {
        apollo_router_core::plugins()
            .get("apollo.telemetry")
            .expect("Plugin not found")
            .create_instance(&serde_json::json!({ "opentelemetry": {
                           "otlp": {
                             "tracing": {
                               "exporter": {
                                 "grpc": {
                                   "protocol": "Grpc"
                                 },
                               },
                               "trace_config": {
                                 "attributes": {
                                   "str": "a",
                                   "int": 1,
                                   "float": 1.0,
                                   "bool": true,
                                   "str_arr": ["a", "b"],
                                   "int_arr": [1, 2],
                                   "float_arr": [1.0, 2.0],
                                   "bool_arr": [true, false]
                                }
                            }
                        }
                    }
                }
            }
            ))
            .unwrap();
    }
}
