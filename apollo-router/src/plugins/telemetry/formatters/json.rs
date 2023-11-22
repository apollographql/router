#[cfg(test)]
use std::collections::BTreeMap;
#[cfg(not(test))]
use std::collections::HashMap;
use std::collections::HashSet;
use std::fmt;
use std::io;

use opentelemetry::Array;
use opentelemetry::Value;
use opentelemetry_sdk::Resource;
use serde::ser::SerializeMap;
use serde::ser::Serializer as _;
use serde_json::Serializer;
use tracing_core::Event;
use tracing_core::Subscriber;
use tracing_opentelemetry::OtelData;
use tracing_serde::AsSerde;
use tracing_subscriber::layer::Context;
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::registry::SpanRef;

use super::EventFormatter;
use super::APOLLO_PRIVATE_PREFIX;
use super::EXCLUDED_ATTRIBUTES;
use crate::plugins::telemetry::config_new::logging::JsonFormat;
use crate::plugins::telemetry::dynamic_attribute::LogAttributes;
use crate::plugins::telemetry::formatters::to_map;

#[derive(Debug)]
pub(crate) struct Json {
    config: JsonFormat,
    #[cfg(not(test))]
    resource: HashMap<String, serde_json::Value>,
    #[cfg(test)]
    resource: BTreeMap<String, serde_json::Value>,
    excluded_attributes: HashSet<&'static str>,
}

impl Json {
    pub(crate) fn new(resource: Resource, config: JsonFormat) -> Self {
        Self {
            #[cfg(not(test))]
            resource: to_map(resource),
            #[cfg(test)]
            resource: to_map(resource).into_iter().collect(),
            config,
            excluded_attributes: EXCLUDED_ATTRIBUTES.into(),
        }
    }
}

impl Default for Json {
    fn default() -> Self {
        Self {
            config: Default::default(),
            resource: Default::default(),
            excluded_attributes: EXCLUDED_ATTRIBUTES.into(),
        }
    }
}

struct SerializableContext<'a, 'b, Span>(Option<SpanRef<'a, Span>>, &'b HashSet<&'static str>)
where
    Span: Subscriber + for<'lookup> tracing_subscriber::registry::LookupSpan<'lookup>;

impl<'a, 'b, Span> serde::ser::Serialize for SerializableContext<'a, 'b, Span>
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
                // TODO: Here in the future we could try to memoize parent spans of the current span to not re serialize eveything if another log happens in the same span
                serializer.serialize_element(&SerializableSpan(&span, self.1))?;
            }
        }

        serializer.end()
    }
}

struct SerializableSpan<'a, 'b, Span>(
    &'b tracing_subscriber::registry::SpanRef<'a, Span>,
    &'b HashSet<&'static str>,
)
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
        {
            let otel_attributes = ext
                .get::<OtelData>()
                .and_then(|otel_data| otel_data.builder.attributes.as_ref());
            if let Some(otel_attributes) = otel_attributes {
                for kv in otel_attributes.iter().filter(|kv| {
                    let key_name = kv.key.as_str();
                    !key_name.starts_with(APOLLO_PRIVATE_PREFIX) && !self.1.contains(&key_name)
                }) {
                    serializer.serialize_entry(kv.key.as_str(), &kv.value.as_str())?;
                }
            }
        }
        // Get custom dynamic attributes
        {
            let custom_attributes = ext.get::<LogAttributes>().map(|attrs| attrs.attributes());
            if let Some(custom_attributes) = custom_attributes {
                #[cfg(test)]
                let custom_attributes: Vec<&opentelemetry::KeyValue> = {
                    let mut my_custom_attributes: Vec<&opentelemetry::KeyValue> =
                        custom_attributes.iter().collect();
                    my_custom_attributes.sort_by_key(|kv| &kv.key);
                    my_custom_attributes
                };
                for kv in custom_attributes.iter().filter(|kv| {
                    let key_name = kv.key.as_str();
                    !key_name.starts_with(APOLLO_PRIVATE_PREFIX) && !self.1.contains(&key_name)
                }) {
                    match &kv.value {
                        Value::Bool(value) => {
                            serializer.serialize_entry(kv.key.as_str(), value)?;
                        }
                        Value::I64(value) => {
                            serializer.serialize_entry(kv.key.as_str(), value)?;
                        }
                        Value::F64(value) => {
                            serializer.serialize_entry(kv.key.as_str(), value)?;
                        }
                        Value::String(value) => {
                            serializer.serialize_entry(kv.key.as_str(), value.as_str())?;
                        }
                        Value::Array(Array::Bool(array)) => {
                            serializer.serialize_entry(kv.key.as_str(), array)?;
                        }
                        Value::Array(Array::I64(array)) => {
                            serializer.serialize_entry(kv.key.as_str(), array)?;
                        }
                        Value::Array(Array::F64(array)) => {
                            serializer.serialize_entry(kv.key.as_str(), array)?;
                        }
                        Value::Array(Array::String(array)) => {
                            let array = array.iter().map(|a| a.as_str()).collect::<Vec<_>>();
                            serializer.serialize_entry(kv.key.as_str(), &array)?;
                        }
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
                #[cfg(test)]
                {
                    serializer.serialize_entry("timestamp", "[timestamp]")?;
                }
                #[cfg(not(test))]
                {
                    let timestamp = time::OffsetDateTime::now_utc()
                        .format(&time::format_description::well_known::Iso8601::DEFAULT)
                        .map_err(|e| serde::ser::Error::custom(e.to_string()))?;
                    serializer.serialize_entry("timestamp", &timestamp)?;
                }
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
                        .serialize_entry("span", &SerializableSpan(span, &self.excluded_attributes))
                        .unwrap_or(());
                }
            }

            if self.config.display_span_list && current_span.is_some() {
                serializer.serialize_entry(
                    "spans",
                    &SerializableContext(ctx.lookup_current(), &self.excluded_attributes),
                )?;
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
