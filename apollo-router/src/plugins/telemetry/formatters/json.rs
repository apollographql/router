use std::collections::BTreeMap;
use std::fmt;
use std::fmt::Write;
use std::io;

use chrono::SecondsFormat;
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
use tracing_subscriber::fmt::FmtContext;
use tracing_subscriber::fmt::FormatEvent;
use tracing_subscriber::fmt::FormatFields;
use tracing_subscriber::fmt::FormattedFields;
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::registry::SpanRef;

use crate::plugins::telemetry::dynamic_attribute::LogAttributes;
const APOLLO_PRIVATE_PREFIX: &str = "apollo_private.";

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub(crate) struct Json {
    pub(crate) flatten_event: bool,
    pub(crate) display_current_span: bool,
    pub(crate) display_filename: bool,
    pub(crate) display_target: bool,
    pub(crate) display_line_number: bool,
    pub(crate) display_span_list: bool,
}

impl Json {
    /// If set to `true` event metadata will be flattened into the root object.
    pub(crate) fn flatten_event(mut self, flatten_event: bool) -> Self {
        self.flatten_event = flatten_event;
        self
    }

    pub(crate) fn with_target(mut self, display_target: bool) -> Self {
        self.display_target = display_target;
        self
    }

    pub(crate) fn with_file(mut self, display_filename: bool) -> Self {
        self.display_filename = display_filename;
        self
    }

    pub(crate) fn with_line_number(mut self, display_line_number: bool) -> Self {
        self.display_line_number = display_line_number;
        self
    }
    /// If set to `false`, formatted events won't contain a field for the current span.
    pub(crate) fn with_current_span(mut self, display_current_span: bool) -> Self {
        self.display_current_span = display_current_span;
        self
    }

    /// If set to `false`, formatted events won't contain a list of all currently
    /// entered spans. Spans are logged in a list from root to leaf.
    pub(crate) fn with_span_list(mut self, display_span_list: bool) -> Self {
        self.display_span_list = display_span_list;
        self
    }
}

struct SerializableContext<'a, Span, N>(Option<SpanRef<'a, Span>>, std::marker::PhantomData<N>)
where
    Span: Subscriber + for<'lookup> tracing_subscriber::registry::LookupSpan<'lookup>,
    N: for<'writer> FormatFields<'writer> + 'static;

impl<'a, Span, N> serde::ser::Serialize for SerializableContext<'a, Span, N>
where
    Span: Subscriber + for<'lookup> tracing_subscriber::registry::LookupSpan<'lookup>,
    N: for<'writer> FormatFields<'writer> + 'static,
{
    fn serialize<Ser>(&self, serializer_o: Ser) -> Result<Ser::Ok, Ser::Error>
    where
        Ser: serde::ser::Serializer,
    {
        use serde::ser::SerializeSeq;
        let mut serializer = serializer_o.serialize_seq(None)?;

        if let Some(leaf_span) = &self.0 {
            for span in leaf_span.scope().from_root() {
                serializer.serialize_element(&SerializableSpan(&span, self.1))?;
            }
        }

        serializer.end()
    }
}

struct SerializableSpan<'a, 'b, Span, N>(
    &'b tracing_subscriber::registry::SpanRef<'a, Span>,
    std::marker::PhantomData<N>,
)
where
    Span: for<'lookup> tracing_subscriber::registry::LookupSpan<'lookup>,
    N: for<'writer> FormatFields<'writer> + 'static;

