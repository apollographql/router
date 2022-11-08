use std::collections::HashMap;
use std::collections::HashSet;

use tracing::field;
use tracing::Level;
use tracing::Subscriber;
use tracing_core::Field;
use tracing_subscriber::Layer;

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

pub(crate) struct FilterLayer {
    attributes_to_exclude: Option<AttributesToExclude>,
}

impl FilterLayer {
    pub(crate) fn new(attributes_to_exclude: Option<AttributesToExclude>) -> Self {
        Self {
            attributes_to_exclude,
        }
    }
}

impl<S> Layer<S> for FilterLayer
where
    S: Subscriber + Send + Sync + 'static,
{
    fn enabled(
        &self,
        metadata: &tracing::Metadata<'_>,
        ctx: tracing_subscriber::layer::Context<'_, S>,
    ) -> bool {
        // The logic is splitted between `enabled` and `event_enabled` because we can't evaluate the value of an attribute in this method
        // This method is evaluated only once for a specific callsite, which means if I return false here I won't re-enter in this method everytime for this event
        // Which is better for performance. So for supergraph level we can use this method, for subgraph level we have to know the value of subgraph attribute
        if metadata.level() == &Level::DEBUG && metadata.is_event() {
            match &self.attributes_to_exclude {
                Some(attributes_to_exclude) => {
                    let mut subgraph = false;
                    let mut to_exclude = false;
                    for field_name in metadata.fields().iter().map(|f| f.name()) {
                        if field_name == SUBGRAPH_ATTRIBUTE_NAME {
                            subgraph = true;
                            // Cannot do anything for subgraph at this level. It will be in `event_enabled` method.
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

        ctx.enabled(metadata)
    }

    fn event_enabled(
        &self,
        event: &tracing::Event<'_>,
        _ctx: tracing_subscriber::layer::Context<'_, S>,
    ) -> bool {
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
                            return false;
                        }
                    } else if !attributes_to_exclude
                        .all_subgraphs
                        .iter()
                        .map(|a| a.as_str())
                        .collect::<HashSet<&str>>()
                        .is_disjoint(&field_names)
                    {
                        return false;
                    }
                }
            }
        }

        true
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
