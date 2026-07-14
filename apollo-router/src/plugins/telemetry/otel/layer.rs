use std::any::TypeId;
use std::fmt;
use std::marker;
use std::thread;
use std::time::Instant;
use std::time::SystemTime;

use once_cell::unsync;
use opentelemetry::Context as OtelContext;
use opentelemetry::Key;
use opentelemetry::KeyValue;
use opentelemetry::StringValue;
use opentelemetry::Value;
use opentelemetry::trace as otel;
use opentelemetry::trace::Span as _;
use opentelemetry::trace::TraceContextExt;
use opentelemetry::trace::noop;
use opentelemetry_sdk::trace::IdGenerator;
use opentelemetry_sdk::trace::RandomIdGenerator;
use tracing_core::Event;
use tracing_core::Subscriber;
use tracing_core::field;
use tracing_core::span;
use tracing_core::span::Attributes;
use tracing_core::span::Id;
use tracing_core::span::Record;
use tracing_subscriber::Layer;
use tracing_subscriber::layer::Context;
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::registry::SpanRef;

use super::OtelData;
use crate::plugins::response_cache::invalidation_endpoint::INVALIDATION_ENDPOINT_SPAN_NAME;
use crate::plugins::telemetry::consts::FIELD_EXCEPTION_MESSAGE;
use crate::plugins::telemetry::consts::FIELD_EXCEPTION_STACKTRACE;
use crate::plugins::telemetry::consts::OTEL_KIND;
use crate::plugins::telemetry::consts::OTEL_NAME;
use crate::plugins::telemetry::consts::OTEL_ORIGINAL_NAME;
use crate::plugins::telemetry::consts::OTEL_STATUS_CODE;
use crate::plugins::telemetry::consts::OTEL_STATUS_MESSAGE;
use crate::plugins::telemetry::consts::REQUEST_SPAN_NAME;
use crate::plugins::telemetry::consts::ROUTER_SPAN_NAME;
use crate::plugins::telemetry::reload::otel::IsSampled;
use crate::plugins::telemetry::reload::otel::SampledSpan;
use crate::plugins::telemetry::utils::upsert_attribute;
use crate::query_planner::subscription::SUBSCRIPTION_EVENT_SPAN_NAME;
use crate::router_factory::STARTING_SPAN_NAME;

/// An [OpenTelemetry] propagation layer for use in a project that uses
/// [tracing].
///
/// [OpenTelemetry]: https://opentelemetry.io
/// [tracing]: https://github.com/tokio-rs/tracing
pub(crate) struct OpenTelemetryLayer<S, T> {
    /// ONLY for tests
    force_sampling: bool,
    tracer: T,
    location: bool,
    tracked_inactivity: bool,
    with_threads: bool,
    exception_config: ExceptionFieldConfig,
    get_context: WithContext,
    _registry: marker::PhantomData<S>,
}
impl<S> Default for OpenTelemetryLayer<S, noop::NoopTracer>
where
    S: Subscriber + for<'span> LookupSpan<'span>,
{
    fn default() -> Self {
        OpenTelemetryLayer::new(noop::NoopTracer::new())
    }
}

impl<S> OpenTelemetryLayer<S, noop::NoopTracer>
where
    S: Subscriber + for<'span> LookupSpan<'span>,
{
    #[cfg(test)]
    pub(crate) fn force_sampling(mut self) -> Self {
        self.force_sampling = true;
        self
    }
}

/// Construct a layer to track spans via [OpenTelemetry].
///
/// [OpenTelemetry]: https://opentelemetry.io
///
/// # Examples
///
/// ```rust,no_run
/// use tracing_subscriber::layer::SubscriberExt;
/// use tracing_subscriber::Registry;
///
/// // Use the tracing subscriber `Registry`, or any other subscriber
/// // that impls `LookupSpan`
/// let subscriber = Registry::default().with(tracing_opentelemetry::layer());
/// # drop(subscriber);
/// ```
pub(crate) fn layer<S>() -> OpenTelemetryLayer<S, noop::NoopTracer>
where
    S: Subscriber + for<'span> LookupSpan<'span>,
{
    OpenTelemetryLayer::default()
}

// this function "remembers" the types of the subscriber so that we
// can downcast to something aware of them without knowing those
// types at the callsite.
//
// See https://github.com/tokio-rs/tracing/blob/4dad420ee1d4607bad79270c1520673fa6266a3d/tracing-error/src/layer.rs
pub(crate) struct WithContext(
    #[allow(clippy::type_complexity)]
    fn(&tracing::Dispatch, &span::Id, f: &mut dyn FnMut(&mut OtelData)),
);

impl WithContext {
    // This function allows a function to be called in the context of the
    // "remembered" subscriber. `OtelData` is always already built by the time `f` is
    // invoked - see the doc comment on `OtelData` itself.
    pub(crate) fn with_context(
        &self,
        dispatch: &tracing::Dispatch,
        id: &span::Id,
        mut f: impl FnMut(&mut OtelData),
    ) {
        (self.0)(dispatch, id, &mut f)
    }
}

pub(crate) fn str_to_span_kind(s: &str) -> Option<otel::SpanKind> {
    match s {
        s if s.eq_ignore_ascii_case("server") => Some(otel::SpanKind::Server),
        s if s.eq_ignore_ascii_case("client") => Some(otel::SpanKind::Client),
        s if s.eq_ignore_ascii_case("producer") => Some(otel::SpanKind::Producer),
        s if s.eq_ignore_ascii_case("consumer") => Some(otel::SpanKind::Consumer),
        s if s.eq_ignore_ascii_case("internal") => Some(otel::SpanKind::Internal),
        _ => None,
    }
}

pub(crate) fn str_to_status(s: &str) -> otel::Status {
    match s {
        s if s.eq_ignore_ascii_case("ok") => otel::Status::Ok,
        s if s.eq_ignore_ascii_case("error") => otel::Status::error(""),
        _ => otel::Status::Unset,
    }
}

struct SpanEventVisitor<'a, 'b> {
    event_builder: &'a mut otel::Event,
    otel_data: Option<&'b mut OtelData>,
    exception_config: ExceptionFieldConfig,
    custom_event: bool,
}

impl field::Visit for SpanEventVisitor<'_, '_> {
    /// Record events on the underlying OpenTelemetry [`Span`] from `bool` values.
    ///
    /// [`Span`]: opentelemetry::trace::Span
    fn record_bool(&mut self, field: &field::Field, value: bool) {
        match field.name() {
            "message" => self.event_builder.name = value.to_string().into(),
            name => {
                self.event_builder
                    .attributes
                    .push(KeyValue::new(name, value));
            }
        }
    }

    /// Record events on the underlying OpenTelemetry [`Span`] from `f64` values.
    ///
    /// [`Span`]: opentelemetry::trace::Span
    fn record_f64(&mut self, field: &field::Field, value: f64) {
        match field.name() {
            "message" => self.event_builder.name = value.to_string().into(),
            name => {
                self.event_builder
                    .attributes
                    .push(KeyValue::new(name, value));
            }
        }
    }

    /// Record events on the underlying OpenTelemetry [`Span`] from `i64` values.
    ///
    /// [`Span`]: opentelemetry::trace::Span
    fn record_i64(&mut self, field: &field::Field, value: i64) {
        match field.name() {
            "message" => self.event_builder.name = value.to_string().into(),
            name => {
                self.event_builder
                    .attributes
                    .push(KeyValue::new(name, value));
            }
        }
    }

