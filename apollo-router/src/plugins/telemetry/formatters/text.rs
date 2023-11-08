use std::collections::BTreeMap;
use std::fmt;

use nu_ansi_term::Color;
use nu_ansi_term::Style;
use opentelemetry::trace::TraceContextExt;
use tracing_core::Event;
use tracing_core::Level;
use tracing_core::Subscriber;
use tracing_opentelemetry::OtelData;
use tracing_subscriber::field;
use tracing_subscriber::field::Visit;
use tracing_subscriber::fmt::format::DefaultVisitor;
use tracing_subscriber::fmt::format::FormatEvent;
use tracing_subscriber::fmt::format::FormatFields;
use tracing_subscriber::fmt::format::Writer;
use tracing_subscriber::fmt::time::FormatTime;
use tracing_subscriber::fmt::time::SystemTime;
use tracing_subscriber::fmt::FmtContext;
use tracing_subscriber::fmt::FormattedFields;
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::registry::SpanRef;

use crate::plugins::telemetry::dynamic_attribute::LogAttributes;
use crate::plugins::telemetry::tracing::APOLLO_PRIVATE_PREFIX;

#[derive(Debug, Clone)]
pub(crate) struct Text {
    pub(crate) timer: SystemTime,
    display_timestamp: bool,
    display_target: bool,
    display_filename: bool,
    display_line_number: bool,
    display_level: bool,
    display_thread_id: bool,
    display_thread_name: bool,
}

impl Default for Text {
    fn default() -> Self {
        Self::new()
    }
}

