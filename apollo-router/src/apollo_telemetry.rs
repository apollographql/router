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
    global, sdk,
    sdk::export::{
        trace::{ExportResult, SpanData, SpanExporter},
        ExportError,
    },
    trace::TracerProvider,
    Value,
};
use std::fmt::Debug;
use tokio::runtime::Runtime;
use tokio::time::{sleep, Duration};
use usage_agent::report::{ContextualizedStats, QueryLatencyStats, StatsContext};
use usage_agent::server::ReportServer;
use usage_agent::Reporter;

/// Pipeline builder
#[derive(Debug)]
pub struct PipelineBuilder {
    trace_config: Option<sdk::trace::Config>,
    rt: Runtime,
    reporter: Reporter,
}

/// Create a new apollo telemetry exporter pipeline builder.
pub fn new_pipeline() -> PipelineBuilder {
    PipelineBuilder::default()
}

impl Default for PipelineBuilder {
    /// Return the default pipeline builder.
    fn default() -> Self {
        let rt = Runtime::new().expect("Creating tokio runtime");
        rt.spawn(async {
            // XXX Hard-Code, spawn a server and expect it to succeed

            let report_server =
                ReportServer::new("0.0.0.0:50051".parse().expect("parsing server address"));
            report_server.serve().await.expect("serving reports");
        });

        let jh = rt.spawn(async {
            loop {
                match Reporter::try_new("https://127.0.0.1:50051")
                    .await
                    .map_err::<ApolloError, _>(Into::into)
                {
                    Ok(r) => {
                        tracing::info!("Connected to server, proceeding...");
                        return r;
                    }
                    Err(e) => {
                        tracing::warn!("Could not connect to server({}), re-trying...", e);
                        sleep(Duration::from_millis(50)).await;
                    }
                }
            }
        });
        let reporter: Reporter = futures::executor::block_on(jh).expect("join task");
        Self {
            trace_config: None,
            rt,
            reporter,
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

    /// Specify the reporter to use.
    #[allow(dead_code)]
    pub fn with_reporter(mut self, reporter: Reporter) -> Self {
        self.reporter = reporter;
        self
    }
}

impl PipelineBuilder {
    /// Install the apollo telemetry exporter pipeline with the recommended defaults.
    pub fn install_simple(mut self) -> sdk::trace::Tracer {
        let exporter = Exporter::new(self.rt, self.reporter);

        let mut provider_builder =
            sdk::trace::TracerProvider::builder().with_simple_exporter(exporter);
        if let Some(config) = self.trace_config.take() {
            provider_builder = provider_builder.with_config(config);
        }
        let provider = provider_builder.build();
        let tracer = provider.tracer("apollo-opentelemetry", Some(env!("CARGO_PKG_VERSION")));
        let _ = global::set_tracer_provider(provider);

        tracer
    }
}

/// A [`SpanExporter`] that writes to [`Reporter`].
///
/// [`SpanExporter`]: super::SpanExporter
/// [`Reporter`]: usage_agent::Reporter
#[derive(Debug)]
pub struct Exporter {
    // We have to keep the runtime alive, but we don't use it directly
    _rt: Runtime,
    reporter: Reporter,
}

impl Exporter {
    /// Create a new apollo telemetry `Exporter`.
    pub fn new(rt: Runtime, reporter: Reporter) -> Self {
        Self { _rt: rt, reporter }
    }
}

/// Apollo Telemetry exporter's error
#[derive(thiserror::Error, Debug)]
#[error(transparent)]
struct ApolloError(#[from] usage_agent::ReporterError);

impl ExportError for ApolloError {
    fn exporter_name(&self) -> &'static str {
        "apollo-telemetry"
    }
}

#[async_trait]
impl SpanExporter for Exporter {
    /// Export spans to apollo telemetry
    async fn export(&mut self, batch: Vec<SpanData>) -> ExportResult {
        /*
         * Break down batch and send to studio
         */
        for span in batch {
            if span.name == "prepare_query" {
                tracing::info!("span: {:?}", span);
                if let Some(q) = span
                    .attributes
                    .get(&opentelemetry::Key::from_static_str("query"))
                {
                    let busy = span
                        .attributes
                        .get(&opentelemetry::Key::from_static_str("busy_ns"))
                        .unwrap();
                    let busy_v = match busy {
                        Value::I64(v) => v / 1_000,
                        _ => panic!("value should be a signed integer"),
                    };
                    tracing::info!("query: {}", q);
                    tracing::info!("busy: {}", busy_v);

                    let stats = ContextualizedStats {
                        context: Some(StatsContext {
                            client_name: "client name".to_string(),
                            client_version: "client version".to_string(),
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

                    let msg = self
                        .reporter
                        .submit_stats(key, stats)
                        .await
                        .expect("XXX")
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
    tracing::info!("ast:\n {:?}", ast);
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
    tracing::info!("required definitions: {:?}", required_definitions);
    assert_eq!(required_definitions.len(), 1);
    let required_definition = required_definitions.pop().unwrap();
    tracing::info!("required_definition: {:?}", required_definition);
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