    /// Record events on the underlying OpenTelemetry [`Span`] from `&str` values.
    ///
    /// [`Span`]: opentelemetry::trace::Span
    fn record_str(&mut self, field: &field::Field, value: &str) {
        match field.name() {
            "message" => self.event_builder.name = value.to_string().into(),
            name => {
                if name == "kind" {
                    self.custom_event = true;
                }
                self.event_builder
                    .attributes
                    .push(KeyValue::new(name, value.to_string()));
            }
        }
    }

    /// Record events on the underlying OpenTelemetry [`Span`] from values that
    /// implement Debug.
    ///
    /// [`Span`]: opentelemetry::trace::Span
    fn record_debug(&mut self, field: &field::Field, value: &dyn fmt::Debug) {
        match field.name() {
            "message" => self.event_builder.name = format!("{value:?}").into(),
            name => {
                if name == "kind" {
                    self.custom_event = true;
                }
                self.event_builder
                    .attributes
                    .push(KeyValue::new(name, format!("{value:?}")));
            }
        }
    }

    /// Set attributes on the underlying OpenTelemetry [`Span`] using a [`std::error::Error`]'s
    /// [`std::fmt::Display`] implementation. Also adds the `source` chain as an extra field
    ///
    /// [`Span`]: opentelemetry::trace::Span
    fn record_error(
        &mut self,
        field: &tracing_core::Field,
        value: &(dyn std::error::Error + 'static),
    ) {
        let mut chain: Vec<StringValue> = Vec::new();
        let mut next_err = value.source();

        while let Some(err) = next_err {
            chain.push(err.to_string().into());
            next_err = err.source();
        }

        let error_msg = value.to_string();

        if self.exception_config.record {
            self.event_builder
                .attributes
                .push(KeyValue::new(FIELD_EXCEPTION_MESSAGE, error_msg.clone()));

            // NOTE: This is actually not the stacktrace of the exception. This is
            // the "source chain". It represents the hierarchy of errors from the
            // app level to the lowest level such as IO. It does not represent all
            // of the callsites in the code that led to the error happening.
            // `std::error::Error::backtrace` is a nightly-only API and cannot be
            // used here until the feature is stabilized.
            self.event_builder.attributes.push(KeyValue::new(
                FIELD_EXCEPTION_STACKTRACE,
                Value::Array(chain.clone().into()),
            ));
        }

        if self.exception_config.propagate
            && let Some(otel_data) = self.otel_data.as_deref_mut()
        {
            otel_data.upsert_attribute(KeyValue::new(FIELD_EXCEPTION_MESSAGE, error_msg.clone()));

            // NOTE: This is actually not the stacktrace of the exception. This is
            // the "source chain". It represents the hierarchy of errors from the
            // app level to the lowest level such as IO. It does not represent all
            // of the callsites in the code that led to the error happening.
            // `std::error::Error::backtrace` is a nightly-only API and cannot be
            // used here until the feature is stabilized.
            otel_data.upsert_attribute(KeyValue::new(
                FIELD_EXCEPTION_STACKTRACE,
                Value::Array(chain.clone().into()),
            ));
        }

        self.event_builder
            .attributes
            .push(KeyValue::new(field.name(), error_msg));
        self.event_builder.attributes.push(KeyValue::new(
            format!("{}.chain", field.name()),
            Value::Array(chain.into()),
        ));
    }
}

/// Control over opentelemetry conventional exception fields
#[derive(Clone, Copy)]
struct ExceptionFieldConfig {
    /// If an error value is recorded on an event/span, should the otel fields
    /// be added
    record: bool,

    /// If an error value is recorded on an event, should the otel fields be
    /// added to the corresponding span
    propagate: bool,
}

/// Visitor applying `tracing` field values while a span is still being assembled
/// into a `SpanBuilder`, before it's built for real.
struct NewSpanAttributeVisitor<'a> {
    builder: &'a mut otel::SpanBuilder,
    status: &'a mut otel::Status,
    attributes: &'a mut Vec<KeyValue>,
    exception_config: ExceptionFieldConfig,
}

impl NewSpanAttributeVisitor<'_> {
    /// Adds or replaces `kv` by key in both the local attribute mirror and the span builder.
    fn push_attribute(&mut self, kv: KeyValue) {
        upsert_attribute(self.attributes, kv.clone());
        upsert_attribute(self.builder.attributes.get_or_insert_with(Vec::new), kv);
    }
}

impl field::Visit for NewSpanAttributeVisitor<'_> {
    /// Set attributes on the underlying OpenTelemetry [`Span`] from `bool` values.
    ///
    /// [`Span`]: opentelemetry::trace::Span
    fn record_bool(&mut self, field: &field::Field, value: bool) {
        self.push_attribute(KeyValue::new(field.name(), value));
    }

    /// Set attributes on the underlying OpenTelemetry [`Span`] from `f64` values.
    ///
    /// [`Span`]: opentelemetry::trace::Span
    fn record_f64(&mut self, field: &field::Field, value: f64) {
        self.push_attribute(KeyValue::new(field.name(), value));
    }

    /// Set attributes on the underlying OpenTelemetry [`Span`] from `i64` values.
    ///
    /// [`Span`]: opentelemetry::trace::Span
    fn record_i64(&mut self, field: &field::Field, value: i64) {
        self.push_attribute(KeyValue::new(field.name(), value));
    }

    /// Set attributes on the underlying OpenTelemetry [`Span`] from `&str` values.
    ///
    /// [`Span`]: opentelemetry::trace::Span
    fn record_str(&mut self, field: &field::Field, value: &str) {
        match field.name() {
            OTEL_NAME => self.builder.name = value.to_string().into(),
            OTEL_KIND => self.builder.span_kind = str_to_span_kind(value),
            OTEL_STATUS_CODE => *self.status = str_to_status(value),
            OTEL_STATUS_MESSAGE => *self.status = otel::Status::error(value.to_string()),
            _ => self.push_attribute(KeyValue::new(field.name(), value.to_string())),
        }
    }

    /// Set attributes on the underlying OpenTelemetry [`Span`] from values that
    /// implement Debug.
    ///
    /// [`Span`]: opentelemetry::trace::Span
    fn record_debug(&mut self, field: &field::Field, value: &dyn fmt::Debug) {
        match field.name() {
            OTEL_NAME => self.builder.name = format!("{value:?}").into(),
            OTEL_KIND => self.builder.span_kind = str_to_span_kind(&format!("{value:?}")),
            OTEL_STATUS_CODE => *self.status = str_to_status(&format!("{value:?}")),
            OTEL_STATUS_MESSAGE => *self.status = otel::Status::error(format!("{value:?}")),
            _ => self.push_attribute(KeyValue::new(field.name(), format!("{value:?}"))),
        }
    }

    /// Set attributes on the underlying OpenTelemetry [`Span`] using a [`std::error::Error`]'s
    /// [`std::fmt::Display`] implementation. Also adds the `source` chain as an extra field
    ///
    /// [`Span`]: opentelemetry::trace::Span
    fn record_error(
        &mut self,
        field: &tracing_core::Field,
        value: &(dyn std::error::Error + 'static),
    ) {
        let mut chain: Vec<StringValue> = Vec::new();
        let mut next_err = value.source();

        while let Some(err) = next_err {
            chain.push(err.to_string().into());
            next_err = err.source();
        }

        let error_msg = value.to_string();

        if self.exception_config.record {
            self.push_attribute(KeyValue::new(FIELD_EXCEPTION_MESSAGE, error_msg.clone()));

            // NOTE: This is actually not the stacktrace of the exception. This is
            // the "source chain". It represents the hierarchy of errors from the
            // app level to the lowest level such as IO. It does not represent all
            // of the callsites in the code that led to the error happening.
            // `std::error::Error::backtrace` is a nightly-only API and cannot be
            // used here until the feature is stabilized.
            self.push_attribute(KeyValue::new(
                FIELD_EXCEPTION_STACKTRACE,
                Value::Array(chain.clone().into()),
            ));
        }

        self.push_attribute(KeyValue::new(field.name(), error_msg));
        self.push_attribute(KeyValue::new(
            format!("{}.chain", field.name()),
            Value::Array(chain.into()),
        ));
    }
}

