//! # Apollo-Telemetry Span Exporter
//!
//! The apollo-telemetry [`SpanExporter`] sends [`Report`]s to its configured
//! [`Reporter`] instance. By default it will write to the Apollo Ingress.
//!
//! [`SpanExporter`]: SpanExporter
//! [`Span`]: crate::trace::Span
//! [`Report`]: apollo_spaceport::report::Report
//! [`Reporter`]: apollo_spaceport::Reporter
//!
//! # Examples
//!
//! ```ignore
//! use apollo_router::apollo_telemetry;
//! use opentelemetry::trace::Tracer;
//! use opentelemetry::global::shutdown_tracer_provider;
//!
//! fn main() {
//!     let tracer = apollo_telemetry::new_pipeline()
//!         .install_simple();
//!
//!     tracer.in_span("doing_work", |cx| {
//!         // Traced app logic here...
//!     });
//!
//!     shutdown_tracer_provider(); // sending remaining spans
//! }
//! ```
use std::collections::HashMap;
use std::fmt::Debug;
use std::str::FromStr;

use apollo_spaceport::Reporter;
use async_trait::async_trait;
use derivative::Derivative;
use opentelemetry::global;
use opentelemetry::runtime::Tokio;
use opentelemetry::sdk;
use opentelemetry::sdk::export::trace::ExportResult;
use opentelemetry::sdk::export::trace::SpanData;
use opentelemetry::sdk::export::trace::SpanExporter;
use opentelemetry::sdk::export::ExportError;
use opentelemetry::trace::TracerProvider;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use tokio::task::JoinError;

const DEFAULT_SERVER_URL: &str = "https://127.0.0.1:50051";

