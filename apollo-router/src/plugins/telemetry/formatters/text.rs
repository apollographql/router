use std::fmt;

use nu_ansi_term::Color;
use nu_ansi_term::Style;
use opentelemetry::trace::TraceContextExt;
use tracing_core::Event;
use tracing_core::Level;
use tracing_core::Subscriber;
use tracing_subscriber::field;
use tracing_subscriber::field::Visit;
use tracing_subscriber::fmt::format::DefaultVisitor;
use tracing_subscriber::fmt::format::FormatEvent;
use tracing_subscriber::fmt::format::FormatFields;
use tracing_subscriber::fmt::format::Writer;
use tracing_subscriber::fmt::time::FormatTime;
use tracing_subscriber::fmt::time::SystemTime;
use tracing_subscriber::fmt::FmtContext;
use tracing_subscriber::registry::LookupSpan;

use crate::plugins::telemetry::reload::IsSampled;

#[derive(Debug, Clone)]
pub(crate) struct TextFormatter {
    pub(crate) timer: SystemTime,
    display_target: bool,
    display_filename: bool,
    display_line: bool,
}

impl Default for TextFormatter {
    fn default() -> Self {
        Self::new()
    }
}

impl TextFormatter {
    const TRACE_STR: &'static str = "TRACE";
    const DEBUG_STR: &'static str = "DEBUG";
    const INFO_STR: &'static str = "INFO";
    const WARN_STR: &'static str = "WARN";
    const ERROR_STR: &'static str = "ERROR";

    pub(crate) fn new() -> Self {
        Self {
            timer: Default::default(),
            display_target: false,
            display_filename: false,
            display_line: false,
        }
    }

    pub(crate) fn with_target(self, display_target: bool) -> Self {
        Self {
            display_target,
            ..self
        }
    }

    pub(crate) fn with_filename(self, display_filename: bool) -> Self {
        Self {
            display_filename,
            ..self
        }
    }

    pub(crate) fn with_line(self, display_line: bool) -> Self {
        Self {
            display_line,
            ..self
        }
    }

    #[inline]
    fn format_level(&self, level: &Level, writer: &mut Writer<'_>) -> fmt::Result {
        if writer.has_ansi_escapes() {
            match *level {
                Level::TRACE => write!(writer, "{}", Color::Purple.paint(TextFormatter::TRACE_STR)),
                Level::DEBUG => write!(writer, "{}", Color::Blue.paint(TextFormatter::DEBUG_STR)),
                Level::INFO => write!(writer, "{}", Color::Green.paint(TextFormatter::INFO_STR)),
                Level::WARN => write!(writer, "{}", Color::Yellow.paint(TextFormatter::WARN_STR)),
                Level::ERROR => write!(writer, "{}", Color::Red.paint(TextFormatter::ERROR_STR)),
            }?;
        } else {
            match *level {
                Level::TRACE => write!(writer, "{}", TextFormatter::TRACE_STR),
                Level::DEBUG => write!(writer, "{}", TextFormatter::DEBUG_STR),
                Level::INFO => write!(writer, "{}", TextFormatter::INFO_STR),
                Level::WARN => write!(writer, "{}", TextFormatter::WARN_STR),
                Level::ERROR => write!(writer, "{}", TextFormatter::ERROR_STR),
            }?;
        }
        writer.write_char(' ')
    }

    #[inline]
    fn format_timestamp(&self, writer: &mut Writer<'_>) -> fmt::Result {
        if writer.has_ansi_escapes() {
            let style = Style::new().dimmed();
            write!(writer, "{}", style.prefix())?;

            // If getting the timestamp failed, don't bail --- only bail on
            // formatting errors.
            if self.timer.format_time(writer).is_err() {
                writer.write_str("<unknown time>")?;
            }

            write!(writer, "{}", style.suffix())?;
        } else {
            // If getting the timestamp failed, don't bail --- only bail on
            // formatting errors.
            if self.timer.format_time(writer).is_err() {
                writer.write_str("<unknown time>")?;
            }
        }
        writer.write_char(' ')
    }

