use std::collections::HashMap;

use tracing_subscriber::layer::Context;
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::Layer;
use tracing_subscriber::Registry;

use super::SubgraphRequestLogAttributes;

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
        if extensions
            .get_mut::<SubgraphRequestLogAttributes>()
            .is_none()
        {
            extensions.insert(SubgraphRequestLogAttributes::default());
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
                        match extensions.get_mut::<SubgraphRequestLogAttributes>() {
                            Some(attributes) => {
                                attributes.0.insert(key, value);
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
                        match extensions.get_mut::<SubgraphRequestLogAttributes>() {
                            Some(registered_attributes) => {
                                registered_attributes.0.extend(attributes);
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
}
