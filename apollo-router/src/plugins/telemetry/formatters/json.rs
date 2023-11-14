use std::collections::HashMap;
use std::fmt;
use std::fmt::Write;
use std::io;

use opentelemetry::sdk::Resource;
use opentelemetry_api::Array;
use opentelemetry_api::Value;
use serde::ser::SerializeMap;
use serde::ser::Serializer as _;
use serde_json::Serializer;
use tracing::span::Record;
use tracing_core::Event;
use tracing_core::Field;
use tracing_core::Subscriber;
use tracing_opentelemetry::OtelData;
use tracing_serde::AsSerde;
use tracing_subscriber::field;
use tracing_subscriber::field::VisitOutput;
use tracing_subscriber::fmt::format::Writer;
use tracing_subscriber::fmt::FormatFields;
use tracing_subscriber::fmt::FormattedFields;
use tracing_subscriber::layer::Context;
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::registry::SpanRef;

use super::EventFormatter;
use super::APOLLO_PRIVATE_PREFIX;
use super::EXCLUDED_ATTRIBUTES;
use crate::plugins::telemetry::config_new::logging::JsonFormat;
use crate::plugins::telemetry::dynamic_attribute::LogAttributes;
use crate::plugins::telemetry::formatters::to_map;

#[derive(Debug, Default)]
pub(crate) struct Json {
    config: JsonFormat,
    resource: HashMap<String, serde_json::Value>,
}

impl Json {
    pub(crate) fn new(resource: Resource, config: JsonFormat) -> Self {
        Self {
            resource: to_map(resource),
            config,
        }
    }
}

struct SerializableContext<'a, Span>(Option<SpanRef<'a, Span>>)
where
    Span: Subscriber + for<'lookup> tracing_subscriber::registry::LookupSpan<'lookup>;

impl<'a, Span> serde::ser::Serialize for SerializableContext<'a, Span>
where
    Span: Subscriber + for<'lookup> tracing_subscriber::registry::LookupSpan<'lookup>,
{
    fn serialize<Ser>(&self, serializer_o: Ser) -> Result<Ser::Ok, Ser::Error>
    where
        Ser: serde::ser::Serializer,
    {
        use serde::ser::SerializeSeq;
        let mut serializer = serializer_o.serialize_seq(None)?;

        if let Some(leaf_span) = &self.0 {
            for span in leaf_span.scope().from_root() {
                serializer.serialize_element(&SerializableSpan(&span))?;
            }
        }

        serializer.end()
    }
}

struct SerializableSpan<'a, 'b, Span>(&'b tracing_subscriber::registry::SpanRef<'a, Span>)
where
    Span: for<'lookup> tracing_subscriber::registry::LookupSpan<'lookup>;

impl<'a, 'b, Span> serde::ser::Serialize for SerializableSpan<'a, 'b, Span>
where
    Span: for<'lookup> tracing_subscriber::registry::LookupSpan<'lookup>,
{
    fn serialize<Ser>(&self, serializer: Ser) -> Result<Ser::Ok, Ser::Error>
    where
        Ser: serde::ser::Serializer,
    {
        let mut serializer = serializer.serialize_map(None)?;

        let ext = self.0.extensions();

        // Get otel attributes
        let otel_attributes = ext
            .get::<OtelData>()
            .and_then(|otel_data| otel_data.builder.attributes.as_ref());
        if let Some(otel_attributes) = otel_attributes {
            for (key, value) in otel_attributes.iter().filter(|(k, _)| {
                let key_name = k.as_str();
                !key_name.starts_with(APOLLO_PRIVATE_PREFIX)
                    && !EXCLUDED_ATTRIBUTES.contains(&key_name)
            }) {
                serializer.serialize_entry(key.as_str(), &value.as_str())?;
            }
        }
        // Get custom dynamic attributes
        let custom_attributes = ext.get::<LogAttributes>().map(|attrs| attrs.attributes());
        if let Some(custom_attributes) = custom_attributes {
            for (key, value) in custom_attributes.iter().filter(|(k, _)| {
                let key_name = k.as_str();
                !key_name.starts_with(APOLLO_PRIVATE_PREFIX)
                    && !EXCLUDED_ATTRIBUTES.contains(&key_name)
            }) {
                match value {
                    Value::Bool(value) => {
                        serializer.serialize_entry(key.as_str(), value)?;
                    }
                    Value::I64(value) => {
                        serializer.serialize_entry(key.as_str(), value)?;
                    }
                    Value::F64(value) => {
                        serializer.serialize_entry(key.as_str(), value)?;
                    }
                    Value::String(value) => {
                        serializer.serialize_entry(key.as_str(), value.as_str())?;
                    }
                    Value::Array(Array::Bool(array)) => {
                        serializer.serialize_entry(key.as_str(), array)?;
                    }
                    Value::Array(Array::I64(array)) => {
                        serializer.serialize_entry(key.as_str(), array)?;
                    }
                    Value::Array(Array::F64(array)) => {
                        serializer.serialize_entry(key.as_str(), array)?;
                    }
                    Value::Array(Array::String(array)) => {
                        let array = array.iter().map(|a| a.as_str()).collect::<Vec<_>>();
                        serializer.serialize_entry(key.as_str(), &array)?;
                    }
                }
            }
        }

        serializer.serialize_entry("name", self.0.metadata().name())?;
        serializer.end()
    }
}

