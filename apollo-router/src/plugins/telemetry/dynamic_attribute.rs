use opentelemetry::Key;
use opentelemetry::KeyValue;
use opentelemetry::OrderMap;
use tracing_subscriber::layer::Context;
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::Layer;
use tracing_subscriber::Registry;

use super::otel::layer::str_to_span_kind;
use super::otel::layer::str_to_status;
use super::otel::layer::SPAN_KIND_FIELD;
use super::otel::layer::SPAN_NAME_FIELD;
use super::otel::layer::SPAN_STATUS_CODE_FIELD;
use super::otel::layer::SPAN_STATUS_MESSAGE_FIELD;
use super::otel::OtelData;
use super::reload::IsSampled;
use super::tracing::APOLLO_PRIVATE_PREFIX;

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
                                    update_otel_data(otel_data, &key, &value);
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
                                        otel_data.builder.attributes = Some(
                                            attributes
                                                .inspect(|attr| {
                                                    update_otel_data(
                                                        otel_data,
                                                        &attr.key,
                                                        &attr.value,
                                                    )
                                                })
                                                .collect(),
                                        );
                                    } else {
                                        let attributes: Vec<KeyValue> = attributes
                                            .inspect(|attr| {
                                                update_otel_data(otel_data, &attr.key, &attr.value)
                                            })
                                            .collect();
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

fn update_otel_data(otel_data: &mut OtelData, key: &Key, value: &opentelemetry::Value) {
    match key.as_str() {
        SPAN_NAME_FIELD => otel_data.forced_span_name = Some(value.to_string()),
        SPAN_KIND_FIELD => otel_data.builder.span_kind = str_to_span_kind(&value.as_str()),
        SPAN_STATUS_CODE_FIELD => otel_data.forced_status = str_to_status(&value.as_str()).into(),
        SPAN_STATUS_MESSAGE_FIELD => {
            otel_data.builder.status =
                opentelemetry::trace::Status::error(value.as_str().to_string())
        }
        _ => {}
    }
}

/// To add dynamic attributes for spans
pub(crate) trait EventDynAttribute {
    /// Always use before sending the event
    fn set_event_dyn_attribute(&self, key: Key, value: opentelemetry::Value);
    /// Always use before sending the event
    fn set_event_dyn_attributes(&self, attributes: impl IntoIterator<Item = KeyValue>);
}

impl EventDynAttribute for ::tracing::Span {
    fn set_event_dyn_attribute(&self, key: Key, value: opentelemetry::Value) {
        self.with_subscriber(move |(id, dispatch)| {
            if let Some(reg) = dispatch.downcast_ref::<Registry>() {
                match reg.span(id) {
                    None => eprintln!("no spanref, this is a bug"),
                    Some(s) => {
                        if key.as_str().starts_with(APOLLO_PRIVATE_PREFIX) {
                            return;
                        }
                        if s.is_sampled() {
                            let mut extensions = s.extensions_mut();
                            match extensions.get_mut::<OtelData>() {
                                Some(otel_data) => match &mut otel_data.event_attributes {
                                    Some(attributes) => {
                                        attributes.insert(key, value);
                                    }
                                    None => {
                                        let mut order_map = OrderMap::new();
                                        order_map.insert(key, value);
                                        otel_data.event_attributes = Some(order_map);
                                    }
                                },
                                None => {
                                    // Can't use ::tracing::error! because it could create deadlock on extensions
                                    eprintln!("no OtelData, this is a bug");
                                }
                            }
                        } else {
                            // FIXME: we should put event attributes somewhere else to make it work even if it's not sampled like we did with LogAttributes
                        }
                    }
                };
            } else {
                ::tracing::error!("no Registry, this is a bug");
            }
        });
    }

    fn set_event_dyn_attributes(&self, attributes: impl IntoIterator<Item = KeyValue>) {
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
                                Some(otel_data) => match &mut otel_data.event_attributes {
                                    Some(event_attributes) => {
                                        event_attributes
                                            .extend(attributes.map(|kv| (kv.key, kv.value)));
                                    }
                                    None => {
                                        otel_data.event_attributes = Some(OrderMap::from_iter(
                                            attributes.map(|kv| (kv.key, kv.value)),
                                        ));
                                    }
                                },
                                None => {
                                    // Can't use ::tracing::error! because it could create deadlock on extensions
                                    eprintln!("no OtelData, this is a bug");
                                }
                            }
                        } else {
                            // FIXME: we should put event attributes somewhere else to make it work even if it's not sampled like we did with LogAttributes
                        }
                    }
                };
            } else {
                ::tracing::error!("no Registry, this is a bug");
            }
        });
    }
}
