use std::collections::HashMap;

use opentelemetry_api::Key;
use opentelemetry_api::Value;
use tracing_opentelemetry::OtelData;
use tracing_subscriber::layer::Context;
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::Layer;
use tracing_subscriber::Registry;

use super::config::AttributeValue;
use super::reload::IsSampled;

#[derive(Debug)]
pub(crate) struct LogAttributes {
    attributes: HashMap<Key, AttributeValue>,
}

impl Default for LogAttributes {
    fn default() -> Self {
        Self {
            attributes: HashMap::with_capacity(0),
        }
    }
}

impl LogAttributes {
    pub(crate) fn attributes(&self) -> &HashMap<Key, AttributeValue> {
        &self.attributes
    }

    fn insert(&mut self, key: Key, value: AttributeValue) {
        self.attributes.insert(key, value);
    }

    fn extend(&mut self, val: impl IntoIterator<Item = (Key, AttributeValue)>) {
        self.attributes.extend(val);
    }
}

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
    }
}

impl DynAttributeLayer {
    pub(crate) fn new() -> Self {
        Self {}
    }
}

pub(crate) trait DynAttribute {
    fn set_dyn_attribute(&self, key: Key, value: AttributeValue);
    fn set_dyn_attributes(&self, attributes: impl IntoIterator<Item = (Key, AttributeValue)>);
}

impl DynAttribute for ::tracing::Span {
    fn set_dyn_attribute(&self, key: Key, value: AttributeValue) {
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
                                            Some([(key, Value::from(value))].into_iter().collect());
                                    } else {
                                        otel_data
                                            .builder
                                            .attributes
                                            .as_mut()
                                            .unwrap()
                                            .insert(key, Value::from(value));
                                    }
                                }
                                None => {
                                    eprintln!("no OtelData, this is a bug");
                                }
                            }
                        } else {
                            let mut extensions = s.extensions_mut();
                            match extensions.get_mut::<LogAttributes>() {
                                Some(attributes) => {
                                    attributes.insert(key, value);
                                }
                                None => {
                                    eprintln!("no LogAttributes, this is a bug");
                                }
                            }
                        }
                    }
                };
            } else {
                eprintln!("no Registry, this is a bug");
            }
        });
    }

    fn set_dyn_attributes(&self, attributes: impl IntoIterator<Item = (Key, AttributeValue)>) {
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
                                                .into_iter()
                                                .map(|(k, v)| (k, Value::from(v)))
                                                .collect(),
                                        );
                                    } else {
                                        otel_data.builder.attributes.as_mut().unwrap().extend(
                                            attributes
                                                .into_iter()
                                                .map(|(k, v)| (k, Value::from(v))),
                                        );
                                    }
                                }
                                None => {
                                    eprintln!("no OtelData, this is a bug");
                                }
                            }
                        } else {
                            let mut extensions = s.extensions_mut();
                            match extensions.get_mut::<LogAttributes>() {
                                Some(registered_attributes) => {
                                    registered_attributes.extend(attributes);
                                }
                                None => {
                                    eprintln!("no LogAttributes, this is a bug");
                                }
                            }
                        }
                    }
                };
            } else {
                eprintln!("no Registry, this is a bug");
            }
        });
    }
}
