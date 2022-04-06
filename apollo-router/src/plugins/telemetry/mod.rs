use crate::plugins::telemetry::config::Trace;
use crate::plugins::telemetry::tracing::apollo;
use crate::subscriber::replace_layer;
use apollo_router_core::{register_plugin, Plugin};
use apollo_spaceport::server::ReportSpaceport;
use futures::FutureExt;
use opentelemetry::propagation::TextMapPropagator;
use opentelemetry::sdk::propagation::{
    BaggagePropagator, TextMapCompositePropagator, TraceContextPropagator,
};
use opentelemetry::sdk::trace::Builder;
use opentelemetry::sdk::Resource;
use opentelemetry::trace::TracerProvider;
use opentelemetry::{global, Array, KeyValue, Value};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::collections::BTreeMap;
use std::error::Error;
use std::fmt;
use tower::BoxError;
use url::Url;

pub mod metrics;
pub mod tracing;

pub struct Telemetry {
    config: config::Conf,
    tracer_provider: Option<opentelemetry::sdk::trace::TracerProvider>,
    spaceport_shutdown: Option<futures::channel::oneshot::Sender<()>>,
}
pub mod config {
    use super::*;

    #[derive(Clone, Default, Debug, Deserialize, JsonSchema)]
    #[serde(deny_unknown_fields, rename_all = "snake_case")]
    pub struct Conf {
        #[allow(dead_code)]
        pub metrics: Option<Metrics>,
        pub tracing: Option<Tracing>,
        pub apollo: Option<tracing::apollo::Config>,
    }

    #[derive(Clone, Default, Debug, Deserialize, JsonSchema)]
    #[serde(deny_unknown_fields, rename_all = "snake_case")]
    #[allow(dead_code)]
    pub struct Metrics {
        pub otlp: Option<metrics::otlp::Config>,
        pub prometheus: Option<metrics::prometheus::Config>,
        pub datadog: Option<metrics::datadog::Config>,
        pub apollo: Option<metrics::apollo::Config>,
    }

    #[derive(Clone, Default, Debug, Deserialize, JsonSchema)]
    #[serde(deny_unknown_fields, rename_all = "snake_case")]
    pub struct Tracing {
        pub propagation: Option<Propagation>,
        pub trace_config: Option<Trace>,
        pub otlp: Option<tracing::otlp::Config>,
        pub jaeger: Option<tracing::jaeger::Config>,
        pub zipkin: Option<tracing::zipkin::Config>,
        pub datadog: Option<tracing::datadog::Config>,
    }

    #[derive(Clone, Default, Debug, Deserialize, JsonSchema)]
    #[serde(deny_unknown_fields, rename_all = "snake_case")]
    pub struct Propagation {
        pub baggage: Option<bool>,
        pub trace_context: Option<bool>,
        pub jaeger: Option<bool>,
        pub datadog: Option<bool>,
        pub zipkin: Option<bool>,
    }

    #[derive(Default, Debug, Clone, Deserialize, Serialize, JsonSchema)]
    #[serde(deny_unknown_fields)]
    pub struct Trace {
        pub service_name: Option<String>,
        pub service_namespace: Option<String>,
        pub sampler: Option<SamplerOption>,
        pub parent_based_sampler: Option<bool>,
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

    #[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
    #[serde(deny_unknown_fields, untagged)]
    pub enum SamplerOption {
        /// Sample a given fraction of traces. Fractions >= 1 will always sample. If the parent span is
        /// sampled, then it's child spans will automatically be sampled. Fractions < 0 are treated as
        /// zero, but spans may still be sampled if their parent is.
        TraceIdRatioBased(f64),
        Always(Sampler),
    }

    #[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
    #[serde(deny_unknown_fields)]
    pub enum Sampler {
        /// Always sample the trace
        AlwaysOn,
        /// Never sample the trace
        AlwaysOff,
    }