impl<'a, 'b, Span, N> serde::ser::Serialize for SerializableSpan<'a, 'b, Span, N>
where
    Span: for<'lookup> tracing_subscriber::registry::LookupSpan<'lookup>,
    N: for<'writer> FormatFields<'writer> + 'static,
{
    fn serialize<Ser>(&self, serializer: Ser) -> Result<Ser::Ok, Ser::Error>
    where
        Ser: serde::ser::Serializer,
    {
        let mut serializer = serializer.serialize_map(None)?;

        let ext = self.0.extensions();
        let data = ext
            .get::<FormattedFields<N>>()
            .expect("Unable to find FormattedFields in extensions; this is a bug");

        if data.fields.is_empty() {
            return serializer.end();
        }

        match serde_json::from_str::<serde_json::Value>(data) {
            Ok(serde_json::Value::Object(fields)) => {
                for field in fields.into_iter().filter(|(key, _)| !key.starts_with(APOLLO_PRIVATE_PREFIX)) {
                    serializer.serialize_entry(&field.0, &field.1)?;
                }
                // Get otel attributes
                let otel_attributes = ext.get::<OtelData>().and_then(|otel_data| otel_data.builder.attributes.as_ref());
                if let Some(otel_attributes) = otel_attributes {
                    for (key, value) in otel_attributes.iter().filter(|(k, _)| {
                        let key_name = k.as_str();
                        !key_name.starts_with(APOLLO_PRIVATE_PREFIX) && !["code.filepath", "code.namespace", "code.lineno", "thread.id", "thread.name"].contains(&key_name)
                    }) {
                        serializer.serialize_entry(key.as_str(), &value.as_str())?;
                    }
                }
                // Get custom dynamic attributes
                let custom_attributes = ext.get::<LogAttributes>().map(|attrs| attrs.get_attributes());
                if let Some(custom_attributes) = custom_attributes {
                    for (key, value) in custom_attributes {
                        serializer.serialize_entry(key.as_str(), value)?;
                    }
                }
            }
            // We have fields for this span which are valid JSON but not an object.
            // This is probably a bug, so panic if we're in debug mode
            Ok(_) if cfg!(debug_assertions) => panic!(
                "span '{}' had malformed fields! this is a bug.\n  error: invalid JSON object\n  fields: {:?}",
                self.0.metadata().name(),
                data
            ),
            // If we *aren't* in debug mode, it's probably best not to
            // crash the program, let's log the field found but also an
            // message saying it's type  is invalid
            Ok(value) => {
                serializer.serialize_entry("field", &value)?;
                serializer.serialize_entry("field_error", "field was no a valid object")?
            }
            // We have previously recorded fields for this span
            // should be valid JSON. However, they appear to *not*
            // be valid JSON. This is almost certainly a bug, so
            // panic if we're in debug mode
            Err(e) if cfg!(debug_assertions) => panic!(
                "span '{}' had malformed fields! this is a bug.\n  error: {}\n  fields: {:?}",
                self.0.metadata().name(),
                e,
                data
            ),
            // If we *aren't* in debug mode, it's probably best not
            // crash the program, but let's at least make sure it's clear
            // that the fields are not supposed to be missing.
            Err(e) => serializer.serialize_entry("field_error", &format!("{}", e))?,
        };
        serializer.serialize_entry("name", self.0.metadata().name())?;
        serializer.end()
    }
}

impl<S, N> FormatEvent<S, N> for Json
where
    S: Subscriber + for<'lookup> LookupSpan<'lookup>,
    N: for<'writer> FormatFields<'writer> + 'static,
{
    fn format_event(
        &self,
        ctx: &FmtContext<'_, S, N>,
        mut writer: Writer<'_>,
        event: &Event<'_>,
    ) -> fmt::Result
    where
        S: Subscriber + for<'a> LookupSpan<'a>,
    {
        let timestamp = chrono::Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true);

        let meta = event.metadata();

        let mut visit = || {
            let mut serializer = Serializer::new(WriteAdaptor::new(&mut writer));

            let mut serializer = serializer.serialize_map(None)?;

            serializer.serialize_entry("timestamp", &timestamp)?;

            serializer.serialize_entry("level", &meta.level().as_serde())?;

            let format_field_marker: std::marker::PhantomData<N> = std::marker::PhantomData;

            let current_span = event
                .parent()
                .and_then(|id| ctx.span(id))
                .or_else(|| ctx.lookup_current());
            let mut visitor = tracing_serde::SerdeMapVisitor::new(serializer);
            event.record(&mut visitor);

            serializer = visitor.take_serializer()?;

            if self.display_target {
                serializer.serialize_entry("target", meta.target())?;
            }

            if self.display_filename {
                if let Some(filename) = meta.file() {
                    serializer.serialize_entry("filename", filename)?;
                }
            }

            if self.display_line_number {
                if let Some(line_number) = meta.line() {
                    serializer.serialize_entry("line_number", &line_number)?;
                }
            }

            if self.display_current_span {
                if let Some(ref span) = current_span {
                    serializer
                        .serialize_entry("span", &SerializableSpan(span, format_field_marker))
                        .unwrap_or(());
                }
            }

            if self.display_span_list && current_span.is_some() {
                serializer.serialize_entry(
                    "spans",
                    &SerializableContext(ctx.lookup_current(), format_field_marker),
                )?;
            }

            serializer.end()
        };

        visit().map_err(|_| fmt::Error)?;
        writeln!(writer)
    }
}

impl Default for Json {
    fn default() -> Json {
        Json {
            flatten_event: false,
            display_current_span: true,
            display_span_list: true,
            display_filename: true,
            display_line_number: true,
            display_target: true,
        }
    }
}

// TODO: I think we can get rid of the code below

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
        let map: BTreeMap<&'_ str, serde_json::Value> =
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
struct JsonVisitor<'a> {
    values: BTreeMap<&'a str, serde_json::Value>,
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
    fn new(writer: &'a mut dyn Write) -> Self {
        Self {
            values: BTreeMap::new(),
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
            "code.filepath" | "code.namespace" | "code.lineno" | "thread.id" | "thread.name" => {
                println!("!!!!!!!!!!!!!!!!!!!!!");
            }
            field_name => {
                self.values
                    .insert(field_name, serde_json::Value::from(value));
            }
        }
    }

    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        match field.name() {
            "code.filepath" | "code.namespace" | "code.lineno" | "thread.id" | "thread.name" => {
                println!("!!!!!!!!!!!!!!!!!!!!!");
            }
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
