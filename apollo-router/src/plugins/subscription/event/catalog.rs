use std::collections::HashMap;

use apollo_compiler::ast::Value;

use crate::query_planner::OperationKind;
use crate::spec::Schema;

use super::EventError;

const EVENT_SPEC_URL: &str = "https://specs.apollo.dev/event/v0.1";
const EVENT_SPEC_NAME: &str = "event";
const SUBSCRIBE_DIRECTIVE_NAME: &str = "subscribe";

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
        let directive_names = event_directive_names(supergraph);

        let mut fields = HashMap::new();
        let root = schema.root_operation_name(OperationKind::Subscription);
        let Some(subscription) = supergraph.get_object(root) else {
            return Ok(Self { fields });
        };

        for (field_name, definition) in &subscription.fields {
            for directive in definition.directives.get_all("join__directive") {
                let Some(directive_name) = directive
                    .argument_by_name("name", supergraph)
                    .ok()
                    .and_then(|value| value.as_str())
                else {
                    continue;
                };

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
                        && directive_names
                            .get(graph.as_str())
                            .is_some_and(|expected| expected == directive_name)
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

fn event_directive_names(schema: &apollo_compiler::Schema) -> HashMap<String, String> {
    let mut names = HashMap::new();
    for directive in &schema.schema_definition.directives {
        if directive.name != "join__directive"
            || directive
                .specified_argument_by_name("name")
                .and_then(|value| value.as_str())
                != Some("link")
        {
            continue;
        }
        let Some(args) = directive
            .specified_argument_by_name("args")
            .and_then(|value| value.as_object())
        else {
            continue;
        };
        let argument = |target: &str| {
            args.iter()
                .find_map(|(name, value)| (name.as_str() == target).then_some(value.as_ref()))
        };
        if argument("url").and_then(Value::as_str) != Some(EVENT_SPEC_URL) {
            continue;
        }

        let namespace = argument("as")
            .and_then(Value::as_str)
            .unwrap_or(EVENT_SPEC_NAME);
        let imported_name = argument("import")
            .and_then(Value::as_list)
            .and_then(|imports| {
                imports
                    .iter()
                    .find_map(|value| imported_subscribe_name(value.as_ref()))
            });
        let directive_name =
            imported_name.unwrap_or_else(|| format!("{namespace}__{SUBSCRIBE_DIRECTIVE_NAME}"));

        if let Some(graphs) = directive
            .specified_argument_by_name("graphs")
            .and_then(|value| value.as_list())
        {
            for graph in graphs.iter().filter_map(|value| value.as_enum()) {
                names.insert(graph.to_string(), directive_name.clone());
            }
        }
    }
    names
}

fn imported_subscribe_name(value: &Value) -> Option<String> {
    if let Some(name) = value.as_str() {
        return (name == "@subscribe").then(|| SUBSCRIBE_DIRECTIVE_NAME.to_string());
    }
    let import = value.as_object()?;
    let argument = |target: &str| {
        import
            .iter()
            .find_map(|(name, value)| (name.as_str() == target).then_some(value.as_ref()))
    };
    (argument("name").and_then(Value::as_str) == Some("@subscribe")).then(|| {
        argument("as")
            .and_then(Value::as_str)
            .unwrap_or("@subscribe")
            .trim_start_matches('@')
            .to_string()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_default_imported_and_aliased_directive_names() {
        let schema = apollo_compiler::Schema::parse_and_validate(
            r#"
            schema
              @join__directive(graphs: [DEFAULT], name: "link", args: {url: "https://specs.apollo.dev/event/v0.1"})
              @join__directive(graphs: [NAMESPACE], name: "link", args: {url: "https://specs.apollo.dev/event/v0.1", as: "events"})
              @join__directive(graphs: [IMPORTED], name: "link", args: {url: "https://specs.apollo.dev/event/v0.1", import: ["@subscribe"]})
              @join__directive(graphs: [ALIASED], name: "link", args: {url: "https://specs.apollo.dev/event/v0.1", import: [{name: "@subscribe", as: "@onEvent"}]})
            { query: Query }

            directive @join__directive(graphs: [join__Graph!]!, name: String!, args: join__DirectiveArguments) repeatable on SCHEMA
            scalar join__DirectiveArguments
            enum join__Graph { DEFAULT NAMESPACE IMPORTED ALIASED }
            type Query { value: String }
            "#,
            "catalog.graphql",
        )
        .unwrap();

        assert_eq!(
            event_directive_names(&schema),
            HashMap::from([
                ("DEFAULT".to_string(), "event__subscribe".to_string()),
                ("NAMESPACE".to_string(), "events__subscribe".to_string()),
                ("IMPORTED".to_string(), "subscribe".to_string()),
                ("ALIASED".to_string(), "onEvent".to_string()),
            ])
        );
    }
}
