use std::any::TypeId;
use std::fmt;
use std::marker;
use std::sync::atomic::Ordering;
use std::thread;
use std::time::Instant;
use std::time::SystemTime;

use once_cell::unsync;
use opentelemetry::trace::noop;
use opentelemetry::trace::OrderMap;
use opentelemetry::trace::TraceContextExt;
use opentelemetry::trace::{self as otel};
use opentelemetry::Context as OtelContext;
use opentelemetry::Key;
use opentelemetry::KeyValue;
use opentelemetry::StringValue;
use opentelemetry::Value;
use rand::thread_rng;
use rand::Rng;
use tracing_core::field;
use tracing_core::span;
use tracing_core::span::Attributes;
use tracing_core::span::Id;
use tracing_core::span::Record;
use tracing_core::Event;
use tracing_core::Subscriber;
use tracing_subscriber::layer::Context;
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::Layer;

use super::OtelData;
use super::PreSampledTracer;
use crate::axum_factory::utils::REQUEST_SPAN_NAME;
use crate::plugins::telemetry::config::Sampler;
use crate::plugins::telemetry::config::SamplerOption;
use crate::plugins::telemetry::reload::IsSampled;
use crate::plugins::telemetry::reload::SampledSpan;
use crate::plugins::telemetry::reload::SPAN_SAMPLING_RATE;
use crate::plugins::telemetry::ROUTER_SPAN_NAME;
use crate::query_planner::subscription::SUBSCRIPTION_EVENT_SPAN_NAME;
use crate::router_factory::STARTING_SPAN_NAME;

pub(crate) const SPAN_NAME_FIELD: &str = "otel.name";
pub(crate) const SPAN_KIND_FIELD: &str = "otel.kind";
pub(crate) const SPAN_STATUS_CODE_FIELD: &str = "otel.status_code";
pub(crate) const SPAN_STATUS_MESSAGE_FIELD: &str = "otel.status_message";

const FIELD_EXCEPTION_MESSAGE: &str = "exception.message";
const FIELD_EXCEPTION_STACKTRACE: &str = "exception.stacktrace";

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
    fn(&tracing::Dispatch, &span::Id, f: &mut dyn FnMut(&mut OtelData, &dyn PreSampledTracer)),
);

impl WithContext {
    // This function allows a function to be called in the context of the
    // "remembered" subscriber.
    pub(crate) fn with_context(
        &self,
        dispatch: &tracing::Dispatch,
        id: &span::Id,
        mut f: impl FnMut(&mut OtelData, &dyn PreSampledTracer),
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
    span_builder: Option<&'b mut otel::SpanBuilder>,
    exception_config: ExceptionFieldConfig,
    custom_event: bool,
}

impl<'a, 'b> field::Visit for SpanEventVisitor<'a, 'b> {
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
            "message" => self.event_builder.name = format!("{:?}", value).into(),
            name => {
                if name == "kind" {
                    self.custom_event = true;
                }
                self.event_builder
                    .attributes
                    .push(KeyValue::new(name, format!("{:?}", value)));
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
                .push(Key::new(FIELD_EXCEPTION_MESSAGE).string(error_msg.clone()));

            // NOTE: This is actually not the stacktrace of the exception. This is
            // the "source chain". It represents the heirarchy of errors from the
            // app level to the lowest level such as IO. It does not represent all
            // of the callsites in the code that led to the error happening.
            // `std::error::Error::backtrace` is a nightly-only API and cannot be
            // used here until the feature is stabilized.
            self.event_builder
                .attributes
                .push(Key::new(FIELD_EXCEPTION_STACKTRACE).array(chain.clone()));
        }

        if self.exception_config.propagate {
            if let Some(span) = &mut self.span_builder {
                if let Some(attrs) = span.attributes.as_mut() {
                    attrs.insert(
                        Key::new(FIELD_EXCEPTION_MESSAGE),
                        Value::String(error_msg.clone().into()),
                    );

                    // NOTE: This is actually not the stacktrace of the exception. This is
                    // the "source chain". It represents the heirarchy of errors from the
                    // app level to the lowest level such as IO. It does not represent all
                    // of the callsites in the code that led to the error happening.
                    // `std::error::Error::backtrace` is a nightly-only API and cannot be
                    // used here until the feature is stabilized.
                    attrs.insert(
                        Key::new(FIELD_EXCEPTION_STACKTRACE),
                        Value::Array(chain.clone().into()),
                    );
                }
            }
        }