impl<S> EventFormatter<S> for Json
where
    S: Subscriber + for<'lookup> LookupSpan<'lookup>,
{
    fn format_event<W>(
        &self,
        ctx: &Context<'_, S>,
        writer: &mut W,
        event: &Event<'_>,
    ) -> fmt::Result
    where
        S: Subscriber + for<'a> LookupSpan<'a>,
        W: std::fmt::Write,
    {
        let meta = event.metadata();

        let mut visit = || {
            let mut serializer = Serializer::new(WriteAdaptor::new(writer));

            let mut serializer = serializer.serialize_map(None)?;

            if self.config.display_timestamp {
                let timestamp = time::OffsetDateTime::now_utc()
                    .format(&time::format_description::well_known::Iso8601::DEFAULT)
                    .map_err(|e| serde::ser::Error::custom(e.to_string()))?;
                serializer.serialize_entry("timestamp", &timestamp)?;
            }

            if self.config.display_level {
                serializer.serialize_entry("level", &meta.level().as_serde())?;
            }

            let current_span = event
                .parent()
                .and_then(|id| ctx.span(id))
                .or_else(|| ctx.lookup_current());
            let mut visitor = tracing_serde::SerdeMapVisitor::new(serializer);
            event.record(&mut visitor);

            serializer = visitor.take_serializer()?;

            if self.config.display_target {
                serializer.serialize_entry("target", meta.target())?;
            }

            if self.config.display_filename {
                if let Some(filename) = meta.file() {
                    serializer.serialize_entry("filename", filename)?;
                }
            }

            if self.config.display_line_number {
                if let Some(line_number) = meta.line() {
                    serializer.serialize_entry("line_number", &line_number)?;
                }
            }

            if self.config.display_current_span {
                if let Some(ref span) = current_span {
                    serializer
                        .serialize_entry("span", &SerializableSpan(span))
                        .unwrap_or(());
                }
            }

            if self.config.display_span_list && current_span.is_some() {
                serializer.serialize_entry("spans", &SerializableContext(ctx.lookup_current()))?;
            }

            if self.config.display_resource {
                serializer.serialize_entry("resource", &self.resource)?;
            }

            serializer.end()
        };

        visit().map_err(|_| fmt::Error)?;
        writeln!(writer)
    }
}

/// The JSON [`FormatFields`] implementation.
///
#[derive(Debug, Default)]
pub(crate) struct JsonFields;

impl<'a> FormatFields<'a> for JsonFields {
    /// Format the provided `fields` to the provided `writer`, returning a result.
    fn format_fields<R: field::RecordFields>(
        &self,
        mut writer: Writer<'_>,
        fields: R,
    ) -> fmt::Result {
        let mut v = JsonVisitor::new(&mut writer);
        fields.record(&mut v);
        v.finish()
    }

    /// Record additional field(s) on an existing span.
    ///
    /// By default, this appends a space to the current set of fields if it is
    /// non-empty, and then calls `self.format_fields`. If different behavior is
    /// required, the default implementation of this method can be overridden.
    fn add_fields(
        &self,
        current: &'a mut FormattedFields<Self>,
        fields: &Record<'_>,
    ) -> fmt::Result {
        if current.is_empty() {
            let mut writer = current.as_writer();
            let mut v = JsonVisitor::new(&mut writer);
            fields.record(&mut v);
            v.finish()?;
            return Ok(());
        }

        let mut new = String::new();
        let map: HashMap<&'_ str, serde_json::Value> =
            serde_json::from_str(current).map_err(|_| fmt::Error)?;
        let mut v = JsonVisitor::new(&mut new);
        v.values = map;
        fields.record(&mut v);
        v.finish()?;
        current.fields = new;

        Ok(())
    }
}

