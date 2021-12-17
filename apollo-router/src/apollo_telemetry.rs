//! # Stdout Span Exporter
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
    trace::TracerProvider,
};
use prost_types::Timestamp;
use std::collections::HashMap;
use std::fmt::Debug;
use std::io::{stdout, Stdout, Write};
use std::time::{SystemTime, UNIX_EPOCH};
use usage_agent::report::trace::CachePolicy;
use usage_agent::report::{Report, ReportHeader, Trace, TracesAndStats};

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
        let tracer = provider.tracer("opentelemetry", Some(env!("CARGO_PKG_VERSION")));
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
}

impl<W: Write> Exporter<W> {
    /// Create a new stdout `Exporter`.
    pub fn new(writer: W, pretty_print: bool) -> Self {
        Self {
            writer,
            pretty_print,
        }
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
        for span in batch {
            if span.name == "plan" {
                if let Some(q) = span
                    .attributes
                    .get(&opentelemetry::Key::from_static_str("query"))
                {
                    eprintln!("TRACING OUT A QUERY: {}", q);
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

/// Stdout exporter's error
#[derive(thiserror::Error, Debug)]
#[error(transparent)]
struct Error(#[from] std::io::Error);

impl ExportError for Error {
    fn exporter_name(&self) -> &'static str {
        "stdout"
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut reporter = usage_agent::Reporter::try_new("https://127.0.0.1:50051").await?;

    let time = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("Time went backwards");
    let seconds = time.as_secs();
    let nanos = time.as_nanos() - (seconds as u128 * 1_000_000_000);
    let ts = Timestamp {
        seconds: seconds as i64,
        nanos: nanos as i32,
    };
    let mut tpq = HashMap::new();

    let start_time = ts.clone();
    let mut end_time = ts.clone();
    end_time.nanos += 100;
    let trace = Trace {
        start_time: Some(start_time),
        end_time: Some(end_time),
        duration_ns: 100,
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
    tpq.insert(
        "# query ExampleQuery {
  topProducts {
    name
  }
}"
        .to_string(),
        tns,
    );
    println!("tpq: {:?}", tpq);
    let mut report = Report::new();
    report.header = Some(ReportHeader {
        agent_version: "router-0.1.0-alpha-0.2".to_string(),
        graph_ref: "Usage-Agent@current".to_string(),
        ..Default::default()
    });
    report.traces_per_query = tpq;
    report.end_time = Some(ts);

    let response = reporter.submit(report).await?;
    println!("response: {}", response.into_inner().message);

    Ok(())
}