/// Visitor applying `tracing` field values onto an already-live span.
///
/// `otel.kind` and `otel.name` are no-ops here: a live span's kind can't be changed
/// (no public API) and its current name can't be read back.
struct SpanAttributeVisitor<'a> {
    otel_data: &'a mut OtelData,
    exception_config: ExceptionFieldConfig,
}

impl SpanAttributeVisitor<'_> {
    fn set_status(&mut self, status: otel::Status) {
        self.otel_data.current_cx.span().set_status(status);
    }
}

impl field::Visit for SpanAttributeVisitor<'_> {
    /// Set attributes on the underlying OpenTelemetry [`Span`] from `bool` values.
    ///
    /// [`Span`]: opentelemetry::trace::Span
    fn record_bool(&mut self, field: &field::Field, value: bool) {
        self.otel_data
            .upsert_attribute(KeyValue::new(field.name(), value));
    }

    /// Set attributes on the underlying OpenTelemetry [`Span`] from `f64` values.
    ///
    /// [`Span`]: opentelemetry::trace::Span
    fn record_f64(&mut self, field: &field::Field, value: f64) {
        self.otel_data
            .upsert_attribute(KeyValue::new(field.name(), value));
    }

    /// Set attributes on the underlying OpenTelemetry [`Span`] from `i64` values.
    ///
    /// [`Span`]: opentelemetry::trace::Span
    fn record_i64(&mut self, field: &field::Field, value: i64) {
        self.otel_data
            .upsert_attribute(KeyValue::new(field.name(), value));
    }

    /// Set attributes on the underlying OpenTelemetry [`Span`] from `&str` values.
    ///
    /// [`Span`]: opentelemetry::trace::Span
    fn record_str(&mut self, field: &field::Field, value: &str) {
        match field.name() {
            OTEL_NAME => self
                .otel_data
                .current_cx
                .span()
                .update_name(value.to_string()),
            OTEL_KIND => {}
            OTEL_STATUS_CODE => self.set_status(str_to_status(value)),
            OTEL_STATUS_MESSAGE => self.set_status(otel::Status::error(value.to_string())),
            _ => self
                .otel_data
                .upsert_attribute(KeyValue::new(field.name(), value.to_string())),
        }
    }

    /// Set attributes on the underlying OpenTelemetry [`Span`] from values that
    /// implement Debug.
    ///
    /// [`Span`]: opentelemetry::trace::Span
    fn record_debug(&mut self, field: &field::Field, value: &dyn fmt::Debug) {
        match field.name() {
            OTEL_NAME => self
                .otel_data
                .current_cx
                .span()
                .update_name(format!("{value:?}")),
            OTEL_KIND => {}
            OTEL_STATUS_CODE => self.set_status(str_to_status(&format!("{value:?}"))),
            OTEL_STATUS_MESSAGE => self.set_status(otel::Status::error(format!("{value:?}"))),
            _ => self
                .otel_data
                .upsert_attribute(KeyValue::new(field.name(), format!("{value:?}"))),
        }
    }

    /// Set attributes on the underlying OpenTelemetry [`Span`] using a [`std::error::Error`]'s
    /// [`std::fmt::Display`] implementation. Also adds the `source` chain as an extra field
    ///
    /// [`Span`]: opentelemetry::trace::Span
    fn record_error(
        &mut self,
        field: &tracing_core::Field,
        value: &(dyn std::error::Error + 'static),
    ) {
        let mut chain: Vec<StringValue> = Vec::new();
        let mut next_err = value.source();

        while let Some(err) = next_err {
            chain.push(err.to_string().into());
            next_err = err.source();
        }

        let error_msg = value.to_string();

        if self.exception_config.record {
            self.otel_data
                .upsert_attribute(KeyValue::new(FIELD_EXCEPTION_MESSAGE, error_msg.clone()));

            // NOTE: This is actually not the stacktrace of the exception. This is
            // the "source chain". It represents the hierarchy of errors from the
            // app level to the lowest level such as IO. It does not represent all
            // of the callsites in the code that led to the error happening.
            // `std::error::Error::backtrace` is a nightly-only API and cannot be
            // used here until the feature is stabilized.
            self.otel_data.upsert_attribute(KeyValue::new(
                FIELD_EXCEPTION_STACKTRACE,
                Value::Array(chain.clone().into()),
            ));
        }

        self.otel_data
            .upsert_attribute(KeyValue::new(field.name(), error_msg));
        self.otel_data.upsert_attribute(KeyValue::new(
            format!("{}.chain", field.name()),
            Value::Array(chain.into()),
        ));
    }
}