/// The [visitor] produced by [`JsonFields`]'s [`MakeVisitor`] implementation.
///
/// [visitor]: tracing_subscriber::field::Visit
/// [`MakeVisitor`]: tracing_subscriber::field::MakeVisitor
pub(crate) struct JsonVisitor<'a> {
    pub(crate) values: HashMap<&'a str, serde_json::Value>,
    writer: &'a mut dyn Write,
}

impl<'a> fmt::Debug for JsonVisitor<'a> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_fmt(format_args!("JsonVisitor {{ values: {:?} }}", self.values))
    }
}

impl<'a> JsonVisitor<'a> {
    /// Returns a new default visitor that formats to the provided `writer`.
    ///
    /// # Arguments
    /// - `writer`: the writer to format to.
    /// - `is_empty`: whether or not any fields have been previously written to
    ///   that writer.
    pub(crate) fn new(writer: &'a mut dyn Write) -> Self {
        Self {
            values: HashMap::new(),
            writer,
        }
    }
}

impl<'a> tracing_subscriber::field::VisitFmt for JsonVisitor<'a> {
    fn writer(&mut self) -> &mut dyn fmt::Write {
        self.writer
    }
}

impl<'a> tracing_subscriber::field::VisitOutput<fmt::Result> for JsonVisitor<'a> {
    fn finish(self) -> fmt::Result {
        let inner = || {
            let mut serializer = Serializer::new(WriteAdaptor::new(self.writer));
            let mut ser_map = serializer.serialize_map(None)?;

            for (k, v) in self.values {
                ser_map.serialize_entry(k, &v)?;
            }

            ser_map.end()
        };

        if inner().is_err() {
            Err(fmt::Error)
        } else {
            Ok(())
        }
    }
}

impl<'a> field::Visit for JsonVisitor<'a> {
    /// Visit a double precision floating point value.
    fn record_f64(&mut self, field: &Field, value: f64) {
        self.values
            .insert(field.name(), serde_json::Value::from(value));
    }

    /// Visit a signed 64-bit integer value.
    fn record_i64(&mut self, field: &Field, value: i64) {
        self.values
            .insert(field.name(), serde_json::Value::from(value));
    }

    /// Visit an unsigned 64-bit integer value.
    fn record_u64(&mut self, field: &Field, value: u64) {
        self.values
            .insert(field.name(), serde_json::Value::from(value));
    }

    /// Visit a boolean value.
    fn record_bool(&mut self, field: &Field, value: bool) {
        self.values
            .insert(field.name(), serde_json::Value::from(value));
    }

    /// Visit a string value.
    fn record_str(&mut self, field: &Field, value: &str) {
        let field_name = field.name();
        match field_name {
            "code.filepath" | "code.namespace" | "code.lineno" | "thread.id" | "thread.name" => {}
            field_name => {
                self.values
                    .insert(field_name, serde_json::Value::from(value));
            }
        }
    }

    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        match field.name() {
            "code.filepath" | "code.namespace" | "code.lineno" | "thread.id" | "thread.name" => {}
            name if name.starts_with("r#") => {
                self.values
                    .insert(&name[2..], serde_json::Value::from(format!("{:?}", value)));
            }
            name => {
                self.values
                    .insert(name, serde_json::Value::from(format!("{:?}", value)));
            }
        };
    }
}

struct WriteAdaptor<'a> {
    fmt_write: &'a mut dyn fmt::Write,
}

impl<'a> WriteAdaptor<'a> {
    fn new(fmt_write: &'a mut dyn fmt::Write) -> Self {
        Self { fmt_write }
    }
}

impl<'a> io::Write for WriteAdaptor<'a> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let s =
            std::str::from_utf8(buf).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;

        self.fmt_write
            .write_str(s)
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;

        Ok(s.as_bytes().len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl<'a> fmt::Debug for WriteAdaptor<'a> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.pad("WriteAdaptor { .. }")
    }
}
