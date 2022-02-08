//! # Apollo-Telemetry Span Exporter
//!
//! The apollo-telemetry [`SpanExporter`] sends [`Reports`]s to its configured
//! [`Reporter`] instance. By default it will write to the Apollo Ingress.
//!
//! [`SpanExporter`]: super::SpanExporter
//! [`Span`]: crate::trace::Span
//! [`Report`]: apollo_spaceport::report::Report
//! [`Reporter`]: apollo_spaceport::Reporter
//!
//! # Examples
//!
//! ```ignore
//! use crate::apollo_telemetry;
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
use apollo_parser::{ast, Parser};
use apollo_spaceport::report::{ContextualizedStats, QueryLatencyStats, StatsContext};
use apollo_spaceport::{Reporter, ReporterGraph};
use async_trait::async_trait;
use opentelemetry::{
    global,
    runtime::Tokio,
    sdk,
    sdk::export::{
        trace::{ExportResult, SpanData, SpanExporter},
        ExportError,
    },
    trace::{TraceError, TracerProvider},
    Value,
};
use std::borrow::Cow;
use std::collections::HashMap;
use std::fmt::Debug;
use std::str::FromStr;
use std::time::Duration;
use tokio::task::JoinError;

use crate::configuration::{SpaceportConfig, StudioGraph};

pub(crate) const DEFAULT_SERVER_URL: &str = "https://127.0.0.0:50051";
pub(crate) const DEFAULT_LISTEN: &str = "0.0.0.0:50051";

/// Pipeline builder
#[derive(Debug)]
pub struct PipelineBuilder {
    graph_config: Option<StudioGraph>,
    spaceport_config: Option<SpaceportConfig>,
    trace_config: Option<sdk::trace::Config>,
}