impl<S, T> OpenTelemetryLayer<S, T>
where
    S: Subscriber + for<'span> LookupSpan<'span>,
    T: otel::Tracer + 'static,
    T::Span: Send + Sync,
{
    /// Set the [`Tracer`] that this layer will use to produce and track
    /// OpenTelemetry [`Span`]s.
    ///
    /// [`Tracer`]: opentelemetry::trace::Tracer
    /// [`Span`]: opentelemetry::trace::Span
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use tracing_opentelemetry::OpenTelemetryLayer;
    /// use tracing_subscriber::layer::SubscriberExt;
    /// use tracing_subscriber::Registry;
    ///
    /// // Create a jaeger exporter pipeline for a `trace_demo` service.
    /// let tracer = opentelemetry_jaeger::new_agent_pipeline()
    ///     .with_service_name("trace_demo")
    ///     .install_simple()
    ///     .expect("Error initializing Jaeger exporter");
    ///
    /// // Create a layer with the configured tracer
    /// let otel_layer = OpenTelemetryLayer::new(tracer);
    ///
    /// // Use the tracing subscriber `Registry`, or any other subscriber
    /// // that impls `LookupSpan`
    /// let subscriber = Registry::default().with(otel_layer);
    /// # drop(subscriber);
    /// ```
    pub(crate) fn new(tracer: T) -> Self {
        OpenTelemetryLayer {
            tracer,
            force_sampling: false,
            location: true,
            tracked_inactivity: true,
            with_threads: true,
            exception_config: ExceptionFieldConfig {
                record: false,
                propagate: false,
            },
            get_context: WithContext(Self::get_context),
            _registry: marker::PhantomData,
        }
    }

    /// Set the [`Tracer`] that this layer will use to produce and track
    /// OpenTelemetry [`Span`]s.
    ///
    /// [`Tracer`]: opentelemetry::trace::Tracer
    /// [`Span`]: opentelemetry::trace::Span
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use tracing_subscriber::layer::SubscriberExt;
    /// use tracing_subscriber::Registry;
    ///
    /// // Create a jaeger exporter pipeline for a `trace_demo` service.
    /// let tracer = opentelemetry_jaeger::new_agent_pipeline()
    ///     .with_service_name("trace_demo")
    ///     .install_simple()
    ///     .expect("Error initializing Jaeger exporter");
    ///
    /// // Create a layer with the configured tracer
    /// let otel_layer = tracing_opentelemetry::layer().force_sampling().with_tracer(tracer);
    ///
    /// // Use the tracing subscriber `Registry`, or any other subscriber
    /// // that impls `LookupSpan`
    /// let subscriber = Registry::default().with(otel_layer);
    /// # drop(subscriber);
    /// ```
    pub(crate) fn with_tracer<Tracer>(self, tracer: Tracer) -> OpenTelemetryLayer<S, Tracer>
    where
        Tracer: otel::Tracer + 'static,
        Tracer::Span: Send + Sync,
    {
        OpenTelemetryLayer {
            tracer,
            force_sampling: self.force_sampling,
            location: self.location,
            tracked_inactivity: self.tracked_inactivity,
            with_threads: self.with_threads,
            exception_config: self.exception_config,
            get_context: WithContext(OpenTelemetryLayer::<S, Tracer>::get_context),
            _registry: self._registry,
        }
    }

    /// Sets whether or not span and event metadata should include OpenTelemetry
    /// exception fields such as `exception.message` and `exception.backtrace`
    /// when an `Error` value is recorded. If multiple error values are recorded
    /// on the same span/event, only the most recently recorded error value will
    /// show up under these fields.
    ///
    /// These attributes follow the [OpenTelemetry semantic conventions for
    /// exceptions][conv].
    ///
    /// By default, these attributes are not recorded.
    ///
    /// [conv]: https://github.com/open-telemetry/semantic-conventions/tree/main/docs/exceptions/
    #[allow(dead_code)]
    pub(crate) fn with_exception_fields(self, exception_fields: bool) -> Self {
        Self {
            exception_config: ExceptionFieldConfig {
                record: exception_fields,
                ..self.exception_config
            },
            ..self
        }
    }

    /// Sets whether or not reporting an `Error` value on an event will
    /// propagate the OpenTelemetry exception fields such as `exception.message`
    /// and `exception.backtrace` to the corresponding span. You do not need to
    /// enable `with_exception_fields` in order to enable this. If multiple
    /// error values are recorded on the same span/event, only the most recently
    /// recorded error value will show up under these fields.
    ///
    /// These attributes follow the [OpenTelemetry semantic conventions for
    /// exceptions][conv].
    ///
    /// By default, these attributes are not propagated to the span.
    ///
    /// [conv]: https://github.com/open-telemetry/semantic-conventions/tree/main/docs/exceptions/
    #[allow(dead_code)]
    pub(crate) fn with_exception_field_propagation(
        self,
        exception_field_propagation: bool,
    ) -> Self {
        Self {
            exception_config: ExceptionFieldConfig {
                propagate: exception_field_propagation,
                ..self.exception_config
            },
            ..self
        }
    }

    /// Sets whether or not spans metadata should include the _busy time_
    /// (total time for which it was entered), and _idle time_ (total time
    /// the span existed but was not entered).
    #[allow(dead_code)]
    pub(crate) fn with_tracked_inactivity(self, tracked_inactivity: bool) -> Self {
        Self {
            tracked_inactivity,
            ..self
        }
    }

    /// Sets whether or not spans record additional attributes for the thread
    /// name and thread ID of the thread they were created on, following the
    /// [OpenTelemetry semantic conventions for threads][conv].
    ///
    /// By default, thread attributes are enabled.
    ///
    /// [conv]: https://github.com/open-telemetry/semantic-conventions/blob/main/docs/general/attributes.md#general-thread-attributes/
    #[allow(dead_code)]
    pub(crate) fn with_threads(self, threads: bool) -> Self {
        Self {
            with_threads: threads,
            ..self
        }
    }

    /// Retrieve the parent OpenTelemetry [`Context`] from the current tracing
    /// [`span`] through the [`Registry`]. This [`Context`] links spans to their
    /// parent for proper hierarchical visualization.
    ///
    /// [`Context`]: opentelemetry::Context
    /// [`span`]: tracing::Span
    /// [`Registry`]: tracing_subscriber::Registry
    fn parent_context(&self, attrs: &Attributes<'_>, ctx: &Context<'_, S>) -> OtelContext {
        // If a span is specified, it _should_ exist in the underlying `Registry`.
        if let Some(parent) = attrs.parent() {
            let span = ctx.span(parent).expect("Span not found, this is a bug");
            let mut extensions = span.extensions_mut();
            extensions
                .get_mut::<OtelData>()
                .map(|data| data.current_cx.clone())
                .unwrap_or_default()
        // Else if the span is inferred from context, look up any available current span.
        } else if attrs.is_contextual() {
            ctx.lookup_current()
                .and_then(|span| {
                    let mut extensions = span.extensions_mut();
                    extensions
                        .get_mut::<OtelData>()
                        .map(|data| data.current_cx.clone())
                })
                .unwrap_or_else(OtelContext::current)
        // Explicit root spans should have no parent context.
        } else {
            OtelContext::new()
        }
    }

    fn get_context(dispatch: &tracing::Dispatch, id: &span::Id, f: &mut dyn FnMut(&mut OtelData)) {
        let subscriber = dispatch
            .downcast_ref::<S>()
            .expect("subscriber should downcast to expected type; this is a bug!");
        let span = subscriber
            .span(id)
            .expect("registry should have a span for the current ID");

        let mut extensions = span.extensions_mut();
        if let Some(otel_data) = extensions.get_mut::<OtelData>() {
            f(otel_data);
        }
    }

    fn extra_span_attrs(&self) -> usize {
        let mut extra_attrs = 0;
        if self.location {
            extra_attrs += 3;
        }
        if self.with_threads {
            extra_attrs += 2;
        }
        extra_attrs
    }
}

thread_local! {
    static THREAD_ID: unsync::Lazy<u64> = unsync::Lazy::new(|| {
        // OpenTelemetry's semantic conventions require the thread ID to be
        // recorded as an integer, but `std::thread::ThreadId` does not expose
        // the integer value on stable, so we have to convert it to a `usize` by
        // parsing it. Since this requires allocating a `String`, store it in a
        // thread local so we only have to do this once.
        // TODO(eliza): once `std::thread::ThreadId::as_u64` is stabilized
        // (https://github.com/rust-lang/rust/issues/67939), just use that.
        thread_id_integer(thread::current().id())
    });
}

impl<S, T> OpenTelemetryLayer<S, T>
where
    S: Subscriber + for<'span> LookupSpan<'span>,
    T: otel::Tracer + 'static,
    T::Span: Send + Sync,
{
    fn enabled(
        &self,
        meta: &tracing::Metadata<'_>,
        cx: &tracing_subscriber::layer::Context<'_, S>,
    ) -> bool {
        // we ignore metric events
        if !meta.is_span() {
            return meta.fields().iter().any(|f| f.name() == "message");
        }

        if self.force_sampling {
            return true;
        }

        // if there's an existing otel context set by the client request, and it is sampled,
        // then that trace is sampled
        let current_otel_context = opentelemetry::Context::current();
        if current_otel_context.span().span_context().is_sampled() {
            return true;
        }

        let current_span = cx.current_span();
        if let Some(spanref) = current_span
            // the current span, which is the parent of the span that might get enabled here,
            // exists, but it might have been enabled by another layer like metrics
            .id()
            .and_then(|id| cx.span(id))
        {
            return spanref.is_sampled();
        }

        // always sample the router loading trace
        if meta.name() == STARTING_SPAN_NAME {
            return true;
        }

        // we only make the sampling decision on the root span. If we reach here for any other span,
        // it means that the parent span was not enabled, so we should not enable this span either
        if meta.name() != REQUEST_SPAN_NAME
            && meta.name() != ROUTER_SPAN_NAME
            && meta.name() != SUBSCRIPTION_EVENT_SPAN_NAME
            && meta.name() != INVALIDATION_ENDPOINT_SPAN_NAME
        {
            return false;
        }

        // - there's no parent span (it's the root), so we make the sampling decision
        true
    }

    /// Check whether this span should be sampled by looking at `SampledSpan` in the span's
    /// extensions.
    ///
    /// # Panics
    ///
    /// This function takes (and then drops) a read lock on `Extensions`. Be careful with using it,
    /// since if you're already holding a write lock on `Extensions` the code can deadlock.
    fn sampled(span: &SpanRef<S>) -> bool {
        let extensions = span.extensions();
        extensions
            .get::<SampledSpan>()
            .map(|s| matches!(s, SampledSpan::Sampled))
            .unwrap_or(false)
    }
}

