use std::collections::HashMap;

use opentelemetry::Key;
use opentelemetry::KeyValue;
use tracing::field::Visit;
use tracing::Event;
use tracing_opentelemetry::OtelData;
use tracing_subscriber::layer::Context;
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::Layer;
use tracing_subscriber::Registry;

use super::reload::IsSampled;
use super::tracing::APOLLO_PRIVATE_PREFIX;

pub(crate) const APOLLO_PRIVATE_KIND: &str = "apollo_private.kind";

#[derive(Debug, Default)]
pub(crate) struct LogAttributes {
    attributes: Vec<KeyValue>,
}

impl LogAttributes {
    pub(crate) fn attributes(&self) -> &Vec<KeyValue> {
        &self.attributes
    }

    pub(crate) fn insert(&mut self, kv: KeyValue) {
        self.attributes.push(kv);
    }

    pub(crate) fn extend(&mut self, other: impl IntoIterator<Item = KeyValue>) {
        self.attributes.extend(other);
    }
}

/// To add dynamic attributes for spans
pub(crate) struct DynSpanAttributeLayer;

impl<S> Layer<S> for DynSpanAttributeLayer
where
    S: tracing_core::Subscriber + for<'lookup> tracing_subscriber::registry::LookupSpan<'lookup>,
{
    fn on_new_span(
        &self,
        _attrs: &tracing_core::span::Attributes<'_>,
        id: &tracing_core::span::Id,
        ctx: Context<'_, S>,
    ) {
        let span = ctx.span(id).expect("Span not found, this is a bug");
        let mut extensions = span.extensions_mut();
        if extensions.get_mut::<LogAttributes>().is_none() {
            extensions.insert(LogAttributes::default());
        }
    }
}

impl DynSpanAttributeLayer {
    pub(crate) fn new() -> Self {
        Self {}
    }
}

/// To add dynamic attributes for spans
pub(crate) trait SpanDynAttribute {
    fn set_span_dyn_attribute(&self, key: Key, value: opentelemetry::Value);
    fn set_span_dyn_attributes(&self, attributes: impl IntoIterator<Item = KeyValue>);
}

impl SpanDynAttribute for ::tracing::Span {
    fn set_span_dyn_attribute(&self, key: Key, value: opentelemetry::Value) {
        self.with_subscriber(move |(id, dispatch)| {
            if let Some(reg) = dispatch.downcast_ref::<Registry>() {
                match reg.span(id) {
                    None => eprintln!("no spanref, this is a bug"),
                    Some(s) => {
                        if s.is_sampled() {
                            let mut extensions = s.extensions_mut();
                            match extensions.get_mut::<OtelData>() {
                                Some(otel_data) => {
                                    if otel_data.builder.attributes.is_none() {
                                        otel_data.builder.attributes =
                                            Some([(key, value)].into_iter().collect());
                                    } else {
                                        otel_data
                                            .builder
                                            .attributes
                                            .as_mut()
                                            .expect("we checked the attributes value in the condition above")
                                            .insert(key, value);
                                    }
                                }
                                None => {
                                    // Can't use ::tracing::error! because it could create deadlock on extensions
                                    eprintln!("no OtelData, this is a bug");
                                }
                            }
                        } else {
                            if key.as_str().starts_with(APOLLO_PRIVATE_PREFIX) {
                                return;
                            }
                            let mut extensions = s.extensions_mut();
                            match extensions.get_mut::<LogAttributes>() {
                                Some(attributes) => {
                                    attributes.insert(KeyValue::new(key, value));
                                }
                                None => {
                                    // Can't use ::tracing::error! because it could create deadlock on extensions
                                    eprintln!("no LogAttributes, this is a bug");
                                }
                            }
                        }
                    }
                };
            } else {
                ::tracing::error!("no Registry, this is a bug");
            }
        });
    }

    fn set_span_dyn_attributes(&self, attributes: impl IntoIterator<Item = KeyValue>) {
        let mut attributes = attributes.into_iter().peekable();
        if attributes.peek().is_none() {
            return;
        }
        self.with_subscriber(move |(id, dispatch)| {
            if let Some(reg) = dispatch.downcast_ref::<Registry>() {
                match reg.span(id) {
                    None => eprintln!("no spanref, this is a bug"),
                    Some(s) => {
                        if s.is_sampled() {
                            let mut extensions = s.extensions_mut();
                            match extensions.get_mut::<OtelData>() {
                                Some(otel_data) => {
                                    if otel_data.builder.attributes.is_none() {
                                        otel_data.builder.attributes = Some(attributes.collect());
                                    } else {
                                        otel_data
                                            .builder
                                            .attributes
                                            .as_mut()
                                            .unwrap()
                                            .extend(attributes);
                                    }
                                }
                                None => {
                                    // Can't use ::tracing::error! because it could create deadlock on extensions
                                    eprintln!("no OtelData, this is a bug");
                                }
                            }
                        } else {
                            let mut attributes = attributes
                                .filter(|kv| !kv.key.as_str().starts_with(APOLLO_PRIVATE_PREFIX))
                                .peekable();
                            if attributes.peek().is_none() {
                                return;
                            }
                            let mut extensions = s.extensions_mut();
                            match extensions.get_mut::<LogAttributes>() {
                                Some(registered_attributes) => {
                                    registered_attributes.extend(attributes);
                                }
                                None => {
                                    // Can't use ::tracing::error! because it could create deadlock on extensions
                                    eprintln!("no LogAttributes, this is a bug");
                                }
                            }
                        }
                    }
                };
            } else {
                ::tracing::error!("no Registry, this is a bug");
            }
        });
    }
}

