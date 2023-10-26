use std::collections::HashMap;

use tracing_subscriber::layer::Context;
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::Layer;
use tracing_subscriber::Registry;

use super::ROUTER_SPAN_NAME;
use super::SUBGRAPH_SPAN_NAME;
use super::SUPERGRAPH_SPAN_NAME;

#[derive(Debug)]
pub(crate) enum LogAttributes {
    Router(HashMap<String, String>),
    Supergraph(HashMap<String, String>),
    Subgraph(HashMap<String, String>),
}

impl LogAttributes {
    pub(crate) fn get_attributes(&self) -> &HashMap<String, String> {
        match self {
            LogAttributes::Router(attributes)
            | LogAttributes::Subgraph(attributes)
            | LogAttributes::Supergraph(attributes) => attributes,
        }
    }

    fn insert(&mut self, span_name: &str, key: String, value: String) {
        match span_name {
            ROUTER_SPAN_NAME => {
                if let Self::Router(attributes) = self {
                    attributes.insert(key, value);
                }
            }
            SUBGRAPH_SPAN_NAME => {
                if let Self::Subgraph(attributes) = self {
                    attributes.insert(key, value);
                }
            }
            SUPERGRAPH_SPAN_NAME => {
                if let Self::Supergraph(attributes) = self {
                    attributes.insert(key, value);
                }
            }
            _ => {
                eprintln!("cannot add custom attributes to this span '{span_name}', it's only available on router/supergraph/subgraph spans");
            }
        }
    }
    fn extend(&mut self, span_name: &str, val: impl IntoIterator<Item = (String, String)>) {
        match span_name {
            ROUTER_SPAN_NAME => {
                if let Self::Router(attributes) = self {
                    attributes.extend(val);
                }
            }
            SUBGRAPH_SPAN_NAME => {
                if let Self::Subgraph(attributes) = self {
                    attributes.extend(val);
                }
            }
            SUPERGRAPH_SPAN_NAME => {
                if let Self::Supergraph(attributes) = self {
                    attributes.extend(val);
                }
            }
            _ => {
                eprintln!("cannot add custom attributes to this span '{span_name}', it's only available on router/supergraph/subgraph spans");
            }
        }
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
        let custom_attributes = match span.name() {
            ROUTER_SPAN_NAME => LogAttributes::Router(HashMap::new()),
            SUBGRAPH_SPAN_NAME => LogAttributes::Subgraph(HashMap::new()),
            SUPERGRAPH_SPAN_NAME => LogAttributes::Supergraph(HashMap::new()),
            _ => {
                return;
            }
        };
        let mut extensions = span.extensions_mut();
        if extensions.get_mut::<LogAttributes>().is_none() {
            extensions.insert(custom_attributes);
        }
    }
}

impl DynAttributeLayer {
    pub(crate) fn new() -> Self {
        Self {}
    }
}

pub(crate) trait DynAttribute {
    fn set_dyn_attribute(&self, key: String, value: String);
    fn set_dyn_attributes(&self, attributes: HashMap<String, String>);
}

impl DynAttribute for ::tracing::Span {
    fn set_dyn_attribute(&self, key: String, value: String) {
        self.with_subscriber(move |(id, dispatch)| {
            if let Some(reg) = dispatch.downcast_ref::<Registry>() {
                match reg.span(id) {
                    None => eprintln!("no spanref, this is a bug"),
                    Some(s) => {
                        let mut extensions = s.extensions_mut();
                        match extensions.get_mut::<LogAttributes>() {
                            Some(attributes) => {
                                attributes.insert(s.name(), key, value);
                            }
                            None => {
                                eprintln!("no SubgraphRequestLogAttributes, this is a bug");
                            }
                        }
                    }
                };
            } else {
                eprintln!("no Registry, this is a bug");
            }
        });
    }

    fn set_dyn_attributes(&self, attributes: HashMap<String, String>) {
        self.with_subscriber(move |(id, dispatch)| {
            if let Some(reg) = dispatch.downcast_ref::<Registry>() {
                match reg.span(id) {
                    None => eprintln!("no spanref, this is a bug"),
                    Some(s) => {
                        let mut extensions = s.extensions_mut();
                        match extensions.get_mut::<LogAttributes>() {
                            Some(registered_attributes) => {
                                registered_attributes.extend(s.name(), attributes);
                            }
                            None => {
                                eprintln!("no LogAttributes, this is a bug");
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
