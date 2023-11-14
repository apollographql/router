use std::collections::HashMap;

use opentelemetry_api::Key;
use tracing_opentelemetry::OtelData;
use tracing_subscriber::layer::Context;
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::Layer;
use tracing_subscriber::Registry;

use super::reload::IsSampled;
use super::tracing::APOLLO_PRIVATE_PREFIX;

#[derive(Debug)]
pub(crate) struct LogAttributes {
    attributes: HashMap<Key, opentelemetry::Value>,
}

impl Default for LogAttributes {
    fn default() -> Self {
        Self {
            attributes: HashMap::with_capacity(0),
        }
    }
}

impl LogAttributes {
    pub(crate) fn attributes(&self) -> &HashMap<Key, opentelemetry::Value> {
        &self.attributes
    }

    fn insert(&mut self, key: Key, value: opentelemetry::Value) {
        self.attributes.insert(key, value);
    }

    pub(crate) fn extend(&mut self, val: impl IntoIterator<Item = (Key, opentelemetry::Value)>) {
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
    fn set_dyn_attribute(&self, key: Key, value: opentelemetry::Value);
    fn set_dyn_attributes(&self, attributes: impl IntoIterator<Item = (Key, opentelemetry::Value)>);
}

impl DynAttribute for ::tracing::Span {
    fn set_dyn_attribute(&self, key: Key, value: opentelemetry::Value) {
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
                                            .unwrap()
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
                                    attributes.insert(key, value);
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

    fn set_dyn_attributes(
        &self,
        attributes: impl IntoIterator<Item = (Key, opentelemetry::Value)>,
    ) {
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
                                            Some(attributes.into_iter().collect());
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
                            let mut extensions = s.extensions_mut();
                            match extensions.get_mut::<LogAttributes>() {
                                Some(registered_attributes) => {
                                    registered_attributes.extend(attributes.into_iter().filter(
                                        |(name, _)| {
                                            !name.as_str().starts_with(APOLLO_PRIVATE_PREFIX)
                                        },
                                    ));
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