    impl From<&Trace> for opentelemetry::sdk::trace::Config {
        fn from(config: &Trace) -> Self {
            let mut trace_config = opentelemetry::sdk::trace::config();

            let sampler = match (&config.sampler, &config.parent_based_sampler) {
                (Some(SamplerOption::Always(Sampler::AlwaysOn)), Some(true)) => {
                    Some(parent_based(opentelemetry::sdk::trace::Sampler::AlwaysOn))
                }
                (Some(SamplerOption::Always(Sampler::AlwaysOff)), Some(true)) => {
                    Some(parent_based(opentelemetry::sdk::trace::Sampler::AlwaysOff))
                }
                (Some(SamplerOption::TraceIdRatioBased(ratio)), Some(true)) => Some(parent_based(
                    opentelemetry::sdk::trace::Sampler::TraceIdRatioBased(*ratio),
                )),
                (Some(SamplerOption::Always(Sampler::AlwaysOn)), _) => {
                    Some(opentelemetry::sdk::trace::Sampler::AlwaysOn)
                }
                (Some(SamplerOption::Always(Sampler::AlwaysOff)), _) => {
                    Some(opentelemetry::sdk::trace::Sampler::AlwaysOff)
                }
                (Some(SamplerOption::TraceIdRatioBased(ratio)), _) => Some(
                    opentelemetry::sdk::trace::Sampler::TraceIdRatioBased(*ratio),
                ),
                (_, _) => None,
            };
            if let Some(sampler) = sampler {
                trace_config = trace_config.with_sampler(sampler);
            }
            if let Some(n) = config.max_events_per_span {
                trace_config = trace_config.with_max_events_per_span(n);
            }
            if let Some(n) = config.max_attributes_per_span {
                trace_config = trace_config.with_max_attributes_per_span(n);
            }
            if let Some(n) = config.max_links_per_span {
                trace_config = trace_config.with_max_links_per_span(n);
            }
            if let Some(n) = config.max_attributes_per_event {
                trace_config = trace_config.with_max_attributes_per_event(n);
            }
            if let Some(n) = config.max_attributes_per_link {
                trace_config = trace_config.with_max_attributes_per_link(n);
            }

            let mut resource_defaults = vec![];
            if let Some(service_name) = &config.service_name {
                resource_defaults.push(KeyValue::new(
                    opentelemetry_semantic_conventions::resource::SERVICE_NAME,
                    service_name.clone(),
                ));
            }
            if let Some(service_namespace) = &config.service_namespace {
                resource_defaults.push(KeyValue::new(
                    opentelemetry_semantic_conventions::resource::SERVICE_NAMESPACE,
                    service_namespace.clone(),
                ));
            }
            let resource = Resource::new(resource_defaults).merge(&mut Resource::new(
                config
                    .attributes
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
}

fn parent_based(sampler: opentelemetry::sdk::trace::Sampler) -> opentelemetry::sdk::trace::Sampler {
    opentelemetry::sdk::trace::Sampler::ParentBased(Box::new(sampler))
}

#[derive(Debug)]
struct ReportingError;

impl fmt::Display for ReportingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ReportingError")
    }
}

impl std::error::Error for ReportingError {}

trait TracingConfigurator {
    fn apply(&self, builder: Builder, trace_config: &Trace) -> Result<Builder, BoxError>;
}

trait GenericWith<T>
where
    Self: Sized,
{
    fn with<B>(self, option: &Option<B>, apply: fn(Self, &B) -> Self) -> Self {
        if let Some(option) = option {
            return apply(self, option);
        }
        self
    }
    fn try_with<B>(
        self,
        option: &Option<B>,
        apply: fn(Self, &B) -> Result<Self, BoxError>,
    ) -> Result<Self, BoxError> {
        if let Some(option) = option {
            return apply(self, option);
        }
        Ok(self)
    }
}

impl<T> GenericWith<T> for T where Self: Sized {}

fn setup<T: TracingConfigurator>(
    mut builder: Builder,
    configurator: &Option<T>,
    tracing_config: &Trace,
) -> Result<Builder, BoxError> {
    if let Some(config) = configurator {
        builder = config.apply(builder, tracing_config)?;
    }
    Ok(builder)
}

fn apollo_key() -> Option<String> {
    std::env::var("APOLLO_KEY").ok()
}

fn apollo_graph_reference() -> Option<String> {
    std::env::var("APOLLO_GRAPH_REF").ok()
}

#[async_trait::async_trait]
impl Plugin for Telemetry {
    type Config = config::Conf;

    async fn startup(&mut self) -> Result<(), BoxError> {
        // Apollo config is special because we enable tracing if some env variables are present.
        let apollo = self.config.apollo.get_or_insert_with(Default::default);
        if apollo.apollo_key.is_none() {
            apollo.apollo_key = apollo_key()
        }
        if apollo.apollo_graph_ref.is_none() {
            apollo.apollo_graph_ref = apollo_graph_reference()
        }

        // If we have key and graph ref but no endpoint we start embedded spaceport
        let (spaceport, shutdown_tx) = match apollo {
            apollo::Config {
                apollo_key: Some(_),
                apollo_graph_ref: Some(_),
                endpoint: None,
            } => {
                let (shutdown_tx, shutdown_rx) = futures::channel::oneshot::channel();
                let report_spaceport = ReportSpaceport::new(
                    "127.0.0.1:0".parse()?,
                    Some(Box::pin(shutdown_rx.map(|_| ()))),
                )
                .await?;
                // Now that the port is known update the config
                apollo.endpoint = Some(Url::parse(&format!(
                    "https://{}",
                    report_spaceport.address()
                ))?);
                (Some(report_spaceport), Some(shutdown_tx))
            }
            _ => (None, None),
        };

        //If store the shutdown handle.
        self.spaceport_shutdown = shutdown_tx;

        // Now that spaceport is set up it is possible to set up the tracer providers.
        self.tracer_provider = Some(Self::create_tracer_provider(&self.config)?);

        // Finally actually start spaceport
        if let Some(spaceport) = spaceport {
            tokio::spawn(async move {
                if let Err(e) = spaceport.serve().await {
                    match e.source() {
                        Some(source) => {
                            ::tracing::warn!("spaceport did not terminate normally: {}", source);
                        }
                        None => {
                            ::tracing::warn!("spaceport did not terminate normally: {}", e);
                        }
                    }
                };
            });
        }
        Ok(())
    }