        self.event_builder
            .attributes
            .push(Key::new(field.name()).string(error_msg));
        self.event_builder
            .attributes
            .push(Key::new(format!("{}.chain", field.name())).array(chain));
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

struct SpanAttributeVisitor<'a> {
    span_builder: &'a mut otel::SpanBuilder,
    exception_config: ExceptionFieldConfig,
}

impl<'a> SpanAttributeVisitor<'a> {
    fn record(&mut self, attribute: KeyValue) {
        debug_assert!(self.span_builder.attributes.is_some());
        if let Some(v) = self.span_builder.attributes.as_mut() {
            v.insert(attribute.key, attribute.value);
        }
    }
}

impl<'a> field::Visit for SpanAttributeVisitor<'a> {
    /// Set attributes on the underlying OpenTelemetry [`Span`] from `bool` values.
    ///
    /// [`Span`]: opentelemetry::trace::Span
    fn record_bool(&mut self, field: &field::Field, value: bool) {
        self.record(KeyValue::new(field.name(), value));
    }

    /// Set attributes on the underlying OpenTelemetry [`Span`] from `f64` values.
    ///
    /// [`Span`]: opentelemetry::trace::Span
    fn record_f64(&mut self, field: &field::Field, value: f64) {
        self.record(KeyValue::new(field.name(), value));
    }

    /// Set attributes on the underlying OpenTelemetry [`Span`] from `i64` values.
    ///
    /// [`Span`]: opentelemetry::trace::Span
    fn record_i64(&mut self, field: &field::Field, value: i64) {
        self.record(KeyValue::new(field.name(), value));
    }

    /// Set attributes on the underlying OpenTelemetry [`Span`] from `&str` values.
    ///
    /// [`Span`]: opentelemetry::trace::Span
    fn record_str(&mut self, field: &field::Field, value: &str) {
        match field.name() {
            SPAN_NAME_FIELD => self.span_builder.name = value.to_string().into(),
            SPAN_KIND_FIELD => self.span_builder.span_kind = str_to_span_kind(value),
            SPAN_STATUS_CODE_FIELD => {
                self.span_builder.status = str_to_status(value);
            }
            SPAN_STATUS_MESSAGE_FIELD => {
                self.span_builder.status = otel::Status::error(value.to_string())
            }
            _ => self.record(KeyValue::new(field.name(), value.to_string())),
        }
    }

