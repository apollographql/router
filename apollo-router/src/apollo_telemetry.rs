//! # Apollo-Telemetry Span Exporter
//!
//! The apollo-telemetry [`SpanExporter`] sends [`Reports`]s to its configured
//! [`Reporter`] instance. By default it will write to the Apollo Ingress.
//!
//! [`SpanExporter`]: super::SpanExporter
//! [`Span`]: crate::trace::Span
//! [`Report`]: usage_agent::report::Report
//! [`Reporter`]: usage_agent::Reporter
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
use std::fmt::Debug;
use tokio::task::JoinError;
use usage_agent::report::{ContextualizedStats, QueryLatencyStats, StatsContext};
use usage_agent::Reporter;

use crate::configuration::StudioUsage;

/// Pipeline builder
#[derive(Debug)]
pub struct PipelineBuilder {
    studio_config: Option<StudioUsage>,
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
            studio_config: None,
            trace_config: None,
        }
    }
}

impl PipelineBuilder {
    /// Assign the SDK trace configuration.
    #[allow(dead_code)]
    pub fn with_trace_config(mut self, config: sdk::trace::Config) -> Self {
        self.trace_config = Some(config);
        self
    }

    /// Assign studio reporting configuration
    pub fn with_studio_config(mut self, config: &Option<StudioUsage>) -> Self {
        self.studio_config = config.clone();
        self
    }

