use std::collections::HashMap;

use crate::query_planner::OperationKind;
use crate::spec::Schema;

use super::EventError;

const SUBSCRIBE_DIRECTIVE_NAME: &str = "event__subscribe";

#[derive(Clone, Debug)]
pub(super) struct EventField {
    pub(super) source: String,
    pub(super) destinations: Vec<String>,
}

#[derive(Clone, Debug, Default)]
pub(super) struct EventCatalog {
    pub(super) fields: HashMap<(String, String), EventField>,
}

impl EventCatalog {
    pub(super) fn from_schema(schema: &Schema) -> Result<Self, EventError> {
        let supergraph = schema.supergraph_schema();
        let graph_names = supergraph
            .get_enum("join__Graph")
            .map(|graph_enum| {
                graph_enum
                    .values
                    .iter()
                    .filter_map(|(enum_name, value)| {
                        let name = value
                            .directives
                            .get("join__graph")?
                            .specified_argument_by_name("name")?
                            .as_str()?;
                        Some((enum_name.to_string(), name.to_string()))
                    })
                    .collect::<HashMap<_, _>>()
            })
            .unwrap_or_default();

        let mut fields = HashMap::new();
        let root = schema.root_operation_name(OperationKind::Subscription);
        let Some(subscription) = supergraph.get_object(root) else {
            return Ok(Self { fields });
        };

        for (field_name, definition) in &subscription.fields {
            for directive in definition.directives.get_all("join__directive") {
                let is_event = directive
                    .argument_by_name("name", supergraph)
                    .ok()
                    .and_then(|value| value.as_str())
                    == Some(SUBSCRIBE_DIRECTIVE_NAME);
                if !is_event {
                    continue;
                }

                let Some(args) = directive
                    .argument_by_name("args", supergraph)
                    .ok()
                    .and_then(|value| value.as_object())
                else {
                    return Err(EventError::new(format!(
                        "event subscription field '{field_name}' has no directive arguments"
                    )));
                };
                let source = args.iter().find_map(|(name, value)| {
                    (name.as_str() == "source")
                        .then(|| value.as_str())
                        .flatten()
                });
                let destinations = args
                    .iter()
                    .find_map(|(name, value)| {
                        (name.as_str() == "destinations")
                            .then(|| value.as_list())
                            .flatten()
                    })
                    .map(|values| {
                        values
                            .iter()
                            .filter_map(|value| value.as_str().map(ToString::to_string))
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                let graphs = directive
                    .argument_by_name("graphs", supergraph)
                    .ok()
                    .and_then(|value| value.as_list());

                let (Some(source), Some(graphs)) = (source, graphs) else {
                    return Err(EventError::new(format!(
                        "event subscription field '{field_name}' must specify source and graphs"
                    )));
                };
                let mut resolved_graph = false;
                for graph in graphs {
                    if let Some(graph) = graph.as_enum()
                        && let Some(service_name) = graph_names.get(graph.as_str())
                    {
                        resolved_graph = true;
                        fields.insert(
                            (service_name.clone(), field_name.to_string()),
                            EventField {
                                source: source.to_string(),
                                destinations: destinations.clone(),
                            },
                        );
                    }
                }
                if !resolved_graph {
                    return Err(EventError::new(format!(
                        "event subscription field '{field_name}' has no resolvable subgraph"
                    )));
                }
            }
        }
        Ok(Self { fields })
    }
}
