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
use crate::plugins::telemetry::ROUTER_SPAN_NAME;
use apollo_parser::{ast, Parser};
use apollo_spaceport::report::{ContextualizedStats, QueryLatencyStats, StatsContext};
use apollo_spaceport::{Reporter, ReporterGraph};
use async_trait::async_trait;
use derivative::Derivative;
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
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt::Debug;
use std::str::FromStr;
use std::time::Duration;
use tokio::task::JoinError;

const DEFAULT_SERVER_URL: &str = "https://127.0.0.1:50051";

fn default_collector() -> String {
    DEFAULT_SERVER_URL.to_string()
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub struct SpaceportConfig {
    #[serde(default = "default_collector")]
    pub(crate) collector: String,
}

#[derive(Clone, Derivative, Deserialize, Serialize, JsonSchema)]
#[derivative(Debug)]
pub struct StudioGraph {
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

#[allow(dead_code)]
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
    pub fn install_simple(mut self) -> Result<sdk::trace::Tracer, ApolloError> {
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
    pub fn build_exporter(&self) -> Result<Exporter, ApolloError> {
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
        tracing::debug!("Exporting batch {}", batch.len());
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
        for span in batch.iter().filter(|span| span.name == ROUTER_SPAN_NAME) {
            // We can't process a span if we don't have a query
            if let Some(query) = span
                .attributes
                .get(&opentelemetry::Key::from_static_str("query"))
            {
                // Time may wander and if we ever receive a span which we can't
                // process as a duration, then we should just ignore the span and
                // continue processing the rest of the batch
                let elapsed = match span.end_time.duration_since(span.start_time) {
                    Ok(value) => value,
                    Err(_) => continue,
                };

                tracing::trace!(%span.name, %query, ?span.start_time, ?span.end_time);
                let not_found = Value::String("not found".into());
                let anonymous_or_optional_operation_name = Value::String("".into());
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
                    .get(&opentelemetry::Key::from_static_str("operation_name"))
                    .unwrap_or(&anonymous_or_optional_operation_name);

                // XXX Since normalization is expensive, try to reduce the
                // amount of normalization by doing an exact string match
                // on a query. This might not save a lot of work and may
                // result in too much caching, so re-visit this decision
                // post-integration.
                let key = self
                    .normalized_queries
                    .entry(query.as_str().to_string())
                    .or_insert_with(|| stats_report_key(operation_name, &query.as_str()));

                // Retrieve DurationHistogram from our HashMap, or add a new one
                let dh = dh_map
                    .entry((client_name, client_version, key.clone()))
                    .or_insert_with(|| DurationHistogram::new(None));
                dh.increment_duration(elapsed, 1);
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
            tracing::debug!("server response: {}", msg);
        }

        Ok(())
    }
}

// Taken from TS implementation
static GRAPHQL_PARSE_FAILURE: &str = "## GraphQLParseFailure\n";
#[allow(dead_code)]
static GRAPHQL_VALIDATION_FAILURE: &str = "## GraphQLValidationFailure\n";
static GRAPHQL_UNKNOWN_OPERATION_NAME: &str = "## GraphQLUnknownOperationName\n";

fn stats_report_key(op_name: &opentelemetry::Value, query: &str) -> String {
    let mut op_name: String = op_name.as_str().into_owned();

    let parser = Parser::new(query);
    // compress *before* parsing to modify whitespaces/comments
    let ast = parser.compress().parse();
    tracing::debug!("ast:\n {:?}", ast);
    // If we can't parse the query, we definitely can't normalize it, so
    // just return the appropriate error.
    if ast.errors().len() > 0 {
        tracing::warn!("could not parse query: {}", query);
        return GRAPHQL_PARSE_FAILURE.to_string();
    }
    let doc = ast.document();
    // If we haven't specified an out of band name, then return true
    // for every operation definition and update the op_name if
    // we have one.
    // If we do have an out of band name, then check for equality
    // with the operation definition name.
    // If we find more than one match, then in either case we will
    // fail.
    let filter: Box<dyn FnMut(&ast::Definition) -> bool> = if op_name.is_empty() {
        Box::new(|x| {
            if let ast::Definition::OperationDefinition(op_def) = x {
                if let Some(v) = op_def.name() {
                    op_name = v.text().to_string();
                }
                true
            } else {
                false
            }
        })
    } else {
        Box::new(|x| {
            if let ast::Definition::OperationDefinition(op_def) = x {
                match op_def.name() {
                    Some(v) => v.text() == op_name,
                    None => false,
                }
            } else {
                false
            }
        })
    };
    let mut required_definitions: Vec<_> = doc.definitions().into_iter().filter(filter).collect();
    tracing::debug!("required definitions: {:?}", required_definitions);
    if required_definitions.len() != 1 {
        tracing::warn!("could not find required definition: {}", query);
        return GRAPHQL_UNKNOWN_OPERATION_NAME.to_string();
    }
    tracing::debug!(
        "looking for operation: {}",
        if op_name.is_empty() { "-" } else { &op_name }
    );
    let required_definition = required_definitions.pop().unwrap();
    tracing::debug!("required_definition: {:?}", required_definition);

    // In the event of an operation that could be processed without an operation name,
    // the stats key that our ingress expects demands a `-` be returned in that position.
    if op_name.is_empty() {
        op_name = "-".to_string()
    }

    let def = required_definition.format();
    format!("# {}\n{}", op_name, def)
}

struct DurationHistogram {
    buckets: Vec<i64>,
    entries: u64,
}

// The TS implementation of DurationHistogram does Run Length Encoding (RLE)
// to replace sequences of empty buckets with negative numbers. This
// implementation doesn't because:
// Spending too much time in the export() fn exerts back-pressure into the
// telemetry framework and leads to dropped data spans. Given that the
// histogram data is ultimately gzipped for transfer, I wasn't entirely
// sure that this extra processing was worth performing.
impl DurationHistogram {
    const DEFAULT_SIZE: usize = 74; // Taken from TS implementation
    const MAXIMUM_SIZE: usize = 383; // Taken from TS implementation
    const EXPONENT_LOG: f64 = 0.09531017980432493f64; // ln(1.1) Update when ln() is a const fn (see: https://github.com/rust-lang/rust/issues/57241)
    fn new(init_size: Option<usize>) -> Self {
        Self {
            buckets: vec![0; init_size.unwrap_or(DurationHistogram::DEFAULT_SIZE)],
            entries: 0,
        }
    }

    fn duration_to_bucket(duration: Duration) -> usize {
        // If you use as_micros() here to avoid the divide, tests will fail
        // Because, internally, as_micros() is losing remainders
        let log_duration = f64::ln(duration.as_nanos() as f64 / 1000.0);
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

    // stats_report_key() testing

    #[test]
    fn it_handles_no_name() {
        let op_name = Value::String("".into());
        let query = "query ($limit: Int!) {\n  products(limit: $limit) {\n    upc,\n    name,\n    price\n  }\n}";

        let _ = stats_report_key(&op_name, query);
    }

    #[test]
    fn it_handles_default_name() {
        let expected = "# -\nquery ($limit: Int!) { products(limit: $limit) { upc, name, price } }";
        let op_name = Value::String("".into());
        let query = "query ($limit: Int!) {\n  products(limit: $limit) {\n    upc,\n    name,\n    price\n  }\n}";

        let key = stats_report_key(&op_name, query);
        assert_eq!(expected, key);
    }

    #[test]
    fn it_handles_out_of_band_name_and_no_query_name() {
        let expected = GRAPHQL_UNKNOWN_OPERATION_NAME;
        let op_name = Value::String("OneProduct".into());
        let query = "query ($limit: Int!) {\n  products(limit: $limit) {\n    upc,\n    name,\n    price\n  }\n}";

        let key = stats_report_key(&op_name, query);
        assert_eq!(expected, key);
    }

    #[test]
    fn it_handles_query_specified_name() {
        let expected =
            "# OneProduct\nquery OneProduct($limit: Int!) { products(limit: $limit) { upc, name, price } }";
        let op_name = Value::String("".into());
        let query = "query OneProduct($limit: Int!) {\n  products(limit: $limit) {\n    upc,\n    name,\n    price\n  }\n}";

        let key = stats_report_key(&op_name, query);
        assert_eq!(expected, key);
    }

    #[test]
    fn it_handles_same_out_of_band_and_query_specified_name() {
        let expected =
            "# OneProduct\nquery OneProduct($limit: Int!) { products(limit: $limit) { upc, name, price } }";
        let op_name = Value::String("OneProduct".into());
        let query = "query OneProduct($limit: Int!) {\n  products(limit: $limit) {\n    upc,\n    name,\n    price\n  }\n}";

        let key = stats_report_key(&op_name, query);
        assert_eq!(expected, key);
    }

    #[test]
    fn it_handles_same_out_of_band_and_query_specified_name_multiple_queries() {
        let expected = "# OneProduct\nquery OneProduct { __typename } ";
        let op_name = Value::String("OneProduct".into());
        let query = "query OneProduct { __typename } query AnotherProduct { __typename }";

        let key = stats_report_key(&op_name, query);
        assert_eq!(expected, key);
    }

    #[test]
    fn it_handles_missing_out_of_band_and_query_specified_name_multiple_queries() {
        let expected = GRAPHQL_UNKNOWN_OPERATION_NAME;
        let op_name = Value::String("YetAnotherProduct".into());
        let query = "query OneProduct { __typename } query AnotherProduct { __typename }";

        let key = stats_report_key(&op_name, query);
        assert_eq!(expected, key);
    }

    #[test]
    fn it_handles_no_out_of_band_name_and_multiple_queries() {
        let expected = GRAPHQL_UNKNOWN_OPERATION_NAME;
        let op_name = Value::String("".into());
        let query = "query OneProduct { __typename } query AnotherProduct { __typename }";

        let key = stats_report_key(&op_name, query);
        assert_eq!(expected, key);
    }

    //TODO: This test won't work because we aren't doing any validation. I'm leaving
    //the test to remember that at some point it will need to be addressed. Perhaps initially
    //in the router-bridge enhancements.
    #[test]
    #[ignore]
    fn it_handles_invalid_out_of_band_name() {
        let expected = GRAPHQL_VALIDATION_FAILURE;
        let op_name = Value::String("anythingo r missing".into());
        let query = "query OneProduct { __typename } query AnotherProduct { __typename }";

        let key = stats_report_key(&op_name, query);
        assert_eq!(expected, key);
    }
}