    /// Set attributes on the underlying OpenTelemetry [`Span`] from values that
    /// implement Debug.
    ///
    /// [`Span`]: opentelemetry::trace::Span
    fn record_debug(&mut self, field: &field::Field, value: &dyn fmt::Debug) {
        match field.name() {
            SPAN_NAME_FIELD => self.span_builder.name = format!("{:?}", value).into(),
            SPAN_KIND_FIELD => {
                self.span_builder.span_kind = str_to_span_kind(&format!("{:?}", value))
            }
            SPAN_STATUS_CODE_FIELD => {
                self.span_builder.status = str_to_status(&format!("{:?}", value))
            }
            SPAN_STATUS_MESSAGE_FIELD => {
                self.span_builder.status = otel::Status::error(format!("{:?}", value))
            }
            _ => self.record(Key::new(field.name()).string(format!("{:?}", value))),
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
            self.record(Key::new(FIELD_EXCEPTION_MESSAGE).string(error_msg.clone()));

            // NOTE: This is actually not the stacktrace of the exception. This is
            // the "source chain". It represents the heirarchy of errors from the
            // app level to the lowest level such as IO. It does not represent all
            // of the callsites in the code that led to the error happening.
            // `std::error::Error::backtrace` is a nightly-only API and cannot be
            // used here until the feature is stabilized.
            self.record(Key::new(FIELD_EXCEPTION_STACKTRACE).array(chain.clone()));
        }

        self.record(Key::new(field.name()).string(error_msg));
        self.record(Key::new(format!("{}.chain", field.name())).array(chain));
    }
}

impl<S, T> OpenTelemetryLayer<S, T>
where
    S: Subscriber + for<'span> LookupSpan<'span>,
    T: otel::Tracer + PreSampledTracer + 'static,
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
        Tracer: otel::Tracer + PreSampledTracer + 'static,
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
                .map(|builder| self.tracer.sampled_context(builder))
                .unwrap_or_default()
        // Else if the span is inferred from context, look up any available current span.
        } else if attrs.is_contextual() {
            ctx.lookup_current()
                .and_then(|span| {
                    let mut extensions = span.extensions_mut();
                    extensions
                        .get_mut::<OtelData>()
                        .map(|builder| self.tracer.sampled_context(builder))
                })
                .unwrap_or_else(OtelContext::current)
        // Explicit root spans should have no parent context.
        } else {
            OtelContext::new()
        }
    }

    fn get_context(
        dispatch: &tracing::Dispatch,
        id: &span::Id,
        f: &mut dyn FnMut(&mut OtelData, &dyn PreSampledTracer),
    ) {
        let subscriber = dispatch
            .downcast_ref::<S>()
            .expect("subscriber should downcast to expected type; this is a bug!");
        let span = subscriber
            .span(id)
            .expect("registry should have a span for the current ID");
        let layer = dispatch
            .downcast_ref::<OpenTelemetryLayer<S, T>>()
            .expect("layer should downcast to expected type; this is a bug!");

        let mut extensions = span.extensions_mut();
        if let Some(builder) = extensions.get_mut::<OtelData>() {
            f(builder, &layer.tracer);
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

pub(crate) fn configure(sampler: &SamplerOption) {
    let ratio = match sampler {
        SamplerOption::TraceIdRatioBased(ratio) => {
            // can't use std::cmp::min because f64 is not Ord
            if *ratio > 1.0 {
                1.0
            } else {
                *ratio
            }
        }
        SamplerOption::Always(s) => match s {
            Sampler::AlwaysOn => 1f64,
            Sampler::AlwaysOff => 0f64,
        },
    };

    SPAN_SAMPLING_RATE.store(f64::to_bits(ratio), Ordering::Relaxed);
}

impl<S, T> OpenTelemetryLayer<S, T> {
    fn sample(&self) -> bool {
        let s: f64 = thread_rng().gen_range(0.0..=1.0);
        s <= f64::from_bits(SPAN_SAMPLING_RATE.load(Ordering::Relaxed))
    }
}

impl<S, T> OpenTelemetryLayer<S, T>
where
    S: Subscriber + for<'span> LookupSpan<'span>,
    T: otel::Tracer + PreSampledTracer + 'static,
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

        // if there's an exsting otel context set by the client request, and it is sampled,
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
        {
            return false;
        }

        // - there's no parent span (it's the root), so we make the sampling decision
        self.sample()
    }
}

