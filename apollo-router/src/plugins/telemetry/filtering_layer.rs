use std::collections::HashMap;
use std::collections::HashSet;

use tracing::field;
use tracing::Level;
use tracing::Subscriber;
use tracing_core::Field;

// Specific attributes for logging
pub(crate) const SPECIFIC_ATTRIBUTES: [&str; 4] = [
    "request",
    "response_headers",
    "response_body",
    "operation_name",
];

const SUBGRAPH_ATTRIBUTE_NAME: &str = "subgraph";

#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct AttributesToExclude {
    pub(crate) supergraph: HashSet<String>,
    pub(crate) all_subgraphs: HashSet<String>,
    pub(crate) subgraphs: HashMap<String, HashSet<String>>,
}

pub(crate) struct FilterSubscriber<S>
where
    S: Subscriber + Send + Sync + 'static,
{
    inner: S,
    attributes_to_exclude: Option<AttributesToExclude>,
}

impl<S> FilterSubscriber<S>
where
    S: Subscriber + Send + Sync + 'static,
{
    pub(crate) fn new(inner: S, attributes_to_exclude: Option<AttributesToExclude>) -> Self {
        Self {
            inner,
            attributes_to_exclude,
        }
    }
}

impl<S> Subscriber for FilterSubscriber<S>
where
    S: Subscriber + Send + Sync + 'static,
{
    // Called only one time for a specific callsite
    fn enabled(&self, metadata: &tracing::Metadata<'_>) -> bool {
        if metadata.level() == &Level::DEBUG && metadata.is_event() {
            match &self.attributes_to_exclude {
                Some(attributes_to_exclude) => {
                    let mut subgraph = false;
                    let mut to_exclude = false;
                    for field_name in metadata.fields().iter().map(|f| f.name()) {
                        if field_name == SUBGRAPH_ATTRIBUTE_NAME {
                            subgraph = true;
                            // Cannot do anything for subgraph at this level. It will be in `event` method.
                            break;
                        }
                        if attributes_to_exclude.supergraph.contains(field_name) {
                            to_exclude = true;
                        }
                    }
                    if !subgraph && to_exclude {
                        return false;
                    }
                }
                None => {
                    if metadata
                        .fields()
                        .iter()
                        .map(|f| f.name())
                        .any(|n| SPECIFIC_ATTRIBUTES.contains(&n))
                    {
                        return false;
                    }
                }
            }
        }

        self.inner.enabled(metadata)
    }

    fn new_span(&self, span: &tracing_core::span::Attributes<'_>) -> tracing_core::span::Id {
        self.inner.new_span(span)
    }

    fn record(&self, span: &tracing_core::span::Id, values: &tracing_core::span::Record<'_>) {
        // Filter for subgraph here
        self.inner.record(span, values)
    }

    fn record_follows_from(&self, span: &tracing_core::span::Id, follows: &tracing_core::span::Id) {
        self.inner.record_follows_from(span, follows)
    }

    fn event(&self, event: &tracing::Event<'_>) {
        if event.metadata().level() == &Level::DEBUG {
            let mut fields_visitor = FieldsVisitor::default();
            event.record(&mut fields_visitor);
            let field_names: HashSet<&str> = fields_visitor.fields.keys().copied().collect();
            // Check only for subgraphs here because other cases have been catched before in `enabled`
            if let Some(attributes_to_exclude) = &self.attributes_to_exclude {
                if let Some(subgraph) = fields_visitor.fields.get(SUBGRAPH_ATTRIBUTE_NAME) {
                    if let Some(attrs_to_exclude) = attributes_to_exclude
                        .subgraphs
                        .get(subgraph)
                        .map(|a| a.iter().map(|a| a.as_str()).collect::<HashSet<&str>>())
                    {
                        if !attrs_to_exclude.is_disjoint(&field_names) {
                            return;
                        }
                    } else if !attributes_to_exclude
                        .all_subgraphs
                        .iter()
                        .map(|a| a.as_str())
                        .collect::<HashSet<&str>>()
                        .is_disjoint(&field_names)
                    {
                        return;
                    }
                }
            }
        }

        self.inner.event(event)
    }

    fn enter(&self, span: &tracing_core::span::Id) {
        self.inner.enter(span)
    }

    fn exit(&self, span: &tracing_core::span::Id) {
        self.inner.exit(span)
    }
}

#[derive(Default, Clone, Debug)]
pub(crate) struct FieldsVisitor {
    fields: HashMap<&'static str, String>,
}

impl field::Visit for FieldsVisitor {
    /// Visit a string value.
    fn record_str(&mut self, field: &Field, value: &str) {
        if SPECIFIC_ATTRIBUTES.contains(&field.name()) || field.name() == SUBGRAPH_ATTRIBUTE_NAME {
            self.fields.insert(field.name(), value.to_string());
        }
    }

    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if SPECIFIC_ATTRIBUTES.contains(&field.name()) || field.name() == SUBGRAPH_ATTRIBUTE_NAME {
            self.fields.insert(field.name(), format!("{:?}", value));
        }
    }
}