// TODO il me faut un autre layer juste pour les attributs d'events et il faut qu'il passe après celui de otel

pub(crate) struct EventsAttributes {
    pub(crate) events_attributes: HashMap<String, LogAttributes>,
}

impl Default for EventsAttributes {
    fn default() -> Self {
        Self {
            events_attributes: HashMap::with_capacity(0),
        }
    }
}

/// To add dynamic attributes for spans
pub(crate) struct DynEventAttributeLayer;

impl<S> Layer<S> for DynEventAttributeLayer
where
    S: tracing_core::Subscriber + for<'lookup> tracing_subscriber::registry::LookupSpan<'lookup>,
{
    fn on_new_span(
        &self,
        _attrs: &tracing_core::span::Attributes<'_>,
        id: &tracing_core::span::Id,
        ctx: Context<'_, S>,
    ) {
        let span = ctx.span(id).expect("Span not found, this is a bug");
        let mut extensions = span.extensions_mut();
        if extensions.get_mut::<EventsAttributes>().is_none() {
            extensions.insert(EventsAttributes::default());
        }
    }

    /// Notifies this layer that an event has occurred.
    fn on_event(&self, event: &Event<'_>, ctx: Context<'_, S>) {
        // Je récupère mes eventsAttributes et specifiquement mon attribut pour l'event et je l'ajoute à mon otelData events.last()
        let mut event_kind = EventKindVisitor::default();
        event.record(&mut event_kind);
        if let Some(event_kind) = event_kind.0 {
            let span = ctx.event_span(event);
            if let Some(span) = span {
                let mut extensions = span.extensions_mut();
                if let (Some(attributes), Some(otel_events)) = (
                    extensions
                        .get::<EventsAttributes>()
                        .and_then(|attrs| attrs.events_attributes.get(&event_kind)),
                    extensions
                        .get_mut::<OtelData>()
                        .and_then(|od| od.builder.events.as_mut())
                        .and_then(|e| e.last_mut()),
                ) {
                    // otel_data.builder.events.
                }
            }
        }
    }

    // The best solution might be to directly fetch eventsAttributes from otel layer
}

impl DynEventAttributeLayer {
    pub(crate) fn new() -> Self {
        Self {}
    }
}

#[derive(Default)]
struct EventKindVisitor(Option<String>);

impl Visit for EventKindVisitor {
    fn record_debug(&mut self, field: &tracing_core::Field, value: &dyn std::fmt::Debug) {
        if field.name() == APOLLO_PRIVATE_KIND {
            self.0 = Some(format!("{value:?}"));
        }
    }

    fn record_str(&mut self, field: &tracing_core::Field, value: &str) {
        if field.name() == APOLLO_PRIVATE_KIND {
            self.0 = Some(value.to_string());
        }
    }
}

/// To add dynamic attributes for spans
pub(crate) trait EventDynAttribute {
    fn set_event_dyn_attribute(&self, event_name: String, key: Key, value: opentelemetry::Value);
    fn set_event_dyn_attributes(
        &self,
        event_name: String,
        attributes: impl IntoIterator<Item = KeyValue>,
    );
}

impl EventDynAttribute for ::tracing::Span {
    fn set_event_dyn_attribute(&self, event_name: String, key: Key, value: opentelemetry::Value) {
        self.with_subscriber(move |(id, dispatch)| {
            if let Some(reg) = dispatch.downcast_ref::<Registry>() {
                match reg.span(id) {
                    None => eprintln!("no spanref, this is a bug"),
                    Some(s) => {
                        if key.as_str().starts_with(APOLLO_PRIVATE_PREFIX) {
                            return;
                        }
                        let mut extensions = s.extensions_mut();
                        match extensions.get_mut::<EventsAttributes>() {
                            Some(attributes) => {
                                match attributes.events_attributes.get_mut(&event_name) {
                                    Some(attributes) => {
                                        attributes.insert(KeyValue::new(key, value));
                                    }
                                    None => {
                                        attributes.events_attributes.insert(
                                            event_name,
                                            LogAttributes {
                                                attributes: vec![KeyValue::new(key, value)],
                                            },
                                        );
                                    }
                                }
                            }
                            None => {
                                // Can't use ::tracing::error! because it could create deadlock on extensions
                                eprintln!("no EventsAttributes, this is a bug");
                            }
                        }
                    }
                };
            } else {
                ::tracing::error!("no Registry, this is a bug");
            }
        });
    }

    fn set_event_dyn_attributes(
        &self,
        event_name: String,
        attributes: impl IntoIterator<Item = KeyValue>,
    ) {
        let mut attributes = attributes.into_iter().peekable();
        if attributes.peek().is_none() {
            return;
        }
        self.with_subscriber(move |(id, dispatch)| {
            if let Some(reg) = dispatch.downcast_ref::<Registry>() {
                match reg.span(id) {
                    None => eprintln!("no spanref, this is a bug"),
                    Some(s) => {
                        let mut extensions = s.extensions_mut();
                        match extensions.get_mut::<EventsAttributes>() {
                            Some(registered_attributes) => {
                                match registered_attributes.events_attributes.get_mut(&event_name) {
                                    Some(registered_attributes) => {
                                        registered_attributes.extend(attributes);
                                    }
                                    None => {
                                        registered_attributes.events_attributes.insert(
                                            event_name,
                                            LogAttributes {
                                                attributes: attributes.collect(),
                                            },
                                        );
                                    }
                                }
                            }
                            None => {
                                // Can't use ::tracing::error! because it could create deadlock on extensions
                                eprintln!("no EventsAttributes, this is a bug");
                            }
                        }
                    }
                };
            } else {
                ::tracing::error!("no Registry, this is a bug");
            }
        });
    }
}
