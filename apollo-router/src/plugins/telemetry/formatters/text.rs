#[cfg(test)]
use std::collections::BTreeMap;
#[cfg(not(test))]
use std::collections::HashMap;
use std::collections::HashSet;
use std::fmt;

use nu_ansi_term::Color;
use nu_ansi_term::Style;
use opentelemetry_sdk::Resource;
use serde_json::Value;
use tracing_core::Event;
use tracing_core::Level;
use tracing_core::Subscriber;
use tracing_opentelemetry::OtelData;
use tracing_subscriber::field;
use tracing_subscriber::field::Visit;
use tracing_subscriber::fmt::format::DefaultVisitor;
use tracing_subscriber::fmt::format::Writer;
#[cfg(not(test))]
use tracing_subscriber::fmt::time::FormatTime;
use tracing_subscriber::fmt::time::SystemTime;
use tracing_subscriber::layer::Context;
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::registry::SpanRef;

use super::EventFormatter;
use super::EXCLUDED_ATTRIBUTES;
use crate::plugins::telemetry::config_new::logging::TextFormat;
use crate::plugins::telemetry::dynamic_attribute::LogAttributes;
use crate::plugins::telemetry::formatters::to_map;
use crate::plugins::telemetry::tracing::APOLLO_PRIVATE_PREFIX;

pub(crate) struct Text {
    #[allow(dead_code)]
    timer: SystemTime,
    #[cfg(not(test))]
    resource: HashMap<String, Value>,
    #[cfg(test)]
    resource: BTreeMap<String, Value>,
    config: TextFormat,
    excluded_attributes: HashSet<&'static str>,
}

impl Default for Text {
    fn default() -> Self {
        Self {
            timer: Default::default(),
            resource: Default::default(),
            config: Default::default(),
            excluded_attributes: EXCLUDED_ATTRIBUTES.into(),
        }
    }
}

impl Text {
    const TRACE_STR: &'static str = "TRACE";
    const DEBUG_STR: &'static str = "DEBUG";
    const INFO_STR: &'static str = "INFO";
    const WARN_STR: &'static str = "WARN";
    const ERROR_STR: &'static str = "ERROR";

    pub(crate) fn new(resource: Resource, config: TextFormat) -> Self {
        Self {
            timer: Default::default(),
            config,
            #[cfg(not(test))]
            resource: to_map(resource),
            #[cfg(test)]
            resource: to_map(resource).into_iter().collect(),
            excluded_attributes: EXCLUDED_ATTRIBUTES.into(),
        }
    }

