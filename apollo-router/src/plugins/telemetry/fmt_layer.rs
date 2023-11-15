use std::cell::RefCell;
use std::collections::HashMap;
use std::marker::PhantomData;

use tracing::field;
use tracing_core::span::Id;
use tracing_core::span::Record;
use tracing_core::Event;
use tracing_core::Field;
use tracing_subscriber::layer::Context;
use tracing_subscriber::Layer;

use super::config_new::ToOtelValue;
use super::dynamic_attribute::LogAttributes;
use super::formatters::EventFormatter;
use super::reload::IsSampled;
use crate::plugins::telemetry::config;
use crate::plugins::telemetry::config_new::logging::Format;
use crate::plugins::telemetry::config_new::logging::StdOut;
use crate::plugins::telemetry::formatters::filter_metric_events;
use crate::plugins::telemetry::formatters::json::Json;
use crate::plugins::telemetry::formatters::text::Text;
use crate::plugins::telemetry::formatters::FilteringFormatter;
use crate::plugins::telemetry::reload::LayeredTracer;
use crate::plugins::telemetry::resource::ConfigResource;

pub(crate) fn create_fmt_layer(
    config: &config::Conf,
) -> Box<dyn Layer<LayeredTracer> + Send + Sync> {
    match &config.exporters.logging.stdout {
        StdOut { enabled, format } if *enabled => match format {
            Format::Json(format_config) => {
                let format = Json::new(
                    config.exporters.logging.common.to_resource(),
                    format_config.clone(),
                );
                FmtLayer::new(FilteringFormatter::new(format, filter_metric_events)).boxed()
            }

            Format::Text(format_config) => {
                let format = Text::new(
                    config.exporters.logging.common.to_resource(),
                    format_config.clone(),
                );
                FmtLayer::new(FilteringFormatter::new(format, filter_metric_events)).boxed()
            }
        },
        _ => NoOpLayer.boxed(),
    }
}

struct NoOpLayer;

impl Layer<LayeredTracer> for NoOpLayer {}

pub(crate) struct FmtLayer<T, S> {
    fmt_event: T,
    _inner: PhantomData<S>,
}

impl<T, S> FmtLayer<T, S>
where
    S: tracing_core::Subscriber + for<'lookup> tracing_subscriber::registry::LookupSpan<'lookup>,
    T: EventFormatter<S>,
{
    pub(crate) fn new(fmt_event: T) -> Self {
        Self {
            fmt_event,
            _inner: PhantomData,
        }
    }
}

impl<S, T> Layer<S> for FmtLayer<T, S>
where
    S: tracing_core::Subscriber + for<'lookup> tracing_subscriber::registry::LookupSpan<'lookup>,
    T: EventFormatter<S> + 'static,
{
    fn on_new_span(
        &self,
        attrs: &tracing_core::span::Attributes<'_>,
        id: &tracing_core::span::Id,
        ctx: Context<'_, S>,
    ) {
        let span = ctx.span(id).expect("Span not found, this is a bug");
        let mut visitor = FieldsVisitor::default();
        // We're checking if it's sampled to not add both attributes in OtelData and our LogAttributes
        if !span.is_sampled() {
            attrs.record(&mut visitor);
        }
        let mut extensions = span.extensions_mut();
        if extensions.get_mut::<LogAttributes>().is_none() {
            let mut fields = LogAttributes::default();
            fields.extend(
                visitor
                    .values
                    .into_iter()
                    .filter_map(|(k, v)| Some((k.into(), v.maybe_to_otel_value()?))),
            );

            extensions.insert(fields);
        } else if !visitor.values.is_empty() {
            let log_attrs = extensions
                .get_mut::<LogAttributes>()
                .expect("LogAttributes exists, we checked just before");
            log_attrs.extend(
                visitor
                    .values
                    .into_iter()
                    .filter_map(|(k, v)| Some((k.into(), v.maybe_to_otel_value()?))),
            );
        }
    }

    fn on_record(&self, id: &Id, values: &Record<'_>, ctx: Context<'_, S>) {
        let span = ctx.span(id).expect("Span not found, this is a bug");
        let mut extensions = span.extensions_mut();
        if let Some(fields) = extensions.get_mut::<LogAttributes>() {
            let mut visitor = FieldsVisitor::default();
            values.record(&mut visitor);
            fields.extend(
                visitor
                    .values
                    .into_iter()
                    .filter_map(|(k, v)| Some((k.into(), v.maybe_to_otel_value()?))),
            );
        } else {
            eprintln!("cannot access to LogAttributes, this is a bug");
        }
    }

    fn on_event(&self, event: &Event<'_>, ctx: Context<'_, S>) {
        thread_local! {
            static BUF: RefCell<String> = RefCell::new(String::new());
        }

        BUF.with(|buf| {
            let borrow = buf.try_borrow_mut();
            let mut a;
            let mut b;
            let mut buf = match borrow {
                Ok(buf) => {
                    a = buf;
                    &mut *a
                }
                _ => {
                    b = String::new();
                    &mut b
                }
            };
            if self.fmt_event.format_event(&ctx, &mut buf, event).is_ok() {
                let mut writer = std::io::stdout();
                if let Err(err) = std::io::Write::write_all(&mut writer, buf.as_bytes()) {
                    eprintln!("cannot flush the logging buffer, this is a bug: {err:?}");
                }
            }
            buf.clear();
        });
    }
}

#[derive(Debug)]
pub(crate) struct FieldsVisitor<'a> {
    pub(crate) values: HashMap<&'a str, serde_json::Value>,
}

impl<'a> Default for FieldsVisitor<'a> {
    fn default() -> Self {
        Self {
            values: HashMap::with_capacity(0),
        }
    }
}

impl<'a> field::Visit for FieldsVisitor<'a> {
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
        if field_name == "code.filepath"
            || field_name == "code.namespace"
            || field_name == "code.lineno"
            || field_name == "thread.id"
            || field_name == "thread.name"
        {
            return;
        }
        self.values
            .insert(field_name, serde_json::Value::from(value));
    }

    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        let field_name = field.name();
        if field_name == "code.filepath"
            || field_name == "code.namespace"
            || field_name == "code.lineno"
            || field_name == "thread.id"
            || field_name == "thread.name"
        {
            return;
        }
        match field_name {
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