impl<S, T> Layer<S> for OpenTelemetryLayer<S, T>
where
    S: Subscriber + for<'span> LookupSpan<'span>,
    T: otel::Tracer + PreSampledTracer + 'static,
{
    /// Creates an [OpenTelemetry `Span`] for the corresponding [tracing `Span`].
    ///
    /// [OpenTelemetry `Span`]: opentelemetry::trace::Span
    /// [tracing `Span`]: tracing::Span
    fn on_new_span(&self, attrs: &Attributes<'_>, id: &span::Id, ctx: Context<'_, S>) {
        let span = ctx.span(id).expect("Span not found, this is a bug");
        let is_sampled = self.enabled(span.metadata(), &ctx);
        let mut extensions = span.extensions_mut();
        let parent_cx = self.parent_context(attrs, &ctx);

        // Record new trace id if there is no active parent span
        let trace_id = if parent_cx.span().span_context().is_valid() {
            // It probably means we have a remote parent trace
            parent_cx.span().span_context().trace_id()
        } else {
            let sampled_span = span
                .parent()
                .and_then(|s| s.extensions().get::<SampledSpan>().cloned());

            match sampled_span {
                // It's not the root span
                Some(SampledSpan::Sampled(trace_id, _) | SampledSpan::NotSampled(trace_id, _)) => {
                    opentelemetry_api::trace::TraceId::from_u128(trace_id.to_u128())
                }
                // It's probably the root span
                None => self.tracer.new_trace_id(),
            }
        };

        let span_id = self.tracer.new_span_id();
        let sampled = if is_sampled {
            SampledSpan::Sampled(trace_id.to_bytes().into(), span_id)
        } else {
            SampledSpan::NotSampled(trace_id.to_bytes().into(), span_id)
        };
        extensions.insert(sampled);

        if !is_sampled {
            // Nothing more to do as it's not sampled
            return;
        }

        if self.tracked_inactivity && extensions.get_mut::<Timings>().is_none() {
            extensions.insert(Timings::new());
        }

        let mut builder = self
            .tracer
            .span_builder(attrs.metadata().name())
            .with_start_time(SystemTime::now())
            // Eagerly assign span id so children have stable parent id
            .with_span_id(span_id)
            .with_trace_id(trace_id);

        let builder_attrs = builder.attributes.get_or_insert(OrderMap::with_capacity(
            attrs.fields().len() + self.extra_span_attrs(),
        ));

        if self.location {
            let meta = attrs.metadata();

            if let Some(filename) = meta.file() {
                builder_attrs.insert("code.filepath".into(), filename.into());
            }

            if let Some(module) = meta.module_path() {
                builder_attrs.insert("code.namespace".into(), module.into());
            }

            if let Some(line) = meta.line() {
                builder_attrs.insert("code.lineno".into(), (line as i64).into());
            }
        }

        if self.with_threads {
            THREAD_ID.with(|id| builder_attrs.insert("thread.id".into(), (**id as i64).into()));
            if let Some(name) = std::thread::current().name() {
                // TODO(eliza): it's a bummer that we have to allocate here, but
                // we can't easily get the string as a `static`. it would be
                // nice if `opentelemetry` could also take `Arc<str>`s as
                // `String` values...
                builder_attrs.insert("thread.name".into(), name.to_owned().into());
            }
        }

        attrs.record(&mut SpanAttributeVisitor {
            span_builder: &mut builder,
            exception_config: self.exception_config,
        });
        extensions.insert(OtelData {
            builder,
            parent_cx,
            event_attributes: None,
            forced_status: None,
            forced_span_name: None,
        });
    }

    fn on_enter(&self, id: &span::Id, ctx: Context<'_, S>) {
        if !self.tracked_inactivity {
            return;
        }

        let span = ctx.span(id).expect("Span not found, this is a bug");
        let mut extensions = span.extensions_mut();
        if extensions
            .get_mut::<SampledSpan>()
            .map(|s| matches!(s, SampledSpan::NotSampled(_, _)))
            .unwrap_or(true)
        {
            // It's not sampled
            return;
        }

        if let Some(timings) = extensions.get_mut::<Timings>() {
            let now = Instant::now();
            timings.idle += (now - timings.last).as_nanos() as i64;
            timings.last = now;
        }
    }

    fn on_exit(&self, id: &span::Id, ctx: Context<'_, S>) {
        if !self.tracked_inactivity {
            return;
        }

        let span = ctx.span(id).expect("Span not found, this is a bug");
        let mut extensions = span.extensions_mut();
        if extensions
            .get_mut::<SampledSpan>()
            .map(|s| matches!(s, SampledSpan::NotSampled(_, _)))
            .unwrap_or(true)
        {
            // It's not sampled
            return;
        }

        if let Some(timings) = extensions.get_mut::<Timings>() {
            let now = Instant::now();
            timings.busy += (now - timings.last).as_nanos() as i64;
            timings.last = now;
        }
    }

    /// Record OpenTelemetry [`attributes`] for the given values.
    ///
    /// [`attributes`]: opentelemetry::trace::SpanBuilder::attributes
    fn on_record(&self, id: &Id, values: &Record<'_>, ctx: Context<'_, S>) {
        let span = ctx.span(id).expect("Span not found, this is a bug");
        let mut extensions = span.extensions_mut();
        if extensions
            .get_mut::<SampledSpan>()
            .map(|s| matches!(s, SampledSpan::NotSampled(_, _)))
            .unwrap_or(true)
        {
            // It's not sampled
            return;
        }

        if let Some(data) = extensions.get_mut::<OtelData>() {
            values.record(&mut SpanAttributeVisitor {
                span_builder: &mut data.builder,
                exception_config: self.exception_config,
            });
        }
    }

    fn on_follows_from(&self, id: &Id, follows: &Id, ctx: Context<S>) {
        let span = ctx.span(id).expect("Span not found, this is a bug");
        let mut extensions = span.extensions_mut();
        if extensions
            .get_mut::<SampledSpan>()
            .map(|s| matches!(s, SampledSpan::NotSampled(_, _)))
            .unwrap_or(true)
        {
            // It's not sampled
            return;
        }

        let data = extensions
            .get_mut::<OtelData>()
            .expect("Missing otel data span extensions");

        let follows_span = ctx
            .span(follows)
            .expect("Span to follow not found, this is a bug");
        let mut follows_extensions = follows_span.extensions_mut();
        let follows_data = follows_extensions
            .get_mut::<OtelData>()
            .expect("Missing otel data span extensions");

        let follows_context = self
            .tracer
            .sampled_context(follows_data)
            .span()
            .span_context()
            .clone();
        let follows_link = otel::Link::new(follows_context, Vec::new());
        if let Some(ref mut links) = data.builder.links {
            links.push(follows_link);
        } else {
            data.builder.links = Some(vec![follows_link]);
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
            let mut extensions = span.extensions_mut();
            if extensions
                .get_mut::<SampledSpan>()
                .map(|s| matches!(s, SampledSpan::NotSampled(_, _)))
                .unwrap_or(true)
            {
                // It's not sampled
                return;
            }
            // Performing read operations before getting a write lock to avoid a deadlock
            // See https://github.com/tokio-rs/tracing/issues/763
            let meta = event.metadata();
            let target = Key::new("target");

            let target = target.string(meta.target());

            let mut otel_data = extensions.get_mut::<OtelData>();
            let span_builder = otel_data.as_mut().map(|o| &mut o.builder);

            let mut otel_event = otel::Event::new(
                String::new(),
                SystemTime::now(),
                vec![Key::new("level").string(meta.level().as_str()), target],
                0,
            );
            let mut span_event_visit = SpanEventVisitor {
                event_builder: &mut otel_event,
                span_builder,
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
                            .map(|(key, value)| KeyValue::new(key, value)),
                    )
                }
            }

            if let Some(OtelData { builder, .. }) = extensions.get_mut::<OtelData>() {
                if builder.status == otel::Status::Unset
                    && *meta.level() == tracing_core::Level::ERROR
                {
                    builder.status = otel::Status::error("")
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

                if let Some(ref mut events) = builder.events {
                    events.push(otel_event);
                } else {
                    builder.events = Some(vec![otel_event]);
                }
            }
        };
    }

    /// Exports an OpenTelemetry [`Span`] on close.
    ///
    /// [`Span`]: opentelemetry::trace::Span
    fn on_close(&self, id: span::Id, ctx: Context<'_, S>) {
        let span = ctx.span(&id).expect("Span not found, this is a bug");
        let mut extensions = span.extensions_mut();
        if extensions
            .get_mut::<SampledSpan>()
            .map(|s| matches!(s, SampledSpan::NotSampled(_, _)))
            .unwrap_or(true)
        {
            // It's not sampled
            return;
        }

        if let Some(OtelData {
            mut builder,
            parent_cx,
            forced_status,
            forced_span_name,
            ..
        }) = extensions.remove::<OtelData>()
        {
            if self.tracked_inactivity {
                // Append busy/idle timings when enabled.
                if let Some(timings) = extensions.get_mut::<Timings>() {
                    let busy_ns = Key::new("busy_ns");
                    let idle_ns = Key::new("idle_ns");

                    let attributes = builder
                        .attributes
                        .get_or_insert_with(|| OrderMap::with_capacity(2));
                    attributes.insert(busy_ns, timings.busy.into());
                    attributes.insert(idle_ns, timings.idle.into());
                }
            }
            if let Some(forced_status) = forced_status {
                builder.status = forced_status;
            }
            if let Some(forced_span_name) = forced_span_name {
                builder.name = forced_span_name.into();
            }

            // Assign end time, build and start span, drop span to export
            builder
                .with_end_time(SystemTime::now())
                .start_with_context(&self.tracer, &parent_cx);
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
    let thread_id = format!("{:?}", id);
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
    use std::sync::Mutex;
    use std::thread;
    use std::time::SystemTime;

    use opentelemetry::trace::noop;
    use opentelemetry::trace::TraceFlags;
    use opentelemetry::StringValue;
    use otel::SpanId;
    use tracing_subscriber::prelude::*;
    use tracing_subscriber::Registry;

    use super::*;
    use crate::plugins::telemetry::dynamic_attribute::DynSpanAttributeLayer;
    use crate::plugins::telemetry::dynamic_attribute::SpanDynAttribute;
    use crate::plugins::telemetry::reload::SampledSpan;
    use crate::tracer::TraceId;

    #[derive(Debug, Clone)]
    struct TestTracer(Arc<Mutex<Option<OtelData>>>);
    impl otel::Tracer for TestTracer {
        type Span = noop::NoopSpan;
        fn start_with_context<T>(&self, _name: T, _context: &OtelContext) -> Self::Span
        where
            T: Into<Cow<'static, str>>,
        {
            noop::NoopSpan::new()
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
            *self.0.lock().unwrap() = Some(OtelData {
                builder,
                parent_cx: parent_cx.clone(),
                event_attributes: None,
                forced_status: None,
                forced_span_name: None,
            });
            noop::NoopSpan::new()
        }
    }

    impl PreSampledTracer for TestTracer {
        fn sampled_context(&self, _builder: &mut super::OtelData) -> OtelContext {
            OtelContext::new()
        }
        fn new_trace_id(&self) -> otel::TraceId {
            otel::TraceId::INVALID
        }
        fn new_span_id(&self) -> otel::SpanId {
            otel::SpanId::INVALID
        }
    }

    impl TestTracer {
        fn with_data<T>(&self, f: impl FnOnce(&OtelData) -> T) -> T {
            let lock = self.0.lock().unwrap();
            let data = lock.as_ref().expect("no span data has been recorded yet");
            f(data)
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

        let recorded_name = tracer
            .0
            .lock()
            .unwrap()
            .as_ref()
            .map(|b| b.builder.name.clone());
        assert_eq!(recorded_name, Some(dynamic_name.into()))
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
                Key::from_static_str("otel.name"),
                opentelemetry::Value::String(forced_dynamic_name.clone().into()),
            );
        });

        let recorded_name = tracer
            .0
            .lock()
            .unwrap()
            .as_ref()
            .map(|b| b.builder.name.clone());
        assert_eq!(recorded_name, Some(Cow::Owned(forced_dynamic_name)))
    }

    #[test]
    fn span_kind() {
        let tracer = TestTracer(Arc::new(Mutex::new(None)));
        let subscriber = tracing_subscriber::registry()
            .with(layer().force_sampling().with_tracer(tracer.clone()));

        tracing::subscriber::with_default(subscriber, || {
            tracing::debug_span!("request", otel.kind = "server");
        });

        let recorded_kind = tracer.with_data(|data| data.builder.span_kind.clone());
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

        let recorded_status = tracer.with_data(|data| data.builder.status.clone());
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

        let recorded_status_message = tracer
            .0
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .builder
            .status
            .clone();

        assert_eq!(recorded_status_message, otel::Status::error(message))
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

        let recorded_trace_id =
            tracer.with_data(|data| data.parent_cx.span().span_context().trace_id());
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

        let attributes = tracer.with_data(|data| data.builder.attributes.as_ref().unwrap().clone());
        let keys = attributes
            .iter()
            .map(|(key, _)| key.as_str())
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

        let attributes = tracer
            .0
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .builder
            .attributes
            .as_ref()
            .unwrap()
            .clone();

        let key_values = attributes
            .into_iter()
            .map(|(key, value)| (key.as_str().to_owned(), value))
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

        let attributes = tracer.with_data(|data| data.builder.attributes.as_ref().unwrap().clone());
        let keys = attributes
            .iter()
            .map(|(key, _)| key.as_str())
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
            .with_data(|data| data.builder.attributes.as_ref().unwrap().clone())
            .drain(..)
            .map(|(key, value)| (key.as_str().to_string(), value))
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

        let attributes = tracer.with_data(|data| data.builder.attributes.as_ref().unwrap().clone());
        let keys = attributes
            .iter()
            .map(|(key, _)| key.as_str())
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

        let attributes = tracer
            .0
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .builder
            .attributes
            .as_ref()
            .unwrap()
            .clone();

        let key_values = attributes
            .into_iter()
            .map(|(key, value)| (key.as_str().to_owned(), value))
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
