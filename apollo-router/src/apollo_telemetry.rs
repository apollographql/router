//! # Apollo-Telemetry Span Exporter
//!
//! The stdout [`SpanExporter`] writes debug printed [`Span`]s to its configured
//! [`Write`] instance. By default it will write to [`Stdout`].
//!
//! [`SpanExporter`]: super::SpanExporter
//! [`Span`]: crate::trace::Span
//! [`Write`]: std::io::Write
//! [`Stdout`]: std::io::Stdout
//!
//! # Examples
//!
//! ```no_run
//! use opentelemetry::trace::Tracer;
//! use opentelemetry::sdk::export::trace::stdout;
//! use opentelemetry::global::shutdown_tracer_provider;
//!
//! fn main() {
//!     let tracer = stdout::new_pipeline()
//!         .with_pretty_print(true)
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
    trace::{TraceError, TracerProvider},
};
use prost_types::Timestamp;
use std::collections::HashMap;
use std::fmt::Debug;
use std::io::{stdout, Stdout, Write};
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::runtime::{Handle, Runtime};
use usage_agent::report::{Report, ReportHeader, Trace, TracesAndStats};
use usage_agent::{report::trace::CachePolicy, Reporter};

/// Pipeline builder
#[derive(Debug)]
pub struct PipelineBuilder<W: Write> {
    pretty_print: bool,
    trace_config: Option<sdk::trace::Config>,
    writer: W,
}

/// Create a new stdout exporter pipeline builder.
pub fn new_pipeline() -> PipelineBuilder<Stdout> {
    PipelineBuilder::default()
}

impl Default for PipelineBuilder<Stdout> {
    /// Return the default pipeline builder.
    fn default() -> Self {
        Self {
            pretty_print: false,
            trace_config: None,
            writer: stdout(),
        }
    }
}

impl<W: Write> PipelineBuilder<W> {
    /// Specify the pretty print setting.
    pub fn with_pretty_print(mut self, pretty_print: bool) -> Self {
        self.pretty_print = pretty_print;
        self
    }

    /// Assign the SDK trace configuration.
    pub fn with_trace_config(mut self, config: sdk::trace::Config) -> Self {
        self.trace_config = Some(config);
        self
    }

    /// Specify the writer to use.
    pub fn with_writer<T: Write>(self, writer: T) -> PipelineBuilder<T> {
        PipelineBuilder {
            pretty_print: self.pretty_print,
            trace_config: self.trace_config,
            writer,
        }
    }
}

impl<W> PipelineBuilder<W>
where
    W: Write + Debug + Send + 'static,
{
    /// Install the stdout exporter pipeline with the recommended defaults.
    pub fn install_simple(mut self) -> sdk::trace::Tracer {
        let exporter = Exporter::new(self.writer, self.pretty_print);

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

/// A [`SpanExporter`] that writes to [`Stdout`] or other configured [`Write`].
///
/// [`SpanExporter`]: super::SpanExporter
/// [`Write`]: std::io::Write
/// [`Stdout`]: std::io::Stdout
#[derive(Debug)]
pub struct Exporter<W: Write> {
    writer: W,
    pretty_print: bool,
    runtime: Runtime,
}

impl<W: Write> Exporter<W> {
    /// Create a new stdout `Exporter`.
    pub fn new(writer: W, pretty_print: bool) -> Self {
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

        let rt = Runtime::new().expect("Creating tokio runtime");
        Self {
            writer,
            pretty_print,
            runtime: rt,
        }
    }
}

/// Stdout exporter's error
#[derive(thiserror::Error, Debug)]
#[error(transparent)]
struct ApolloError(#[from] usage_agent::ReporterError);

impl ExportError for ApolloError {
    fn exporter_name(&self) -> &'static str {
        "apollo-telemetry"
    }
}

#[async_trait]
impl<W> SpanExporter for Exporter<W>
where
    W: Write + Debug + Send + 'static,
{
    /// Export spans to stdout
    async fn export(&mut self, batch: Vec<SpanData>) -> ExportResult {
        /*
         * Break down batch and send to studio
         */
        let handle = self.runtime.handle();
        let _guard = handle.enter();
        let mut reporter = Reporter::try_new("https://127.0.0.1:50051")
            .await
            // .map_err(|e| ExportError::from(e))?;
            .map_err::<ApolloError, _>(Into::into)?;
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

                    let msg = reporter
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