    #[inline]
    fn format_location(&self, event: &Event<'_>, writer: &mut Writer<'_>) -> fmt::Result {
        if let (Some(filename), Some(line)) = (event.metadata().file(), event.metadata().line()) {
            if writer.has_ansi_escapes() {
                let style = Style::new().dimmed();
                write!(writer, "{}", style.prefix())?;
                if self.display_filename {
                    write!(writer, "{filename}")?;
                }
                if self.display_filename && self.display_line {
                    write!(writer, ":")?;
                }
                if self.display_line {
                    write!(writer, "{line}")?;
                }
                write!(writer, "{}", style.suffix())?;
            } else {
                write!(writer, "{filename}:{line}")?;
            }
            writer.write_char(' ')?;
        }

        Ok(())
    }

    #[inline]
    fn format_target(&self, target: &str, writer: &mut Writer<'_>) -> fmt::Result {
        if writer.has_ansi_escapes() {
            let style = Style::new().dimmed();
            write!(writer, "{}", style.prefix())?;
            write!(writer, "{target}:")?;
            write!(writer, "{}", style.suffix())?;
        } else {
            write!(writer, "{target}:")?;
        }
        writer.write_char(' ')
    }

    #[inline]
    fn format_request_id<S, N>(
        &self,
        ctx: &FmtContext<'_, S, N>,
        writer: &mut Writer<'_>,
        event: &Event<'_>,
    ) -> fmt::Result
    where
        S: Subscriber + for<'a> LookupSpan<'a>,
        N: for<'a> FormatFields<'a> + 'static,
    {
        let span = event
            .parent()
            .and_then(|id| ctx.span(id))
            .or_else(|| ctx.lookup_current());
        if let Some(span) = span {
            let ext = span.extensions();
            match &ext.get::<tracing_opentelemetry::OtelData>() {
                Some(otel_data) => {
                    let trace_id = otel_data
                        .builder
                        .trace_id
                        .unwrap_or_else(|| otel_data.parent_cx.span().span_context().trace_id());

                    if writer.has_ansi_escapes() {
                        let style = Style::new().dimmed();
                        write!(writer, "{}", style.prefix())?;
                        write!(writer, "[trace_id={trace_id}]")?;
                        write!(writer, "{}", style.suffix())?;
                    } else {
                        write!(writer, "[trace_id={trace_id}]")?;
                    }
                    writer.write_char(' ')?;
                }
                None => {
                    if span.is_sampled() {
                        eprintln!("Unable to find OtelData in extensions; this is a bug");
                    }
                }
            }
        }

        Ok(())
    }
}

impl<S, N> FormatEvent<S, N> for TextFormatter
where
    S: Subscriber + for<'a> LookupSpan<'a>,
    N: for<'a> FormatFields<'a> + 'static,
{
    fn format_event(
        &self,
        ctx: &FmtContext<'_, S, N>,
        mut writer: Writer<'_>,
        event: &Event<'_>,
    ) -> fmt::Result {
        let meta = event.metadata();
        self.format_timestamp(&mut writer)?;
        self.format_location(event, &mut writer)?;

        self.format_level(meta.level(), &mut writer)?;
        self.format_request_id(ctx, &mut writer, event)?;
        if self.display_target {
            self.format_target(meta.target(), &mut writer)?;
        }
        let mut visitor = CustomVisitor::new(DefaultVisitor::new(writer.by_ref(), true));
        event.record(&mut visitor);

        writeln!(writer)
    }
}

struct CustomVisitor<N>(N);

impl<N> CustomVisitor<N>
where
    N: field::Visit,
{
    #[allow(dead_code)]
    fn new(inner: N) -> Self {
        Self(inner)
    }
}

// TODO we are now able to filter fields here, for now it's just a passthrough
impl<N> Visit for CustomVisitor<N>
where
    N: Visit,
{
    fn record_debug(&mut self, field: &tracing_core::Field, value: &dyn fmt::Debug) {
        self.0.record_debug(field, value)
    }

    fn record_str(&mut self, field: &tracing_core::Field, value: &str) {
        self.0.record_str(field, value)
    }

    fn record_error(
        &mut self,
        field: &tracing_core::Field,
        value: &(dyn std::error::Error + 'static),
    ) {
        self.0.record_error(field, value)
    }

    fn record_f64(&mut self, field: &tracing_core::Field, value: f64) {
        self.0.record_f64(field, value)
    }

    fn record_i64(&mut self, field: &tracing_core::Field, value: i64) {
        self.0.record_i64(field, value)
    }

    fn record_u64(&mut self, field: &tracing_core::Field, value: u64) {
        self.0.record_u64(field, value)
    }

    fn record_bool(&mut self, field: &tracing_core::Field, value: bool) {
        self.0.record_bool(field, value)
    }
}
