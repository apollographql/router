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
//! ```no_run
//! use opentelemetry::trace::Tracer;
//! use opentelemetry::sdk::export::trace::stdout;
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
use async_trait::async_trait;
use opentelemetry::{
    global, sdk,
    sdk::export::{
        trace::{ExportResult, SpanData, SpanExporter},
        ExportError,
    },
    trace::TracerProvider,
};
use prost_types::Timestamp;
use std::collections::HashMap;
use std::fmt::Debug;
use tokio::runtime::Runtime;
use usage_agent::report::{Trace, TracesAndStats};
use usage_agent::{report::trace::CachePolicy, Reporter};

/// Pipeline builder
#[derive(Debug)]
pub struct PipelineBuilder {
    trace_config: Option<sdk::trace::Config>,
    rt: Runtime,
    reporter: Reporter,
}

/// Create a new stdout exporter pipeline builder.
pub fn new_pipeline() -> PipelineBuilder {
    PipelineBuilder::default()
}

impl Default for PipelineBuilder {
    /// Return the default pipeline builder.
    fn default() -> Self {
        let rt = Runtime::new().expect("Creating tokio runtime");
        // let handle = rt.handle();
        // let _guard = handle.enter();
        let jh = rt.spawn(async {
            Reporter::try_new("https://127.0.0.1:50051")
                .await
                .map_err::<ApolloError, _>(Into::into)
                .expect("creating reporter")
        });
        tracing::info!("ABOUT TO BLOCK ON");
        let reporter: Reporter = futures::executor::block_on(jh).expect("XXX");
        tracing::info!("AFTER BLOCK ON");
        Self {
            trace_config: None,
            rt,
            reporter,
        }
    }
}

impl PipelineBuilder {
    /// Assign the SDK trace configuration.
    pub fn with_trace_config(mut self, config: sdk::trace::Config) -> Self {
        self.trace_config = Some(config);
        self
    }

    /// Specify the reporter to use.
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
    rt: Runtime,
    reporter: Reporter,
}

impl Exporter {
    /// Create a new stdout `Exporter`.
    pub fn new(rt: Runtime, reporter: Reporter) -> Self {
        /*
        let fut_values = async move {
            println!("ABOUT TO WAIT");
            let res = Reporter::try_new_with_static("https://127.0.0.1:50051")
                .await
                .expect("XXX");
            println!("AFTER WAIT");
            res
        };

        let handle = Handle::current();
        let guard = handle.enter();
        let hdl = handle.spawn(fut_values);
        tracing::info!("ABOUT TO BLOCK ON");
        // let reporter: Reporter = handle.spawn(hdl).expect("XXX");
        tracing::info!("AFTER BLOCK ON");
        drop(guard);
        let hdl = handle.spawn_blocking(|| fut_values);
        // let _ = handle.enter();
        tracing::info!("ABOUT TO BLOCK ON");
        // let current = Handle::current();
        let reporter: Reporter = futures::executor::block_on(hdl).expect("XXX");
        // let reporter: Reporter = current.spawn(fut_values);
        tracing::info!("AFTER BLOCK ON");
        */

        // let rt = Runtime::new().expect("Creating tokio runtime");
        Self { rt, reporter }
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
    /// Export spans to stdout
    async fn export(&mut self, batch: Vec<SpanData>) -> ExportResult {
        /*
         * Break down batch and send to studio
         */
        for span in batch {
            if span.name == "prepare_query" {
                if let Some(q) = span
                    .attributes
                    .get(&opentelemetry::Key::from_static_str("query"))
                {
                    eprintln!("TRACING OUT A QUERY: {}", q);
                    let mut report =
                        usage_agent::Report::try_new("Usage-Agent-uc0sri@current").expect("XXX");
                    let ts_start: Timestamp = span.start_time.into();
                    let ts_end: Timestamp = span.end_time.into();

                    let mut tpq = HashMap::new();

                    let trace = Trace {
                        start_time: Some(ts_start),
                        end_time: Some(ts_end.clone()),
                        cache_policy: Some(CachePolicy {
                            scope: 0,
                            max_age_ns: 0,
                        }),
                        ..Default::default()
                    };
                    let tns = TracesAndStats {
                        trace: vec![trace],
                        ..Default::default()
                    };
                    let hash_q = format!("# {}", q);
                    tpq.insert(hash_q, tns);
                    report.traces_per_query = tpq;
                    report.end_time = Some(ts_end);

                    let msg = self
                        .reporter
                        .submit(report)
                        .await
                        .expect("XXX")
                        .into_inner()
                        .message;
                    tracing::info!("server response: {}", msg);
                }
            }
            /*
            if self.pretty_print {
                self.writer
                    .write_all(format!("{:#?}\n", span).as_bytes())
                    .map_err::<Error, _>(Into::into)?;
            } else {
                self.writer
                    .write_all(format!("{:?}\n", span).as_bytes())
                    .map_err::<Error, _>(Into::into)?;
            }
            */
        }

        Ok(())
    }
}