impl<S, T> Layer<S> for OpenTelemetryLayer<S, T>
where
    S: Subscriber + for<'span> LookupSpan<'span>,
    T: otel::Tracer + 'static,
    T::Span: Send + Sync,
{
    /// Creates an [OpenTelemetry `Span`] for the corresponding [tracing `Span`].
    ///
    /// [OpenTelemetry `Span`]: opentelemetry::trace::Span
    /// [tracing `Span`]: tracing::Span
    fn on_new_span(&self, attrs: &Attributes<'_>, id: &span::Id, ctx: Context<'_, S>) {
        if let Some(span) = ctx.span(id) {
            // NB: order matters here! `parent_context` will temporarily lock `extensions` and we
            // need to make sure that there isn't a lock already in place.
            let parent_cx = self.parent_context(attrs, &ctx);
            let mut extensions = span.extensions_mut();

            if !self.enabled(attrs.metadata(), &ctx) {
                // Nothing is ever exported for a span that isn't sampled, so it's safe to
                // fabricate a trace/span id here purely for local log correlation. Prefer
                // inheriting the parent's trace id where there is one (e.g. an externally
                // propagated, deliberately-unsampled trace) for continuity.
                let id_generator = RandomIdGenerator::default();
                let parent_trace_id = parent_cx.span().span_context().trace_id();
                let trace_id = if parent_trace_id != otel::TraceId::INVALID {
                    parent_trace_id
                } else {
                    id_generator.new_trace_id()
                };
                extensions.insert(SampledSpan::NotSampled(
                    trace_id.to_bytes().into(),
                    id_generator.new_span_id(),
                ));

                // Inactivity may still be tracked even if the span isn't sampled.
                if self.tracked_inactivity && extensions.get_mut::<Timings>().is_none() {
                    extensions.insert(Timings::new());
                }
                return;
            }
            extensions.insert(SampledSpan::Sampled);

            if self.tracked_inactivity && extensions.get_mut::<Timings>().is_none() {
                extensions.insert(Timings::new());
            }

            let mut builder = self
                .tracer
                .span_builder(attrs.metadata().name())
                .with_start_time(SystemTime::now());

            let builder_attrs = builder.attributes.get_or_insert(Vec::with_capacity(
                attrs.fields().len() + self.extra_span_attrs(),
            ));

            if self.location {
                let meta = attrs.metadata();

                if let Some(filename) = meta.file() {
                    builder_attrs.push(KeyValue::new("code.filepath", filename));
                }

                if let Some(module) = meta.module_path() {
                    builder_attrs.push(KeyValue::new("code.namespace", module));
                }

                if let Some(line) = meta.line() {
                    builder_attrs.push(KeyValue::new("code.lineno", line as i64));
                }
            }

            if self.with_threads {
                THREAD_ID.with(|id| builder_attrs.push(KeyValue::new("thread.id", **id as i64)));
                if let Some(name) = std::thread::current().name() {
                    // TODO(eliza): it's a bummer that we have to allocate here, but
                    // we can't easily get the string as a `static`. it would be
                    // nice if `opentelemetry` could also take `Arc<str>`s as
                    // `String` values...
                    builder_attrs.push(KeyValue::new("thread.name", name.to_owned()));
                }
            }

            let mut attributes = builder.attributes.clone().unwrap_or_default();
            let mut status = otel::Status::Unset;

            attrs.record(&mut NewSpanAttributeVisitor {
                builder: &mut builder,
                status: &mut status,
                attributes: &mut attributes,
                exception_config: self.exception_config,
            });

            // Build the span for real immediately, rather than deferring to the first
            // access. Router code (log correlation, response headers) expects a
            // sampled span's trace/span id to be available as soon as it's created,
            // and the only way to guarantee that without risking it diverging from
            // the id actually exported is to build for real up front - `SpanBuilder`
            // no longer lets us force a specific trace/span id into a deferred build
            // (removed upstream in opentelemetry 0.32).
            let mut span = builder.start_with_context(&self.tracer, &parent_cx);
            span.set_status(status);

            extensions.insert(OtelData {
                current_cx: parent_cx.with_span(span),
                attributes,
                original_name: attrs.metadata().name(),
                event_attributes: None,
                forced_status: None,
                forced_span_name: None,
            });
        } else {
            eprintln!("OpenTelemetryLayer::on_new_span: Span not found, this is a bug");
        }
    }

    fn on_enter(&self, id: &span::Id, ctx: Context<'_, S>) {
        if !self.tracked_inactivity {
            return;
        }

        if let Some(span) = ctx.span(id) {
            if !Self::sampled(&span) {
                return;
            }

            let mut extensions = span.extensions_mut();
            if let Some(timings) = extensions.get_mut::<Timings>() {
                let now = Instant::now();
                timings.idle += (now - timings.last).as_nanos() as i64;
                timings.last = now;
            }
        } else {
            eprintln!("OpenTelemetryLayer::on_enter: Span not found, this is a bug");
        }
    }

    fn on_exit(&self, id: &span::Id, ctx: Context<'_, S>) {
        if !self.tracked_inactivity {
            return;
        }

        if let Some(span) = ctx.span(id) {
            if !Self::sampled(&span) {
                return;
            }

            let mut extensions = span.extensions_mut();
            if let Some(timings) = extensions.get_mut::<Timings>() {
                let now = Instant::now();
                timings.busy += (now - timings.last).as_nanos() as i64;
                timings.last = now;
            }
        } else {
            eprintln!("OpenTelemetryLayer::on_exit: Span not found, this is a bug");
        }
    }

    /// Record OpenTelemetry [`attributes`] for the given values.
    ///
    /// [`attributes`]: opentelemetry::trace::SpanBuilder::attributes
    fn on_record(&self, id: &Id, values: &Record<'_>, ctx: Context<'_, S>) {
        if let Some(span) = ctx.span(id) {
            if !Self::sampled(&span) {
                return;
            }

            let mut extensions = span.extensions_mut();
            if let Some(data) = extensions.get_mut::<OtelData>() {
                values.record(&mut SpanAttributeVisitor {
                    otel_data: data,
                    exception_config: self.exception_config,
                });
            }
        } else {
            eprintln!("OpenTelemetryLayer::on_record: Span not found, this is a bug");
        }
    }

    fn on_follows_from(&self, id: &Id, follows: &Id, ctx: Context<S>) {
        if let (Some(span), Some(follows_span)) = (ctx.span(id), ctx.span(follows)) {
            if !Self::sampled(&span) {
                return;
            }

            // NB: inside block so that `follows_span.extensions_mut()` will be dropped before
            // `span.extensions_mut()` is called later.
            let follows_context = {
                let mut follows_extensions = follows_span.extensions_mut();
                let follows_data = follows_extensions
                    .get_mut::<OtelData>()
                    .expect("Missing otel data span extensions");

                follows_data.current_cx.span().span_context().clone()
            };

            let mut extensions = span.extensions_mut();
            let data = extensions
                .get_mut::<OtelData>()
                .expect("Missing otel data span extensions");

            data.current_cx.span().add_link(follows_context, Vec::new());
        } else {
            eprintln!("OpenTelemetryLayer::on_follows_from: Span not found, this is a bug");
        }
    }

    /// Records OpenTelemetry [`Event`] data on event.
    ///
    /// Note: an [`ERROR`]-level event will also set the OpenTelemetry span status code to
    /// [`Error`], signaling that an error has occurred.
    ///
    /// [`Event`]: opentelemetry::trace::Event
    /// [`ERROR`]: tracing::Level::ERROR
    /// [`Error`]: opentelemetry::trace::StatusCode::Error
    fn on_event(&self, event: &Event<'_>, ctx: Context<'_, S>) {
        // Ignore events that are not in the context of a span
        if let Some(span) = ctx.lookup_current() {
            if !Self::sampled(&span) {
                return;
            }

            // Performing read operations before getting a write lock to avoid a deadlock
            // See https://github.com/tokio-rs/tracing/issues/763
            let meta = event.metadata();
            let target = KeyValue::new("target", meta.target());

            let mut extensions = span.extensions_mut();
            let mut otel_data = extensions.get_mut::<OtelData>();

            let mut otel_event = otel::Event::new(
                String::new(),
                SystemTime::now(),
                vec![KeyValue::new("level", meta.level().as_str()), target],
                0,
            );
            let mut span_event_visit = SpanEventVisitor {
                event_builder: &mut otel_event,
                otel_data: otel_data.as_deref_mut(),
                exception_config: self.exception_config,
                custom_event: false,
            };
            event.record(&mut span_event_visit);
            let custom_event = span_event_visit.custom_event;
            // Add custom event attributes for this event
            if custom_event {
                let event_attributes = otel_data.as_ref().and_then(|o| o.event_attributes.clone());

                if let Some(event_attributes) = event_attributes {
                    otel_event.attributes.extend(
                        event_attributes
                            .into_iter()
                            .map(|(k, v)| KeyValue::new(k, v)),
                    )
                }
            }

            if self.location {
                let (file, module) = (
                    event.metadata().file().map(Value::from),
                    event.metadata().module_path().map(Value::from),
                );

                if let Some(file) = file {
                    otel_event
                        .attributes
                        .push(KeyValue::new("code.filepath", file));
                }
                if let Some(module) = module {
                    otel_event
                        .attributes
                        .push(KeyValue::new("code.namespace", module));
                }
                if let Some(line) = meta.line() {
                    otel_event
                        .attributes
                        .push(KeyValue::new("code.lineno", line as i64));
                }
            }

            if let Some(o) = otel_data {
                let live_span = o.current_cx.span();
                if *meta.level() == tracing_core::Level::ERROR {
                    live_span.set_status(otel::Status::error(""));
                }
                live_span.add_event(otel_event.name, otel_event.attributes);
            }
        };
    }

    /// Exports an OpenTelemetry [`Span`] on close.
    ///
    /// [`Span`]: opentelemetry::trace::Span
    fn on_close(&self, id: span::Id, ctx: Context<'_, S>) {
        if let Some(span) = ctx.span(&id) {
            if !Self::sampled(&span) {
                return;
            }

            let mut extensions = span.extensions_mut();
            if let Some(OtelData {
                current_cx,
                forced_status,
                forced_span_name,
                original_name,
                ..
            }) = extensions.remove::<OtelData>()
            {
                let timing_attrs = if self.tracked_inactivity {
                    extensions.remove::<Timings>().map(|timings| {
                        vec![
                            KeyValue::new(Key::new("busy_ns"), timings.busy),
                            KeyValue::new(Key::new("idle_ns"), timings.idle),
                        ]
                    })
                } else {
                    None
                };

                let live_span = current_cx.span();
                if let Some(timing_attrs) = timing_attrs {
                    live_span.set_attributes(timing_attrs);
                }
                if let Some(forced_status) = forced_status {
                    live_span.set_status(forced_status);
                }
                if let Some(forced_span_name) = forced_span_name {
                    // There's no way to read the span's current name back once it's
                    // live, so we rely on the name captured up front in
                    // `OtelData::original_name` instead.
                    live_span.set_attribute(KeyValue::new(OTEL_ORIGINAL_NAME, original_name));
                    live_span.update_name(forced_span_name);
                }
                live_span.end_with_timestamp(SystemTime::now());
            }
        } else {
            eprintln!("OpenTelemetryLayer::on_close: Span not found, this is a bug");
        }
    }

    // SAFETY: this is safe because the `WithContext` function pointer is valid
    // for the lifetime of `&self`.
    unsafe fn downcast_raw(&self, id: TypeId) -> Option<*const ()> {
        match id {
            id if id == TypeId::of::<Self>() => Some(self as *const _ as *const ()),
            id if id == TypeId::of::<WithContext>() => {
                Some(&self.get_context as *const _ as *const ())
            }
            _ => None,
        }
    }
}