impl Text {
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
            display_level: false,
            display_line_number: false,
            display_thread_id: false,
            display_thread_name: false,
            display_timestamp: false,
        }
    }

    pub(crate) fn with_target(self, display_target: bool) -> Self {
        Self {
            display_target,
            ..self
        }
    }

    pub(crate) fn with_file(self, display_filename: bool) -> Self {
        Self {
            display_filename,
            ..self
        }
    }

    pub(crate) fn with_level(self, display_level: bool) -> Self {
        Self {
            display_level,
            ..self
        }
    }

    pub(crate) fn with_line_number(self, display_line_number: bool) -> Self {
        Self {
            display_line_number,
            ..self
        }
    }

    pub(crate) fn with_thread_ids(self, display_thread_id: bool) -> Self {
        Self {
            display_thread_id,
            ..self
        }
    }

    pub(crate) fn with_thread_names(self, display_thread_name: bool) -> Self {
        Self {
            display_thread_name,
            ..self
        }
    }

    pub(crate) fn with_timestamp(self, display_timestamp: bool) -> Self {
        Self {
            display_timestamp,
            ..self
        }
    }

    #[inline]
    fn format_level(&self, level: &Level, writer: &mut Writer<'_>) -> fmt::Result {
        if writer.has_ansi_escapes() {
            match *level {
                Level::TRACE => write!(writer, "{}", Color::Purple.paint(Text::TRACE_STR)),
                Level::DEBUG => write!(writer, "{}", Color::Blue.paint(Text::DEBUG_STR)),
                Level::INFO => write!(writer, "{}", Color::Green.paint(Text::INFO_STR)),
                Level::WARN => write!(writer, "{}", Color::Yellow.paint(Text::WARN_STR)),
                Level::ERROR => write!(writer, "{}", Color::Red.paint(Text::ERROR_STR)),
            }?;
        } else {
            match *level {
                Level::TRACE => write!(writer, "{}", Text::TRACE_STR),
                Level::DEBUG => write!(writer, "{}", Text::DEBUG_STR),
                Level::INFO => write!(writer, "{}", Text::INFO_STR),
                Level::WARN => write!(writer, "{}", Text::WARN_STR),
                Level::ERROR => write!(writer, "{}", Text::ERROR_STR),
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
                if self.display_filename && self.display_line_number {
                    write!(writer, ":")?;
                }
                if self.display_line_number {
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
    fn format_attributes<S, N>(
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
        if let Some(mut span) = span {
            Self::write_span(writer, &span)?;
            while let Some(parent) = span.parent() {
                Self::write_span(writer, &parent)?;
                span = parent;
            }
        }

        Ok(())
    }

    fn write_span<S>(writer: &mut Writer, span: &SpanRef<S>) -> fmt::Result
    where
        S: Subscriber + for<'a> LookupSpan<'a>,
    {
        let ext = span.extensions();
        let mut attributes = BTreeMap::new();
        if let Some(data) = ext.get::<FormattedFields<S>>() {
            if let Ok(serde_json::Value::Object(fields)) =
                serde_json::from_str::<serde_json::Value>(data)
            {
                for field in fields
                    .into_iter()
                    .filter(|(key, _)| !key.starts_with(APOLLO_PRIVATE_PREFIX))
                {
                    attributes.insert(field.0, field.1.to_string());
                }
            };
        }

        if let Some(dyn_attributes) = ext.get::<LogAttributes>() {
            for (key, val) in dyn_attributes.attributes() {
                attributes.insert(key.to_string(), val.to_string());
            }
        }

        if let Some(otel_attributes) = ext
            .get::<OtelData>()
            .and_then(|otel_data| otel_data.builder.attributes.as_ref())
        {
            for (key, val) in otel_attributes.iter().filter(|(k, _)| {
                let key_name = k.as_str();
                !key_name.starts_with(APOLLO_PRIVATE_PREFIX)
                    && ![
                        "code.filepath",
                        "code.namespace",
                        "code.lineno",
                        "thread.id",
                        "thread.name",
                    ]
                    .contains(&key_name)
            }) {
                attributes.insert(key.to_string(), val.to_string());
            }
        }
        if !attributes.is_empty() {
            if writer.has_ansi_escapes() {
                let style = Style::new().dimmed();
                write!(writer, "{}", style.prefix())?;
                Self::write_attributes(writer, span.name(), attributes)?;
                write!(writer, "{}", style.suffix())?;
            } else {
                Self::write_attributes(writer, span.name(), attributes)?;
            }
            writer.write_char(' ')?;
        }

        Ok(())
    }

    fn write_attributes(
        writer: &mut Writer,
        span_name: &str,
        attributes: BTreeMap<String, String>,
    ) -> fmt::Result {
        write!(
            writer,
            "{span_name}{{{}}}",
            attributes
                .into_iter()
                .map(|(k, v)| format!("{}={}", k, v))
                .collect::<Vec<String>>()
                .join(",")
        )?;
        Ok(())
    }
}

impl<S, N> FormatEvent<S, N> for Text
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

        if self.display_timestamp {
            self.format_timestamp(&mut writer)?;
        }
        if self.display_level {
            self.format_level(meta.level(), &mut writer)?;
        }

        if self.display_thread_name {
            let current_thread = std::thread::current();
            match current_thread.name() {
                Some(name) => {
                    write!(writer, "{} ", FmtThreadName::new(name))?;
                }
                // fall-back to thread id when name is absent and ids are not enabled
                None if !self.display_thread_id => {
                    write!(writer, "{:0>2?} ", current_thread.id())?;
                }
                _ => {}
            }
        }

        if self.display_thread_id {
            write!(writer, "{:0>2?} ", std::thread::current().id())?;
        }

        self.format_attributes(ctx, &mut writer, event)?;
        if self.display_target {
            self.format_target(meta.target(), &mut writer)?;
        }
        self.format_location(event, &mut writer)?;

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

struct FmtThreadName<'a> {
    name: &'a str,
}

impl<'a> FmtThreadName<'a> {
    pub(crate) fn new(name: &'a str) -> Self {
        Self { name }
    }
}

impl<'a> fmt::Display for FmtThreadName<'a> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        use std::sync::atomic::AtomicUsize;
        use std::sync::atomic::Ordering::AcqRel;
        use std::sync::atomic::Ordering::Acquire;
        use std::sync::atomic::Ordering::Relaxed;

        // Track the longest thread name length we've seen so far in an atomic,
        // so that it can be updated by any thread.
        static MAX_LEN: AtomicUsize = AtomicUsize::new(0);
        let len = self.name.len();
        // Snapshot the current max thread name length.
        let mut max_len = MAX_LEN.load(Relaxed);

        while len > max_len {
            // Try to set a new max length, if it is still the value we took a
            // snapshot of.
            match MAX_LEN.compare_exchange(max_len, len, AcqRel, Acquire) {
                // We successfully set the new max value
                Ok(_) => break,
                // Another thread set a new max value since we last observed
                // it! It's possible that the new length is actually longer than
                // ours, so we'll loop again and check whether our length is
                // still the longest. If not, we'll just use the newer value.
                Err(actual) => max_len = actual,
            }
        }

        // pad thread name using `max_len`
        write!(f, "{:>width$}", self.name, width = max_len)
    }
}