    #[inline]
    fn format_level(&self, level: &Level, writer: &mut Writer<'_>) -> fmt::Result {
        if self.config.ansi_escape_codes {
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
        if self.config.ansi_escape_codes {
            let style = Style::new().dimmed();
            write!(writer, "{}", style.prefix())?;

            // If getting the timestamp failed, don't bail --- only bail on
            // formatting errors.
            #[cfg(not(test))]
            if self.timer.format_time(writer).is_err() {
                writer.write_str("<unknown time>")?;
            }
            #[cfg(test)]
            writer.write_str("[timestamp]")?;

            write!(writer, "{}", style.suffix())?;
        } else {
            // If getting the timestamp failed, don't bail --- only bail on
            // formatting errors.
            #[cfg(not(test))]
            if self.timer.format_time(writer).is_err() {
                writer.write_str("<unknown time>")?;
            }

            #[cfg(test)]
            writer.write_str("[timestamp]")?;
        }
        writer.write_char(' ')
    }

    #[inline]
    fn format_location(&self, event: &Event<'_>, writer: &mut Writer<'_>) -> fmt::Result {
        if let (Some(filename), Some(line)) = (event.metadata().file(), event.metadata().line()) {
            let style = Style::new().dimmed();
            if self.config.ansi_escape_codes {
                write!(writer, "{}", style.prefix())?;
            }
            if self.config.display_filename {
                write!(writer, "{filename}")?;
            }
            if self.config.display_filename && self.config.display_line_number {
                write!(writer, ":")?;
            }
            if self.config.display_line_number {
                write!(writer, "{line}")?;
            }
            if self.config.ansi_escape_codes {
                write!(writer, "{}", style.suffix())?;
            }
            writer.write_char(' ')?;
        }

        Ok(())
    }

    #[inline]
    fn format_target(&self, writer: &mut Writer<'_>, target: &str) -> fmt::Result {
        if self.config.ansi_escape_codes {
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
    fn format_attributes<S>(
        &self,
        ctx: &Context<'_, S>,
        writer: &mut Writer<'_>,
        event: &Event<'_>,
    ) -> fmt::Result
    where
        S: Subscriber + for<'a> LookupSpan<'a>,
    {
        let span = event
            .parent()
            .and_then(|id| ctx.span(id))
            .or_else(|| ctx.lookup_current());
        if let Some(mut span) = span {
            if self.config.display_current_span {
                self.write_span(writer, &span)?;
            }
            if self.config.display_span_list {
                while let Some(parent) = span.parent() {
                    self.write_span(writer, &parent)?;
                    span = parent;
                }
            }
        }

        Ok(())
    }

    fn write_span<S>(&self, writer: &mut Writer, span: &SpanRef<S>) -> fmt::Result
    where
        S: Subscriber + for<'a> LookupSpan<'a>,
    {
        let ext = span.extensions();
        let mut wrote_something = false;

        let style = Style::new().dimmed();
        if self.config.ansi_escape_codes {
            write!(writer, "{}", style.prefix())?;
        }

        if let Some(dyn_attributes) = ext.get::<LogAttributes>() {
            let mut attrs = dyn_attributes
                .attributes()
                .iter()
                .filter(|kv| {
                    let key_name = kv.key.as_str();
                    !key_name.starts_with(APOLLO_PRIVATE_PREFIX)
                        && !self.excluded_attributes.contains(&key_name)
                })
                .peekable();
            if attrs.peek().is_some() {
                wrote_something = true;
                write!(writer, "{}{{", span.name())?;
            }
            #[cfg(test)]
            let attrs: Vec<&opentelemetry::KeyValue> = {
                let mut my_attrs: Vec<&opentelemetry::KeyValue> = attrs.collect();
                my_attrs.sort_by_key(|kv| &kv.key);
                my_attrs
            };
            for kv in attrs {
                let key = &kv.key;
                let value = &kv.value;
                write!(writer, "{key}={value},")?;
            }
        }

        if let Some(otel_attributes) = ext
            .get::<OtelData>()
            .and_then(|otel_data| otel_data.builder.attributes.as_ref())
        {
            let mut attrs = otel_attributes
                .iter()
                .filter(|kv| {
                    let key_name = kv.key.as_str();
                    !key_name.starts_with(APOLLO_PRIVATE_PREFIX)
                        && !self.excluded_attributes.contains(&key_name)
                })
                .peekable();
            if attrs.peek().is_some() && !wrote_something {
                wrote_something = true;
                write!(writer, "{}{{", span.name())?;
            }
            #[cfg(test)]
            let attrs: Vec<&opentelemetry::KeyValue> = {
                let mut my_attrs: Vec<&opentelemetry::KeyValue> = attrs.collect();
                my_attrs.sort_by_key(|kv| &kv.key);
                my_attrs
            };
            for kv in attrs {
                let key = &kv.key;
                let value = &kv.value;
                write!(writer, "{key}={value},")?;
            }
        }

        if wrote_something {
            write!(writer, "}}")?;

            writer.write_char(' ')?;
        }
        if self.config.ansi_escape_codes {
            write!(writer, "{}", style.suffix())?;
        }

        Ok(())
    }

    pub(crate) fn format_resource(
        &self,
        writer: &mut Writer,
        #[cfg(test)] resource: &BTreeMap<String, Value>,
        #[cfg(not(test))] resource: &HashMap<String, Value>,
    ) -> fmt::Result {
        if !resource.is_empty() {
            let style = Style::new().dimmed();
            if self.config.ansi_escape_codes {
                write!(writer, "{}", style.prefix())?;
            }
            let resource_not_empty = !resource.is_empty();

            if resource_not_empty {
                write!(writer, "resource{{")?;
            }

            for (k, v) in resource {
                write!(writer, "{k}={v},")?;
            }

            if resource_not_empty {
                write!(writer, "}}")?;
            }

            if self.config.ansi_escape_codes {
                write!(writer, "{}", style.suffix())?;
            }
            writer.write_char(' ')?;
        }

        Ok(())
    }
}

impl<S> EventFormatter<S> for Text
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    fn format_event<W>(
        &self,
        ctx: &Context<'_, S>,
        writer: &mut W,
        event: &Event<'_>,
    ) -> fmt::Result
    where
        W: std::fmt::Write,
    {
        let meta = event.metadata();
        let mut writer = Writer::new(writer);
        if self.config.display_timestamp {
            self.format_timestamp(&mut writer)?;
        }
        if self.config.display_level {
            self.format_level(meta.level(), &mut writer)?;
        }
        if self.config.display_resource {
            self.format_resource(&mut writer, &self.resource)?;
        }

        if self.config.display_thread_name {
            let current_thread = std::thread::current();
            match current_thread.name() {
                Some(name) => {
                    write!(writer, "{} ", FmtThreadName::new(name))?;
                }
                // fall-back to thread id when name is absent and ids are not enabled
                None if !self.config.display_thread_id => {
                    write!(writer, "{:0>2?} ", current_thread.id())?;
                }
                _ => {}
            }
        }

        if self.config.display_thread_id {
            write!(writer, "{:0>2?} ", std::thread::current().id())?;
        }

        self.format_attributes(ctx, &mut writer, event)?;
        if self.config.display_target {
            self.format_target(&mut writer, meta.target())?;
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