struct Timings {
    idle: i64,
    busy: i64,
    last: Instant,
}

impl Timings {
    fn new() -> Self {
        Self {
            idle: 0,
            busy: 0,
            last: Instant::now(),
        }
    }
}

fn thread_id_integer(id: thread::ThreadId) -> u64 {
    let thread_id = format!("{id:?}");
    thread_id
        .trim_start_matches("ThreadId(")
        .trim_end_matches(')')
        .parse::<u64>()
        .expect("thread ID should parse as an integer")
}

#[cfg(test)]
mod tests {
    use std::borrow::Cow;
    use std::collections::HashMap;
    use std::error::Error;
    use std::fmt::Display;
    use std::sync::Arc;
    use std::thread;
    use std::time::SystemTime;

    use opentelemetry::StringValue;
    use opentelemetry::trace::TraceFlags;
    use parking_lot::Mutex;
    use tracing_subscriber::prelude::*;

    use super::*;
    use crate::plugins::telemetry::OTEL_NAME;
    use crate::plugins::telemetry::dynamic_attribute::SpanDynAttribute;

    /// Span data captured by [`TestTracer`] for inspection in tests.
    #[derive(Debug, Clone)]
    struct TestSpanRecord {
        builder: otel::SpanBuilder,
        status: otel::Status,
        parent_cx: OtelContext,
    }