    fn ready(&mut self) {
        // The active service is about to be swapped in.
        // The rest of this code in this method is expected to succeed.
        // The issue is that Otel uses globals for a bunch of stuff.
        // If we move to a completely tower based architecture then we could make this nicer.
        let tracer_provider = self
            .tracer_provider
            .take()
            .expect("trace_provider will have been set in startup, qed");

        let tracer = tracer_provider.versioned_tracer(
            "apollo-router",
            Some(env!("CARGO_PKG_VERSION")),
            None,
        );

        let telemetry = tracing_opentelemetry::layer().with_tracer(tracer);

        Self::flush_tracer();
        replace_layer(Box::new(telemetry))
            .expect("set_global_subscriber() was not called at startup, fatal");
        opentelemetry::global::set_error_handler(handle_error)
            .expect("otel error handler lock poisoned, fatal");
        global::set_text_map_propagator(Self::create_propagator(&self.config));
    }

    async fn shutdown(&mut self) -> Result<(), BoxError> {
        Self::flush_tracer();
        if let Some(sender) = self.spaceport_shutdown.take() {
            let _ = sender.send(());
        }
        Ok(())
    }

    fn new(config: Self::Config) -> Result<Self, BoxError> {
        Ok(Telemetry {
            spaceport_shutdown: None,
            tracer_provider: None,
            config,
        })
    }
}

impl Telemetry {
    fn create_propagator(config: &config::Conf) -> TextMapCompositePropagator {
        let propagation = config
            .clone()
            .tracing
            .and_then(|c| c.propagation)
            .unwrap_or_default();

        let tracing = config.clone().tracing.unwrap_or_default();

        let mut propagators: Vec<Box<dyn TextMapPropagator + Send + Sync + 'static>> = Vec::new();
        if propagation.baggage.unwrap_or_default() {
            propagators.push(Box::new(BaggagePropagator::default()));
        }
        if propagation.trace_context.unwrap_or_default() || tracing.otlp.is_some() {
            propagators.push(Box::new(TraceContextPropagator::default()));
        }
        if propagation.zipkin.unwrap_or_default() || tracing.zipkin.is_some() {
            propagators.push(Box::new(opentelemetry_zipkin::Propagator::default()));
        }
        if propagation.jaeger.unwrap_or_default() || tracing.jaeger.is_some() {
            propagators.push(Box::new(opentelemetry_jaeger::Propagator::default()));
        }
        if propagation.datadog.unwrap_or_default() || tracing.datadog.is_some() {
            propagators.push(Box::new(opentelemetry_datadog::DatadogPropagator::default()));
        }

        TextMapCompositePropagator::new(propagators)
    }

    fn create_tracer_provider(
        config: &config::Conf,
    ) -> Result<opentelemetry::sdk::trace::TracerProvider, BoxError> {
        let tracing_config = config.tracing.clone().unwrap_or_default();
        let trace_config = &tracing_config.trace_config.unwrap_or_default();
        let mut builder =
            opentelemetry::sdk::trace::TracerProvider::builder().with_config(trace_config.into());

        builder = setup(builder, &tracing_config.jaeger, trace_config)?;
        builder = setup(builder, &tracing_config.zipkin, trace_config)?;
        builder = setup(builder, &tracing_config.datadog, trace_config)?;
        builder = setup(builder, &tracing_config.otlp, trace_config)?;
        builder = setup(builder, &config.apollo, trace_config)?;
        let tracer_provider = builder.build();
        Ok(tracer_provider)
    }

    fn flush_tracer() {
        // This code will hang unless we execute from a separate
        // thread.  See:
        // https://github.com/apollographql/router/issues/331
        // https://github.com/open-telemetry/opentelemetry-rust/issues/536
        // for more details and description.
        let jh = tokio::task::spawn_blocking(|| {
            opentelemetry::global::force_flush_tracer_provider();
        });
        futures::executor::block_on(jh).expect("could not flush previous tracer");
    }
}

fn handle_error<T: Into<opentelemetry::global::Error>>(err: T) {
    match err.into() {
        opentelemetry::global::Error::Trace(err) => {
            ::tracing::error!("OpenTelemetry trace error occurred: {}", err)
        }
        opentelemetry::global::Error::Other(err_msg) => {
            ::tracing::error!("OpenTelemetry error occurred: {}", err_msg)
        }
        other => {
            ::tracing::error!("OpenTelemetry error occurred: {:?}", other)
        }
    }
}

register_plugin!("apollo", "telemetry", Telemetry);

#[cfg(test)]
mod tests {
    #[tokio::test]
    async fn plugin_registered() {
        apollo_router_core::plugins()
            .get("apollo.telemetry")
            .expect("Plugin not found")
            .create_instance(&serde_json::json!({ "tracing": null }))
            .unwrap();
    }

    #[tokio::test]
    async fn attribute_serialization() {
        apollo_router_core::plugins()
            .get("apollo.telemetry")
            .expect("Plugin not found")
            .create_instance(&serde_json::json!({
                "tracing": {

                    "trace_config": {
                        "service_name": "router",
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
            }))
            .unwrap();
    }
}