pub(crate) fn default_collector() -> String {
    DEFAULT_SERVER_URL.to_string()
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub(crate) struct SpaceportConfig {
    #[serde(default = "default_collector")]
    pub(crate) collector: String,
}

#[allow(dead_code)]
#[derive(Clone, Derivative, Deserialize, Serialize, JsonSchema)]
#[derivative(Debug)]
pub(crate) struct StudioGraph {
    #[serde(skip, default = "apollo_graph_reference")]
    pub(crate) reference: String,

    #[serde(skip, default = "apollo_key")]
    #[derivative(Debug = "ignore")]
    pub(crate) key: String,
}

fn apollo_key() -> String {
    std::env::var("APOLLO_KEY")
        .expect("cannot set up usage reporting if the APOLLO_KEY environment variable is not set")
}

fn apollo_graph_reference() -> String {
    std::env::var("APOLLO_GRAPH_REF").expect(
        "cannot set up usage reporting if the APOLLO_GRAPH_REF environment variable is not set",
    )
}

impl Default for SpaceportConfig {
    fn default() -> Self {
        Self {
            collector: default_collector(),
        }
    }
}
/// Pipeline builder
#[derive(Debug)]
pub(crate) struct PipelineBuilder {
    graph_config: Option<StudioGraph>,
    spaceport_config: Option<SpaceportConfig>,
    trace_config: Option<sdk::trace::Config>,
}

/// Create a new apollo telemetry exporter pipeline builder.
pub(crate) fn new_pipeline() -> PipelineBuilder {
    PipelineBuilder::default()
}

impl Default for PipelineBuilder {
    /// Return the default pipeline builder.
    fn default() -> Self {
        Self {
            graph_config: None,
            spaceport_config: None,
            trace_config: None,
        }
    }
}

#[allow(dead_code)]
impl PipelineBuilder {
    const DEFAULT_BATCH_SIZE: usize = 65_536;
    const DEFAULT_QUEUE_SIZE: usize = 65_536;

    /// Assign the SDK trace configuration.
    #[allow(dead_code)]
    pub(crate) fn with_trace_config(mut self, config: sdk::trace::Config) -> Self {
        self.trace_config = Some(config);
        self
    }

    /// Assign graph identification configuration
    pub(crate) fn with_graph_config(mut self, config: &Option<StudioGraph>) -> Self {
        self.graph_config = config.clone();
        self
    }

    /// Assign spaceport reporting configuration
    pub(crate) fn with_spaceport_config(mut self, config: &Option<SpaceportConfig>) -> Self {
        self.spaceport_config = config.clone();
        self
    }

    /// Install the apollo telemetry exporter pipeline with the recommended defaults.
    pub(crate) fn install_batch(mut self) -> Result<sdk::trace::Tracer, ApolloError> {
        let exporter = self.build_exporter()?;

        // Users can override the default batch and queue sizes, but they can't
        // set them to be lower than our specified defaults;
        let queue_size = match std::env::var("OTEL_BSP_MAX_QUEUE_SIZE")
            .ok()
            .and_then(|queue_size| usize::from_str(&queue_size).ok())
        {
            Some(v) => {
                let result = usize::max(PipelineBuilder::DEFAULT_QUEUE_SIZE, v);
                if result > v {
                    tracing::warn!(
                        "Ignoring 'OTEL_BSP_MAX_QUEUE_SIZE' setting. Cannot set max queue size lower than {}",
                        PipelineBuilder::DEFAULT_QUEUE_SIZE
                    );
                }
                result
            }
            None => PipelineBuilder::DEFAULT_QUEUE_SIZE,
        };
        let batch_size = match std::env::var("OTEL_BSP_MAX_EXPORT_BATCH_SIZE")
            .ok()
            .and_then(|batch_size| usize::from_str(&batch_size).ok())
        {
            Some(v) => {
                let result = usize::max(PipelineBuilder::DEFAULT_BATCH_SIZE, v);
                if result > v {
                    tracing::warn!(
                        "Ignoring 'OTEL_BSP_MAX_EXPORT_BATCH_SIZE' setting. Cannot set max export batch size lower than {}",
                        PipelineBuilder::DEFAULT_BATCH_SIZE
                    );
                }
                // Batch size must be <= queue size
                if result > queue_size {
                    tracing::warn!(
                        "Clamping 'OTEL_BSP_MAX_EXPORT_BATCH_SIZE' setting to {}. Cannot set max export batch size greater than max queue size",
                        queue_size
                    );
                    queue_size
                } else {
                    result
                }
            }
            None => PipelineBuilder::DEFAULT_BATCH_SIZE,
        };
        let batch = sdk::trace::BatchSpanProcessor::builder(exporter, Tokio)
            .with_max_queue_size(queue_size)
            .with_max_export_batch_size(batch_size)
            .build();

        let mut provider_builder = sdk::trace::TracerProvider::builder().with_span_processor(batch);
        if let Some(config) = self.trace_config.take() {
            provider_builder = provider_builder.with_config(config);
        }
        let provider = provider_builder.build();

        let tracer = provider.versioned_tracer(
            "apollo-opentelemetry",
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

        Ok(tracer)
    }

    // XXX CANNOT USE SIMPLE WITH OUR IMPLEMENTATION AS NO RUNTIME EXISTS
    // WHEN TRYING TO EXPORT...
    /// Install the apollo telemetry exporter pipeline with the recommended defaults.
    #[allow(dead_code)]
    pub(crate) fn install_simple(mut self) -> Result<sdk::trace::Tracer, ApolloError> {
        let exporter = self.build_exporter()?;

        let mut provider_builder =
            sdk::trace::TracerProvider::builder().with_simple_exporter(exporter);
        if let Some(config) = self.trace_config.take() {
            provider_builder = provider_builder.with_config(config);
        }
        let provider = provider_builder.build();

        let tracer = provider.versioned_tracer(
            "apollo-opentelemetry",
            Some(env!("CARGO_PKG_VERSION")),
            None,
        );
        let _prev_global_provider = global::set_tracer_provider(provider);

        Ok(tracer)
    }

    /// Create a client to talk to our spaceport and return an exporter.
    pub(crate) fn build_exporter(&self) -> Result<Exporter, ApolloError> {
        let collector = match self.spaceport_config.clone() {
            Some(cfg) => cfg.collector,
            None => DEFAULT_SERVER_URL.to_string(),
        };
        let graph = self.graph_config.clone();

        tracing::debug!("collector: {}", collector);
        tracing::debug!("graph: {:?}", graph);

        Ok(Exporter::new(collector, graph))
    }
}

/// A [`SpanExporter`] that writes to [`Reporter`].
///
/// [`SpanExporter`]: super::SpanExporter
/// [`Reporter`]: apollo_spaceport::Reporter
#[derive(Debug)]
#[allow(dead_code)]
pub(crate) struct Exporter {
    collector: String,
    graph: Option<StudioGraph>,
    reporter: tokio::sync::OnceCell<Reporter>,
    normalized_queries: HashMap<String, String>,
}

impl Exporter {
    /// Create a new apollo telemetry `Exporter`.
    pub(crate) fn new(collector: String, graph: Option<StudioGraph>) -> Self {
        Self {
            collector,
            graph,
            reporter: tokio::sync::OnceCell::new(),
            normalized_queries: HashMap::new(),
        }
    }
}

/// Apollo Telemetry exporter's error
#[derive(thiserror::Error, Debug)]
#[error(transparent)]
pub(crate) struct ApolloError(#[from] apollo_spaceport::ReporterError);

impl From<std::io::Error> for ApolloError {
    fn from(error: std::io::Error) -> Self {
        ApolloError(error.into())
    }
}

impl From<JoinError> for ApolloError {
    fn from(error: JoinError) -> Self {
        ApolloError(error.into())
    }
}

impl ExportError for ApolloError {
    fn exporter_name(&self) -> &'static str {
        "apollo-telemetry"
    }
}

#[async_trait]
impl SpanExporter for Exporter {
    /// Export spans to apollo telemetry
    async fn export(&mut self, _batch: Vec<SpanData>) -> ExportResult {
        todo!("Apollo tracing is not yet implemented");
        //return ExportResult::Ok(());
    }
}