    /// Test span that records mutations into the shared [`TestSpanRecord`].
    #[derive(Debug, Clone)]
    struct TestBuiltSpan(Arc<Mutex<Option<TestSpanRecord>>>, otel::SpanContext);
    impl otel::Span for TestBuiltSpan {
        fn add_event_with_timestamp<T: Into<Cow<'static, str>>>(
            &mut self,
            _: T,
            _: SystemTime,
            _: Vec<KeyValue>,
        ) {
        }
        fn span_context(&self) -> &otel::SpanContext {
            &self.1
        }
        fn is_recording(&self) -> bool {
            // `Span::set_attributes`'s default impl only applies attributes when the
            // span reports itself as recording, matching a real active span.
            true
        }
        fn set_attribute(&mut self, attribute: KeyValue) {
            if let Some(record) = self.0.lock().as_mut() {
                record
                    .builder
                    .attributes
                    .get_or_insert_with(Vec::new)
                    .push(attribute);
            }
        }
        fn set_status(&mut self, status: otel::Status) {
            if let Some(record) = self.0.lock().as_mut() {
                record.status = status;
            }
        }
        fn update_name<T: Into<Cow<'static, str>>>(&mut self, new_name: T) {
            if let Some(record) = self.0.lock().as_mut() {
                record.builder.name = new_name.into();
            }
        }
        fn end_with_timestamp(&mut self, _timestamp: SystemTime) {}
        fn add_link(&mut self, span_context: otel::SpanContext, attributes: Vec<KeyValue>) {
            if let Some(record) = self.0.lock().as_mut() {
                record
                    .builder
                    .links
                    .get_or_insert_with(Vec::new)
                    .push(otel::Link::new(span_context, attributes, 0));
            }
        }
    }

    #[derive(Debug, Clone)]
    struct TestTracer(Arc<Mutex<Option<TestSpanRecord>>>);
    impl otel::Tracer for TestTracer {
        type Span = TestBuiltSpan;
        fn start_with_context<T>(&self, _name: T, _context: &OtelContext) -> Self::Span
        where
            T: Into<Cow<'static, str>>,
        {
            TestBuiltSpan(self.0.clone(), otel::SpanContext::empty_context())
        }
        fn span_builder<T>(&self, name: T) -> otel::SpanBuilder
        where
            T: Into<Cow<'static, str>>,
        {
            otel::SpanBuilder::from_name(name)
        }
        fn build_with_context(
            &self,
            builder: otel::SpanBuilder,
            parent_cx: &OtelContext,
        ) -> Self::Span {
            *self.0.lock() = Some(TestSpanRecord {
                builder,
                status: otel::Status::Unset,
                parent_cx: parent_cx.clone(),
            });
            TestBuiltSpan(self.0.clone(), otel::SpanContext::empty_context())
        }
    }

    impl TestTracer {
        fn with_data<T>(&self, f: impl FnOnce(&otel::SpanBuilder, &otel::Status) -> T) -> T {
            let lock = self.0.lock();
            let record = lock.as_ref().expect("no span data has been recorded yet");
            f(&record.builder, &record.status)
        }

        fn parent_cx(&self) -> OtelContext {
            let lock = self.0.lock();
            lock.as_ref()
                .expect("no span data has been recorded yet")
                .parent_cx
                .clone()
        }
    }

    #[derive(Debug, Clone)]
    struct TestSpan(otel::SpanContext);
    impl otel::Span for TestSpan {
        fn add_event_with_timestamp<T: Into<Cow<'static, str>>>(
            &mut self,
            _: T,
            _: SystemTime,
            _: Vec<KeyValue>,
        ) {
        }
        fn span_context(&self) -> &otel::SpanContext {
            &self.0
        }
        fn is_recording(&self) -> bool {
            false
        }
        fn set_attribute(&mut self, _attribute: KeyValue) {}
        fn set_status(&mut self, _status: otel::Status) {}
        fn update_name<T: Into<Cow<'static, str>>>(&mut self, _new_name: T) {}
        fn end_with_timestamp(&mut self, _timestamp: SystemTime) {}
        fn add_link(&mut self, _span_context: otel::SpanContext, _attributes: Vec<KeyValue>) {}
    }

    #[derive(Debug)]
    struct TestDynError {
        msg: &'static str,
        source: Option<Box<TestDynError>>,
    }
    impl Display for TestDynError {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            write!(f, "{}", self.msg)
        }
    }
    impl Error for TestDynError {
        fn source(&self) -> Option<&(dyn Error + 'static)> {
            match &self.source {
                Some(source) => Some(source),
                None => None,
            }
        }
    }
    impl TestDynError {
        fn new(msg: &'static str) -> Self {
            Self { msg, source: None }
        }
        fn with_parent(self, parent_msg: &'static str) -> Self {
            Self {
                msg: parent_msg,
                source: Some(Box::new(self)),
            }
        }
    }

    #[test]
    fn dynamic_span_names() {
        let dynamic_name = "GET http://example.com".to_string();
        let tracer = TestTracer(Arc::new(Mutex::new(None)));
        let subscriber = tracing_subscriber::registry()
            .with(layer().force_sampling().with_tracer(tracer.clone()));

        tracing::subscriber::with_default(subscriber, || {
            tracing::debug_span!("static_name", otel.name = dynamic_name.as_str());
        });

        let recorded_name = tracer.with_data(|builder, _| builder.name.clone());
        assert_eq!(recorded_name, Cow::<str>::from(dynamic_name))
    }

    #[test]
    fn forced_dynamic_span_names() {
        let dynamic_name = "GET http://example.com".to_string();
        let forced_dynamic_name = "OVERRIDE GET http://example.com".to_string();
        let tracer = TestTracer(Arc::new(Mutex::new(None)));
        let subscriber = tracing_subscriber::registry()
            .with(layer().force_sampling().with_tracer(tracer.clone()));

        tracing::subscriber::with_default(subscriber, || {
            let span = tracing::debug_span!("static_name", otel.name = dynamic_name.as_str());
            let _entered = span.enter();
            span.set_span_dyn_attribute(
                Key::from_static_str(OTEL_NAME),
                opentelemetry::Value::String(forced_dynamic_name.clone().into()),
            );
        });

        // `on_close` renames the already-built span via `TestBuiltSpan::update_name`/
        // `set_attribute`, which route back into the shared `TestSpanRecord`.
        let (recorded_name, original_name_attr) = tracer.with_data(|builder, _| {
            (
                builder.name.clone(),
                builder.attributes.as_ref().and_then(|attrs| {
                    attrs
                        .iter()
                        .find(|kv| kv.key.as_str() == OTEL_ORIGINAL_NAME)
                        .map(|kv| kv.value.clone())
                }),
            )
        });
        assert_eq!(recorded_name, Cow::<str>::Owned(forced_dynamic_name));
        // The pre-rename name preserved under `OTEL_ORIGINAL_NAME` is the span's literal
        // metadata name ("static_name"), not `dynamic_name` - the `otel.name` field set
        // at span creation renames the builder directly (see `dynamic_span_names`
        // above), before `forced_span_name`/`OTEL_ORIGINAL_NAME` ever come into play.
        assert_eq!(
            original_name_attr,
            Some(opentelemetry::Value::String("static_name".into()))
        );
    }

    #[test]
    fn span_kind() {
        let tracer = TestTracer(Arc::new(Mutex::new(None)));
        let subscriber = tracing_subscriber::registry()
            .with(layer().force_sampling().with_tracer(tracer.clone()));

        tracing::subscriber::with_default(subscriber, || {
            tracing::debug_span!("request", otel.kind = "server");
        });

        let recorded_kind = tracer.with_data(|builder, _| builder.span_kind.clone());
        assert_eq!(recorded_kind, Some(otel::SpanKind::Server))
    }

    #[test]
    fn span_status_code() {
        let tracer = TestTracer(Arc::new(Mutex::new(None)));
        let subscriber = tracing_subscriber::registry()
            .with(layer().force_sampling().with_tracer(tracer.clone()));

        tracing::subscriber::with_default(subscriber, || {
            tracing::debug_span!("request", otel.status_code = ?otel::Status::Ok);
        });

        let recorded_status = tracer.with_data(|_, status| status.clone());
        assert_eq!(recorded_status, otel::Status::Ok)
    }

    #[test]
    fn span_status_message() {
        let tracer = TestTracer(Arc::new(Mutex::new(None)));
        let subscriber = tracing_subscriber::registry()
            .with(layer().force_sampling().with_tracer(tracer.clone()));

        let message = "message";

        tracing::subscriber::with_default(subscriber, || {
            tracing::debug_span!("request", otel.status_message = message);
        });

        let recorded_status_message = tracer.with_data(|_, status| status.clone());

        assert_eq!(recorded_status_message, otel::Status::error(message))
    }

    #[test]
    fn forced_dynamic_span_status() {
        // Unlike `otel.status_code` set as a span-creation field (covered by
        // `span_status_code` above, applied while still buffering into a builder),
        // `set_span_dyn_attribute` after creation only records `forced_status` on
        // `OtelData` - the span is already built (`Context`) by then, so `on_close`
        // applies it to the live span instead (mirrors the `OTEL_ORIGINAL_NAME` fix).
        let tracer = TestTracer(Arc::new(Mutex::new(None)));
        let subscriber = tracing_subscriber::registry()
            .with(layer().force_sampling().with_tracer(tracer.clone()));

        tracing::subscriber::with_default(subscriber, || {
            let span = tracing::debug_span!("request");
            let _entered = span.enter();
            span.set_span_dyn_attribute(
                Key::from_static_str(OTEL_STATUS_CODE),
                opentelemetry::Value::String("error".into()),
            );
        });

        let recorded_status = tracer.with_data(|_, status| status.clone());
        assert_eq!(recorded_status, otel::Status::error(""))
    }

    #[test]
    fn trace_id_from_existing_context() {
        let tracer = TestTracer(Arc::new(Mutex::new(None)));
        let subscriber = tracing_subscriber::registry()
            .with(layer().force_sampling().with_tracer(tracer.clone()));
        let trace_id = otel::TraceId::from(42u128);
        let existing_cx = OtelContext::current_with_span(TestSpan(otel::SpanContext::new(
            trace_id,
            otel::SpanId::from(1u64),
            TraceFlags::default(),
            false,
            Default::default(),
        )));
        let _g = existing_cx.attach();

        tracing::subscriber::with_default(subscriber, || {
            tracing::debug_span!("request", otel.kind = "server");
        });

        let recorded_trace_id = tracer.parent_cx().span().span_context().trace_id();
        assert_eq!(recorded_trace_id, trace_id)
    }

    #[test]
    fn includes_timings() {
        let tracer = TestTracer(Arc::new(Mutex::new(None)));
        let subscriber = tracing_subscriber::registry().with(
            layer()
                .force_sampling()
                .with_tracer(tracer.clone())
                .with_tracked_inactivity(true),
        );

        tracing::subscriber::with_default(subscriber, || {
            tracing::debug_span!("request");
        });

        let attributes =
            tracer.with_data(|builder, _| builder.attributes.as_ref().unwrap().clone());
        let keys = attributes
            .iter()
            .map(|kv| kv.key.as_str())
            .collect::<Vec<&str>>();
        assert!(keys.contains(&"idle_ns"));
        assert!(keys.contains(&"busy_ns"));
    }

    #[test]
    fn records_error_fields() {
        let tracer = TestTracer(Arc::new(Mutex::new(None)));
        let subscriber = tracing_subscriber::registry().with(
            layer()
                .force_sampling()
                .with_tracer(tracer.clone())
                .with_exception_fields(true),
        );

        let err = TestDynError::new("base error")
            .with_parent("intermediate error")
            .with_parent("user error");

        tracing::subscriber::with_default(subscriber, || {
            tracing::debug_span!(
                "request",
                error = &err as &(dyn std::error::Error + 'static)
            );
        });

        let attributes =
            tracer.with_data(|builder, _| builder.attributes.as_ref().unwrap().clone());

        let key_values = attributes
            .into_iter()
            .map(|kv| (kv.key.to_string(), kv.value))
            .collect::<HashMap<_, _>>();

        assert_eq!(key_values["error"].as_str(), "user error");
        assert_eq!(
            key_values["error.chain"],
            Value::Array(
                vec![
                    StringValue::from("intermediate error"),
                    StringValue::from("base error")
                ]
                .into()
            )
        );

        assert_eq!(key_values[FIELD_EXCEPTION_MESSAGE].as_str(), "user error");
        assert_eq!(
            key_values[FIELD_EXCEPTION_STACKTRACE],
            Value::Array(
                vec![
                    StringValue::from("intermediate error"),
                    StringValue::from("base error")
                ]
                .into()
            )
        );
    }

    #[test]
    fn includes_span_location() {
        let tracer = TestTracer(Arc::new(Mutex::new(None)));
        let subscriber = tracing_subscriber::registry()
            .with(layer().force_sampling().with_tracer(tracer.clone()));

        tracing::subscriber::with_default(subscriber, || {
            tracing::debug_span!("request");
        });

        let attributes =
            tracer.with_data(|builder, _| builder.attributes.as_ref().unwrap().clone());
        let keys = attributes
            .iter()
            .map(|kv| kv.key.as_str())
            .collect::<Vec<&str>>();
        assert!(keys.contains(&"code.filepath"));
        assert!(keys.contains(&"code.namespace"));
        assert!(keys.contains(&"code.lineno"));
    }

    #[test]
    fn includes_thread() {
        let thread = thread::current();
        let expected_name = thread
            .name()
            .map(|name| Value::String(name.to_owned().into()));
        let expected_id = Value::I64(thread_id_integer(thread.id()) as i64);

        let tracer = TestTracer(Arc::new(Mutex::new(None)));
        let subscriber = tracing_subscriber::registry().with(
            layer()
                .force_sampling()
                .with_tracer(tracer.clone())
                .with_threads(true),
        );

        tracing::subscriber::with_default(subscriber, || {
            tracing::debug_span!("request");
        });

        let attributes = tracer
            .with_data(|builder, _| builder.attributes.as_ref().unwrap().clone())
            .drain(..)
            .map(|kv| (kv.key.to_string(), kv.value))
            .collect::<HashMap<_, _>>();
        assert_eq!(attributes.get("thread.name"), expected_name.as_ref());
        assert_eq!(attributes.get("thread.id"), Some(&expected_id));
    }

    #[test]
    fn excludes_thread() {
        let tracer = TestTracer(Arc::new(Mutex::new(None)));
        let subscriber = tracing_subscriber::registry().with(
            layer()
                .force_sampling()
                .with_tracer(tracer.clone())
                .with_threads(false),
        );

        tracing::subscriber::with_default(subscriber, || {
            tracing::debug_span!("request");
        });

        let attributes =
            tracer.with_data(|builder, _| builder.attributes.as_ref().unwrap().clone());
        let keys = attributes
            .iter()
            .map(|kv| kv.key.as_str())
            .collect::<Vec<&str>>();
        assert!(!keys.contains(&"thread.name"));
        assert!(!keys.contains(&"thread.id"));
    }

    #[test]
    fn propagates_error_fields_from_event_to_span() {
        let tracer = TestTracer(Arc::new(Mutex::new(None)));
        let subscriber = tracing_subscriber::registry().with(
            layer()
                .force_sampling()
                .with_tracer(tracer.clone())
                .with_exception_field_propagation(true),
        );

        let err = TestDynError::new("base error")
            .with_parent("intermediate error")
            .with_parent("user error");

        tracing::subscriber::with_default(subscriber, || {
            let _guard = tracing::debug_span!("request",).entered();

            tracing::error!(
                error = &err as &(dyn std::error::Error + 'static),
                "request error!"
            )
        });

        let attributes =
            tracer.with_data(|builder, _| builder.attributes.as_ref().unwrap().clone());

        let key_values = attributes
            .into_iter()
            .map(|kv| (kv.key.to_string(), kv.value))
            .collect::<HashMap<_, _>>();

        assert_eq!(key_values[FIELD_EXCEPTION_MESSAGE].as_str(), "user error");
        assert_eq!(
            key_values[FIELD_EXCEPTION_STACKTRACE],
            Value::Array(
                vec![
                    StringValue::from("intermediate error"),
                    StringValue::from("base error")
                ]
                .into()
            )
        );
    }
}