/// Create a new apollo telemetry exporter pipeline builder.
pub fn new_pipeline() -> PipelineBuilder {
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

impl PipelineBuilder {
    const DEFAULT_BATCH_SIZE: usize = 65_536;
    const DEFAULT_QUEUE_SIZE: usize = 65_536;

    /// Assign the SDK trace configuration.
    #[allow(dead_code)]
    pub fn with_trace_config(mut self, config: sdk::trace::Config) -> Self {
        self.trace_config = Some(config);
        self
    }

    /// Assign graph identification configuration
    pub fn with_graph_config(mut self, config: &Option<StudioGraph>) -> Self {
        self.graph_config = config.clone();
        self
    }

    /// Assign spaceport reporting configuration
    pub fn with_spaceport_config(mut self, config: &Option<SpaceportConfig>) -> Self {
        self.spaceport_config = config.clone();
        self
    }

    /// Install the apollo telemetry exporter pipeline with the recommended defaults.
    pub fn install_batch(mut self) -> Result<sdk::trace::Tracer, ApolloError> {
        let exporter = self.get_exporter()?;

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

        let tracer = provider.tracer("apollo-opentelemetry", Some(env!("CARGO_PKG_VERSION")));
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
    pub fn install_simple(mut self) -> Result<sdk::trace::Tracer, ApolloError> {
        let exporter = self.get_exporter()?;

        let mut provider_builder =
            sdk::trace::TracerProvider::builder().with_simple_exporter(exporter);
        if let Some(config) = self.trace_config.take() {
            provider_builder = provider_builder.with_config(config);
        }
        let provider = provider_builder.build();

        let tracer = provider.tracer("apollo-opentelemetry", Some(env!("CARGO_PKG_VERSION")));
        let _prev_global_provider = global::set_tracer_provider(provider);

        Ok(tracer)
    }

    /// Create a client to talk to our spaceport and return an exporter.
    pub fn get_exporter(&self) -> Result<Exporter, ApolloError> {
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
pub struct Exporter {
    collector: String,
    graph: Option<StudioGraph>,
    reporter: tokio::sync::OnceCell<Reporter>,
    normalized_queries: HashMap<String, String>,
}

impl Exporter {
    /// Create a new apollo telemetry `Exporter`.
    pub fn new(collector: String, graph: Option<StudioGraph>) -> Self {
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
pub struct ApolloError(#[from] apollo_spaceport::ReporterError);

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

impl From<&StudioGraph> for ReporterGraph {
    fn from(graph: &StudioGraph) -> Self {
        ReporterGraph {
            reference: graph.reference.clone(),
            key: graph.key.clone(),
        }
    }
}

#[async_trait]
impl SpanExporter for Exporter {
    /// Export spans to apollo telemetry
    async fn export(&mut self, batch: Vec<SpanData>) -> ExportResult {
        if self.graph.is_none() {
            // It's an error to try and export statistics without
            // graph details. We enforce that elsewhere in the code
            // and panic here in case a logic bug creeps in elsewhere.
            panic!("cannot export statistics without graph details")
        }
        // In every batch we'll have a varying number of actual stats reports to submit
        // Each report is unique by client name, client version and key (derived from op_name)
        // The dh_map contains a batch specific map, keyed on the unique report triple,
        // referencing DurationHistogram values.
        // After processing the batch, we consume the HashMap and send the generated reports
        // to the Reporter.
        let mut dh_map = HashMap::new();
        /*
         * Process the batch
         */
        for span in batch.iter().filter(|span| span.name == "graphql_request") {
            tracing::debug!(%span.name, ?span.start_time, ?span.end_time);
            tracing::debug!("span: {:?}", span);
            if let Some(query) = span
                .attributes
                .get(&opentelemetry::Key::from_static_str("query"))
            {
                tracing::debug!("query: {}", query);
                let not_found = Value::String(Cow::from("not found"));
                let client_name = span
                    .attributes
                    .get(&opentelemetry::Key::from_static_str("client_name"))
                    .unwrap_or(&not_found)
                    .to_string();
                let client_version = span
                    .attributes
                    .get(&opentelemetry::Key::from_static_str("client_version"))
                    .unwrap_or(&not_found)
                    .to_string();
                let operation_name = span
                    .attributes
                    .get(&opentelemetry::Key::from_static_str("operation_name"));
                // XXX Since normalization is expensive, try to reduce the
                // amount of normalization by doing an exact string match
                // on a query. This might not save a lot of work and may
                // result in too much caching, so re-visit this decision
                // post-integration.
                let key = self
                    .normalized_queries
                    .entry(query.as_str().to_string())
                    .or_insert_with(|| normalize(operation_name, &query.as_str()));

                // Retrieve DurationHistogram from our HashMap, or add a new one
                let dh = dh_map
                    .entry((client_name, client_version, key.clone()))
                    .or_insert_with(|| DurationHistogram::new(None));
                dh.increment_duration(span.end_time.duration_since(span.start_time).unwrap(), 1);
            }
        }

        // Guarantee that the reporter is initialised
        self.reporter
            .get_or_try_init(|| async {
                Reporter::try_new(self.collector.clone())
                    .await
                    .map_err::<ApolloError, _>(Into::into)
            })
            .await?;
        let reporter = self.reporter.get_mut().unwrap();
        // Convert the configuration data into a reportable form
        let graph: ReporterGraph = self.graph.as_ref().unwrap().into();

        // Report our consolidated statistics
        for ((client_name, client_version, key), dh) in dh_map.into_iter() {
            tracing::debug!("reporting entries: {}", dh.entries);
            let stats = ContextualizedStats {
                context: Some(StatsContext {
                    client_name,
                    client_version,
                }),
                query_latency_stats: Some(QueryLatencyStats {
                    latency_count: dh.buckets,
                    request_count: dh.entries,
                    ..Default::default()
                }),
                ..Default::default()
            };

            let msg = reporter
                .submit_stats(graph.clone(), key, stats)
                .await
                .map_err::<TraceError, _>(|e| e.to_string().into())?
                .into_inner()
                .message;
            tracing::trace!("server response: {}", msg);
        }

        Ok(())
    }
}

// Taken from TS implementation
static GRAPHQL_PARSE_FAILURE: &str = "## GraphQLParseFailure\n";
static GRAPHQL_VALIDATION_FAILURE: &str = "## GraphQLValidationFailure\n";
static GRAPHQL_UNKNOWN_OPERATION_NAME: &str = "## GraphQLUnknownOperationName\n";

fn normalize(op: Option<&opentelemetry::Value>, query: &str) -> String {
    // If we don't have an operation name, we can't do anything useful
    // with this query. Just return the appropriate error.
    let op_name: String = match op {
        Some(v) => v.as_str().into_owned(),
        None => {
            tracing::warn!("Could not identify operation name: {}", query);
            return GRAPHQL_UNKNOWN_OPERATION_NAME.to_string();
        }
    };

    let parser = Parser::new(query);
    // compress *before* parsing to modify whitespaces/comments
    let ast = parser.compress().parse();
    tracing::debug!("ast:\n {:?}", ast);
    // If we can't parse the query, we definitely can't normalize it, so
    // just return the appropriate error.
    if ast.errors().len() > 0 {
        tracing::warn!("Could not parse query: {}", query);
        return GRAPHQL_PARSE_FAILURE.to_string();
    }
    let doc = ast.document();
    tracing::trace!("looking for operation: {}", op_name);
    let mut required_definitions: Vec<_> = doc
        .definitions()
        .into_iter()
        .filter(|x| {
            if let ast::Definition::OperationDefinition(op_def) = x {
                return match op_def.name() {
                    Some(v) => v.text() == op_name,
                    None => op_name == "-",
                };
            }
            false
        })
        .collect();
    tracing::debug!("required definitions: {:?}", required_definitions);
    if required_definitions.len() != 1 {
        tracing::warn!("Could not find required single definition: {}", query);
        return GRAPHQL_VALIDATION_FAILURE.to_string();
    }
    let required_definition = required_definitions.pop().unwrap();
    tracing::debug!("required_definition: {:?}", required_definition);
    // XXX Somehow find fragments...
    let def = required_definition.format();
    format!("# {}\n{}", op_name, def)
}

struct DurationHistogram {
    buckets: Vec<i64>,
    entries: u64,
}

impl DurationHistogram {
    const DEFAULT_SIZE: usize = 74; // Taken from TS implementation
    const MAXIMUM_SIZE: usize = 383; // Taken from TS implementation
    const EXPONENT_LOG: f64 = 0.13750352375f64; // log2(1.1) Update when log2() is a const fn (see: https://github.com/rust-lang/rust/issues/57241)

    fn new(init_size: Option<usize>) -> Self {
        Self {
            buckets: vec![0; init_size.unwrap_or(DurationHistogram::DEFAULT_SIZE)],
            entries: 0,
        }
    }

    fn duration_to_bucket(duration: Duration) -> usize {
        // If you use as_micros() here to avoid the divide, tests will fail
        // Because, internally, as_micros() is losing remainders
        let log_duration = f64::log2(duration.as_nanos() as f64 / 1000.0);
        let unbounded_bucket = f64::ceil(log_duration / DurationHistogram::EXPONENT_LOG);

        if unbounded_bucket.is_nan() || unbounded_bucket <= 0f64 {
            return 0;
        } else if unbounded_bucket > DurationHistogram::MAXIMUM_SIZE as f64 {
            return DurationHistogram::MAXIMUM_SIZE;
        }

        unbounded_bucket as usize
    }

    fn increment_duration(&mut self, duration: Duration, value: i64) {
        self.increment_bucket(DurationHistogram::duration_to_bucket(duration), value)
    }

    fn increment_bucket(&mut self, bucket: usize, value: i64) {
        if bucket > DurationHistogram::MAXIMUM_SIZE {
            panic!("bucket is out of bounds of the bucket array");
        }
        self.entries += value as u64;
        if bucket >= self.buckets.len() {
            self.buckets.resize(bucket + 1, 0);
        }
        self.buckets[bucket] += value;
    }
}

#[cfg(test)]
mod test {
    use super::*;

    // DurationHistogram Tests

    impl DurationHistogram {
        fn to_array(&self) -> Vec<i64> {
            let mut result = vec![];
            let mut buffered_zeroes = 0;

            for value in &self.buckets {
                if *value == 0 {
                    buffered_zeroes += 1;
                } else {
                    if buffered_zeroes == 1 {
                        result.push(0);
                    } else if buffered_zeroes != 0 {
                        result.push(0 - buffered_zeroes);
                    }
                    result.push(*value);
                    buffered_zeroes = 0;
                }
            }
            result
        }
    }

    #[test]
    fn it_generates_empty_histogram() {
        let histogram = DurationHistogram::new(None);
        let expected: Vec<i64> = vec![];
        assert_eq!(histogram.to_array(), expected);
    }

    #[test]
    fn it_generates_populated_histogram() {
        let mut histogram = DurationHistogram::new(None);
        histogram.increment_bucket(100, 1);
        assert_eq!(histogram.to_array(), vec![-100, 1]);
        histogram.increment_bucket(102, 1);
        assert_eq!(histogram.to_array(), vec![-100, 1, 0, 1]);
        histogram.increment_bucket(382, 1);
        assert_eq!(histogram.to_array(), vec![-100, 1, 0, 1, -279, 1]);
    }

    #[test]
    fn it_buckets_to_zero_and_one() {
        assert_eq!(
            DurationHistogram::duration_to_bucket(Duration::from_nanos(0)),
            0
        );
        assert_eq!(
            DurationHistogram::duration_to_bucket(Duration::from_nanos(1)),
            0
        );
        assert_eq!(
            DurationHistogram::duration_to_bucket(Duration::from_nanos(999)),
            0
        );
        assert_eq!(
            DurationHistogram::duration_to_bucket(Duration::from_nanos(1000)),
            0
        );
        assert_eq!(
            DurationHistogram::duration_to_bucket(Duration::from_nanos(1001)),
            1
        );
    }

    #[test]
    fn it_buckets_to_one_and_two() {
        assert_eq!(
            DurationHistogram::duration_to_bucket(Duration::from_nanos(1100)),
            1
        );
        assert_eq!(
            DurationHistogram::duration_to_bucket(Duration::from_nanos(1101)),
            2
        );
    }

    #[test]
    fn it_buckets_to_threshold() {
        assert_eq!(
            DurationHistogram::duration_to_bucket(Duration::from_nanos(10000)),
            25
        );
        assert_eq!(
            DurationHistogram::duration_to_bucket(Duration::from_nanos(10834)),
            25
        );
        assert_eq!(
            DurationHistogram::duration_to_bucket(Duration::from_nanos(10835)),
            26
        );
    }

    #[test]
    fn it_buckets_common_times() {
        assert_eq!(
            DurationHistogram::duration_to_bucket(Duration::from_nanos(1e5 as u64)),
            49
        );
        assert_eq!(
            DurationHistogram::duration_to_bucket(Duration::from_nanos(1e6 as u64)),
            73
        );
        assert_eq!(
            DurationHistogram::duration_to_bucket(Duration::from_nanos(1e9 as u64)),
            145
        );
    }

    #[test]
    fn it_limits_to_last_bucket() {
        assert_eq!(
            DurationHistogram::duration_to_bucket(Duration::from_nanos(1e64 as u64)),
            DurationHistogram::MAXIMUM_SIZE
        );
    }
}