    /// Install the apollo telemetry exporter pipeline with the recommended defaults.
    #[allow(dead_code)]
    pub fn install_batch(mut self) -> Result<sdk::trace::Tracer, ApolloError> {
        let exporter = self.get_exporter()?;

        let mut provider_builder =
            sdk::trace::TracerProvider::builder().with_batch_exporter(exporter, Tokio);
        if let Some(config) = self.trace_config.take() {
            provider_builder = provider_builder.with_config(config);
        }
        let provider = provider_builder.build();

        let tracer = provider.tracer("apollo-opentelemetry", Some(env!("CARGO_PKG_VERSION")));
        // The call to set_tracer_provider() manipulate a sync RwLock.
        // Even though this code is sync, it is called from within an
        // async context. If we don't do this in a separate thread,
        // it will cause issues with the async runtime that prevents
        // the router from working correctly.
        let _prev_global_provider = std::thread::spawn(|| {
            opentelemetry::global::set_tracer_provider(provider);
        })
        .join();

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

    /// Create a client to talk to our relay and return an exporter.
    pub fn get_exporter(&self) -> Result<Exporter, ApolloError> {
        let collector = match self.studio_config.clone() {
            Some(cfg) => cfg.collector,
            None => "https://127.0.0.1:50051".to_string(),
        };

        tracing::debug!("collector: {}", collector);

        Ok(Exporter::new(collector))
    }
}

/// A [`SpanExporter`] that writes to [`Reporter`].
///
/// [`SpanExporter`]: super::SpanExporter
/// [`Reporter`]: usage_agent::Reporter
#[derive(Debug)]
pub struct Exporter {
    collector: String,
}

impl Exporter {
    /// Create a new apollo telemetry `Exporter`.
    pub fn new(collector: String) -> Self {
        Self { collector }
    }
}

/// Apollo Telemetry exporter's error
#[derive(thiserror::Error, Debug)]
#[error(transparent)]
pub struct ApolloError(#[from] usage_agent::ReporterError);

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
    async fn export(&mut self, batch: Vec<SpanData>) -> ExportResult {
        let mut reporter = Reporter::try_new(self.collector.clone())
            .await
            .map_err::<ApolloError, _>(Into::into)?;
        /*
         * Break down batch and send to studio
         */
        for (index, span) in batch.into_iter().enumerate() {
            // tracing::debug!("index: {}, span: {:?}", index, span);
            tracing::debug!(index, %span.name, ?span.start_time, ?span.end_time);
            if span.name == "graphql_request" {
                tracing::debug!("span: {:?}", span);
                if let Some(q) = span
                    .attributes
                    .get(&opentelemetry::Key::from_static_str("query"))
                {
                    let busy_v = span
                        .end_time
                        .duration_since(span.start_time)
                        .unwrap()
                        .as_micros() as i64;
                    /*
                    let busy = span
                        .attributes
                        .get(&opentelemetry::Key::from_static_str("busy_ns"))
                        .unwrap();
                    let busy_v = match busy {
                        Value::I64(v) => v / 1_000_000,
                        _ => panic!("value should be a signed integer"),
                    };
                    */
                    tracing::debug!("query: {}", q);
                    tracing::debug!("busy: {}", busy_v);

                    let not_found = Value::String(Cow::from("not found"));
                    let stats = ContextualizedStats {
                        context: Some(StatsContext {
                            client_name: span
                                .attributes
                                .get(&opentelemetry::Key::from_static_str("client_name"))
                                .unwrap_or(&not_found)
                                .to_string(),
                            client_version: span
                                .attributes
                                .get(&opentelemetry::Key::from_static_str("client_version"))
                                .unwrap_or(&not_found)
                                .to_string(),
                        }),
                        query_latency_stats: Some(QueryLatencyStats {
                            latency_count: vec![busy_v],
                            request_count: 1,
                            ..Default::default()
                        }),
                        ..Default::default()
                    };
                    let operation_name = span
                        .attributes
                        .get(&opentelemetry::Key::from_static_str("operation_name"));
                    // XXX NEED TO NORMALISE THE QUERY
                    let key = normalize(operation_name, &q.as_str());

                    let msg = reporter
                        .submit_stats(key, stats)
                        .await
                        .map_err::<TraceError, _>(|e| e.to_string().into())?
                        .into_inner()
                        .message;
                    tracing::info!("server response: {}", msg);
                }
            }
        }

        Ok(())
    }
}

fn normalize(op: Option<&opentelemetry::Value>, q: &str) -> String {
    // If we don't have an operation name, no point normalizing
    // it. Just return the unprocessed input.
    let op_name: String = match op {
        Some(v) => v.as_str().into_owned(),
        None => return q.to_string(),
    };
    let parser = Parser::new(q);
    // compress *before* parsing to modify whitespaces/comments
    let ast = parser.compress().parse();
    tracing::debug!("ast:\n {:?}", ast);
    // If we can't parse the query, we definitely can't normalize it, so
    // just return the un-processed input
    if ast.errors().len() > 0 {
        return q.to_string();
    }
    let doc = ast.document();
    tracing::info!("{}", doc.format());
    tracing::info!("looking for operation: {}", op_name);
    let mut required_definitions: Vec<_> = doc
        .definitions()
        .into_iter()
        .filter(|x| {
            if let ast::Definition::OperationDefinition(op_def) = x {
                match op_def.name() {
                    Some(v) => return v.text() == op_name,
                    None => return false,
                }
            }
            false
        })
        .collect();
    tracing::debug!("required definitions: {:?}", required_definitions);
    assert_eq!(required_definitions.len(), 1);
    let required_definition = required_definitions.pop().unwrap();
    tracing::debug!("required_definition: {:?}", required_definition);
    // XXX Somehow find fragments...
    let def = required_definition.format();
    format!("# {} \n{}", op_name, def)
}

#[cfg(test)]
mod test {
    use super::*;
    use std::borrow::Cow;

    // Tests ported from TypeScript implementation in Apollo Server

    #[test]
    // #[tracing_test::traced_test]
    fn basic_test() {
        let q = r#"
{
    user {
        name
    }
}
"#;
        let normalized = normalize(None, q);
        insta::assert_snapshot!(normalized);
    }

    #[test]
    // #[tracing_test::traced_test]
    fn basic_test_with_query() {
        let q = r#"
query {
    user {
        name
    }
}
"#;
        let normalized = normalize(None, q);
        insta::assert_snapshot!(normalized);
    }

    #[test]
    // #[tracing_test::traced_test]
    fn basic_with_operation_name() {
        let q = r#"
query OpName {
    user {
        name
    }
}
"#;
        let op_name = opentelemetry::Value::String(Cow::from("OpName"));
        let normalized = normalize(Some(&op_name), q);
        insta::assert_snapshot!(normalized);
    }

    #[test]
    // #[tracing_test::traced_test]
    fn fragment() {
        let q = r#"
{
  user {
    name
    ...Bar
  }
}
fragment Bar on User {
  asd
}
fragment Baz on User {
  jkl
}
"#;
        let normalized = normalize(None, q);
        insta::assert_snapshot!(normalized);
    }
}
