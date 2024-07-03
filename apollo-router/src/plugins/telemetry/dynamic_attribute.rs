use opentelemetry::{Key, KeyValue, Value};
use tracing_subscriber::layer::Context;
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::Layer;
use tracing_subscriber::Registry;

use super::consts::OTEL_KIND;
use super::consts::OTEL_NAME;
use super::consts::OTEL_STATUS_CODE;
use super::consts::OTEL_STATUS_MESSAGE;
use super::formatters::APOLLO_PRIVATE_PREFIX;
use super::otel::layer::str_to_span_kind;
use super::otel::layer::str_to_status;
use super::otel::OtelData;
use super::reload::IsSampled;

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
pub(crate) struct DynAttributeLayer;

impl<S> Layer<S> for DynAttributeLayer
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
        if extensions.get_mut::<EventAttributes>().is_none() {
            extensions.insert(EventAttributes::default());
        }
    }
}

impl DynAttributeLayer {
    pub(crate) fn new() -> Self {
        Self {}
    }
}

/// To add dynamic attributes for spans
pub(crate) trait SpanDynAttribute {
    fn set_span_dyn_attribute<K: Into<Key>, V: Into<Value>>(&self, key: K, value: V);
    fn set_span_dyn_attributes(&self, attributes: impl IntoIterator<Item = KeyValue>);
}

impl SpanDynAttribute for ::tracing::Span {
    fn set_span_dyn_attribute<K: Into<Key>, V: Into<Value>>(&self, key: K, value: V) {
        let key_value = KeyValue::new(key.into(), value.into());
        self.with_subscriber(move |(id, dispatch)| {
            if let Some(reg) = dispatch.downcast_ref::<Registry>() {
                match reg.span(id) {
                    None => eprintln!("no spanref, this is a bug"),
                    Some(s) => {
                        if s.is_sampled() {
                            let mut extensions = s.extensions_mut();
                            match extensions.get_mut::<OtelData>() {
                                Some(otel_data) => {
                                    update_otel_data(otel_data, &key_value);
                                    if otel_data.builder.attributes.is_none() {
                                        otel_data.builder.attributes =
                                            Some(vec![key_value]);
                                    } else {
                                        otel_data
                                            .builder
                                            .attributes
                                            .as_mut()
                                            .expect("we checked the attributes value in the condition above")
                                            .push(key_value);
                                    }
                                }
                                None => {
                                    // Can't use ::tracing::error! because it could create deadlock on extensions
                                    eprintln!("no OtelData, this is a bug");
                                }
                            }
                        } else {
                            if key_value.key.as_str().starts_with(APOLLO_PRIVATE_PREFIX) {
                                return;
                            }
                            let mut extensions = s.extensions_mut();
                            match extensions.get_mut::<LogAttributes>() {
                                Some(attributes) => {
                                    attributes.insert(key_value);
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
                                                .inspect(|key_value| {
                                                    update_otel_data(otel_data, key_value)
                                                })
                                                .collect(),
                                        );
                                    } else {
                                        let attributes: Vec<KeyValue> = attributes
                                            .inspect(|attr| update_otel_data(otel_data, &attr))
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

fn update_otel_data(otel_data: &mut OtelData, key_value: &KeyValue) {
    match key_value.key.as_str() {
        OTEL_NAME if otel_data.forced_span_name.is_none() => {
            otel_data.forced_span_name = Some(key_value.value.to_string())
        }
        OTEL_KIND => otel_data.builder.span_kind = str_to_span_kind(&key_value.value.as_str()),
        OTEL_STATUS_CODE => {
            otel_data.forced_status = str_to_status(&key_value.value.as_str()).into()
        }
        OTEL_STATUS_MESSAGE => {
            otel_data.builder.status =
                opentelemetry::trace::Status::error(key_value.value.as_str().to_string())
        }
        _ => {}
    }
}

#[derive(Debug, Default)]
pub(crate) struct EventAttributes {
    attributes: Vec<KeyValue>,
}

impl EventAttributes {
    pub(crate) fn insert(&mut self, kv: KeyValue) {
        self.attributes.push(kv);
    }

    pub(crate) fn extend(&mut self, other: impl IntoIterator<Item = KeyValue>) {
        self.attributes.extend(other);
    }

    pub(crate) fn take(&mut self) -> Vec<KeyValue> {
        std::mem::take(&mut self.attributes)
    }
}

/// To add dynamic attributes for spans
pub(crate) trait EventDynAttribute {
    /// Always use before sending the event
    fn set_event_dyn_attribute(&self, key_value: KeyValue);
    /// Always use before sending the event
    fn set_event_dyn_attributes(&self, attributes: impl IntoIterator<Item = KeyValue>);
}

impl EventDynAttribute for ::tracing::Span {
    fn set_event_dyn_attribute(&self, key_value: KeyValue) {
        self.with_subscriber(move |(id, dispatch)| {
            if let Some(reg) = dispatch.downcast_ref::<Registry>() {
                match reg.span(id) {
                    None => eprintln!("no spanref, this is a bug"),
                    Some(s) => {
                        if key_value.key.as_str().starts_with(APOLLO_PRIVATE_PREFIX) {
                            return;
                        }
                        if s.is_sampled() {
                            let mut extensions = s.extensions_mut();
                            match extensions.get_mut::<OtelData>() {
                                Some(otel_data) => match &mut otel_data.event_attributes {
                                    Some(attributes) => {
                                        attributes.push(key_value);
                                    }
                                    None => {
                                        otel_data.event_attributes = Some(vec![key_value]);
                                    }
                                },
                                None => {
                                    // Can't use ::tracing::error! because it could create deadlock on extensions
                                    eprintln!("no OtelData, this is a bug");
                                }
                            }
                        } else {
                            if key_value.key.as_str().starts_with(APOLLO_PRIVATE_PREFIX) {
                                return;
                            }
                            let mut extensions = s.extensions_mut();
                            match extensions.get_mut::<EventAttributes>() {
                                Some(attributes) => {
                                    attributes.insert(key_value);
                                }
                                None => {
                                    // Can't use ::tracing::error! because it could create deadlock on extensions
                                    eprintln!("no EventAttributes, this is a bug");
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
                                        event_attributes.extend(attributes);
                                    }
                                    None => {
                                        otel_data.event_attributes = Some(attributes.collect());
                                    }
                                },
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
                            match extensions.get_mut::<EventAttributes>() {
                                Some(registered_attributes) => {
                                    registered_attributes.extend(attributes);
                                }
                                None => {
                                    // Can't use ::tracing::error! because it could create deadlock on extensions
                                    eprintln!("no EventAttributes, this is a bug");
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
