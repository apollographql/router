use std::collections::HashMap;

use opentelemetry_api::Key;
use opentelemetry_api::Value;
use tracing_core::field::Visit;
use tracing_core::Event;
use tracing_opentelemetry::OtelData;
use tracing_subscriber::layer::Context;
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::Layer;

use super::SubgraphRequestLogAttributes;

pub(crate) struct DynAttributeLayer;

impl<S> Layer<S> for DynAttributeLayer
where
    S: tracing_core::Subscriber + for<'lookup> tracing_subscriber::registry::LookupSpan<'lookup>,
{
    // TODO: later we might use on_event like the previous MetricsLayer we had to add it in the extensions
    fn on_event(&self, event: &Event<'_>, ctx: Context<'_, S>) {
        let mut attrs: HashMap<String, String> = HashMap::new();

        let mut visitor = DynAttributeVisitor::default();
        event.record(&mut visitor);

        for (key, value) in visitor.dyn_attributes {
            attrs.insert(key, value);
        }

        // TODO: Take all attributes prefixed by apollo_dynamic_attributes and remove them from OtelData
        println!("before");
        if let Some(span) = ctx.lookup_current() {
            println!("inside");
            // dbg!(&span.extensions().get::<OtelData>());
            span.extensions_mut()
                .insert(SubgraphRequestLogAttributes(attrs));
            println!("after");
        }
    }
}

#[derive(Debug, Default)]
struct DynAttributeVisitor {
    dyn_attributes: HashMap<String, String>,
}

impl Visit for DynAttributeVisitor {
    fn record_debug(&mut self, field: &tracing_core::Field, value: &dyn std::fmt::Debug) {
        // TODO strip on request/response/subgraph/supergraph...

        if let Some(name) = dbg!(field.name()).strip_prefix("apollo_dynamic_attribute.") {
            self.dyn_attributes
                .insert(name.to_string(), format!("{value:?}"));
        }
    }

    fn record_str(&mut self, field: &tracing_core::Field, value: &str) {
        if let Some(name) = field.name().strip_prefix("apollo_dynamic_attribute.") {
            self.dyn_attributes
                .insert(name.to_string(), value.to_string());
        }
    }
}

// pub(crate) trait DynAttribute {
//     fn set_dyn_attribute(&self, key: impl Into<Key>, value: impl Into<Value>);
// }

// impl DynAttribute for ::tracing::Span {
//     fn set_dyn_attribute(&self, key: impl Into<Key>, value: impl Into<Value>) {
//         self.with_subscriber(move |(id, subscriber)| {
//             if let Some(get_context) = subscriber.downcast_ref::<WithContext>() {
//                 let mut key = Some(key.into());
//                 let mut value = Some(value.into());
//                 get_context.with_context(subscriber, id, move |builder, _| {
//                     if builder.builder.attributes.is_none() {
//                         builder.builder.attributes = Some(Default::default());
//                     }
//                     builder
//                         .builder
//                         .attributes
//                         .as_mut()
//                         .unwrap()
//                         .insert(key.take().unwrap(), value.take().unwrap());
//                 })
//             }
//         });
//     }
// }
