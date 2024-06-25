/// When moving from the "outer" supergraph to the "inner" supergraph, we can't
/// use _entities. In fact, we wouldn't want to — the inner query planner needs
/// more information to know what connectors to use.
///
/// Instead, we'll swap the _entities root field for one that matches the
/// relevant connectors. It doesn't matter what the name of that field is, as
/// long as it matches connectors correctly.
///
/// Some connectors should share a root field, because they require the same
/// entity references and the query planner should fetch them in parallel.
///
/// To determine what finder field to use for a Fetch node, we look at the
/// "requires" selection set, which describes the entity reference that our
/// connector depends on. It has a type condition, so we can look only at
/// connectors that relevant to that entity type. (In the case of abstract
/// types, we might need to find the abstract type for all the implementing
/// types.)
///
/// Then we look at the fields provided in the entity reference. Any connectors
/// that need fields not provided are removed as candidates. The rest we rank
/// by number of required input fields in common with the entity reference.
use std::collections::HashSet;

use apollo_compiler::ast::Selection as GraphQLSelection;
use apollo_compiler::schema::Name;
use itertools::Itertools;

use super::Connector;
use crate::query_planner::fetch::RestProtocolWrapper;
use crate::query_planner::QueryPlannerSelection as Selection;
use crate::spec::Schema;

// --- finder fields for connectors --------------------------------------------

pub(super) fn finder_field_for_connector(connector: &Connector) -> Option<Name> {
    use super::connector::ConnectorKind::*;
    match &connector.kind {
        Entity { type_name, .. } => Some(Name::new_unchecked(
            format!(
                "_{}_{}",
                type_name,
                flatten_inputs_graphql_name(&connector.input_selection)
                    .iter()
                    .sorted()
                    .join("_")
            )
            .into(),
        )),
        EntityField { type_name, .. } => Some(Name::new_unchecked(
            format!(
                "_{}_{}",
                type_name,
                flatten_inputs_graphql_name(&connector.input_selection)
                    .iter()
                    .sorted()
                    .join("_")
            )
            .into(),
        )),
        RootField { .. } => None,
    }
}

fn flatten_inputs_graphql_name(inputs: &[GraphQLSelection]) -> HashSet<Name> {
    inputs
        .iter()
        .flat_map(|s| match s {
            GraphQLSelection::Field(f) => {
                if f.selection_set.is_empty() {
                    if f.name == "__typename" {
                        HashSet::new()
                    } else {
                        HashSet::from([f.name.clone()])
                    }
                } else {
                    flatten_inputs(&f.selection_set)
                        .iter()
                        // TODO: expect?
                        .map(|child| {
                            Name::new(format!("{}__{}", f.name, child)).expect("name is valid")
                        })
                        .collect()
                }
            }
            GraphQLSelection::InlineFragment(f) => flatten_inputs(&f.selection_set),
            _ => unreachable!("should not see fragment spread in connector input selections"),
        })
        .collect()
}

// --- determining the finder field for query planning -------------------------

pub(crate) fn finder_field_for_fetch_node(
    schema: &Schema,
    connectors: &[&Connector],
    requires: &[Selection],
) -> Option<RestProtocolWrapper> {
    let Some(output_type) = output_type_from_requires(schema, requires) else {
        return None;
    };

    let relevant_connectors = connectors
        .iter()
        .filter(move |connector| {
            use crate::plugins::connectors::connector::ConnectorKind::*;
            let connector_type = match &connector.kind {
                Entity { type_name, .. } => type_name,
                EntityField { type_name, .. } => type_name,
                _ => return false,
            };
            output_type == *connector_type
        })
        .collect::<Vec<_>>();

    let available_keys = flatten_requires(requires);

    relevant_connectors
        .iter()
        .filter_map(|connector| {
            let required_keys = flatten_inputs(&connector.input_selection);
            let missing_keys = required_keys.difference(&available_keys).count();
            let common_keys = required_keys.intersection(&available_keys).count() as i32;
            if missing_keys == 0 {
                // most common keys wins (inverted so we can sort lowest to highest)
                Some((-common_keys, connector))
            } else {
                None
            }
        })
        .sorted_by_key(|(common_keys, _)| *common_keys)
        .filter_map(|(_, connector)| {
            connector
                .finder_field_name()
                .map(|finder| (connector.display_name(), connector.name.clone(), finder))
        })
        .next()
        .map(
            |(connector_service_name, connector_graph_key, s)| RestProtocolWrapper {
                connector_service_name,
                connector_graph_key: Some(connector_graph_key),
                magic_finder_field: Some(s.as_str().to_string()),
            },
        )
}

fn flatten_inputs(inputs: &[GraphQLSelection]) -> HashSet<Name> {
    inputs
        .iter()
        .flat_map(|s| match s {
            GraphQLSelection::Field(f) => {
                if f.selection_set.is_empty() {
                    if f.name == "__typename" {
                        HashSet::new()
                    } else {
                        HashSet::from([f.name.clone()])
                    }
                } else {
                    flatten_inputs(&f.selection_set)
                        .iter()
                        // TODO: Expect?
                        .map(|child| {
                            Name::new(format!("{}.{}", f.name, child))
                                .expect("name should be valid")
                        })
                        .collect()
                }
            }
            GraphQLSelection::InlineFragment(f) => flatten_inputs(&f.selection_set),
            _ => unreachable!("should not see fragment spread in connector input selections"),
        })
        .collect()
}

fn output_type_from_requires(schema: &Schema, requires: &[Selection]) -> Option<Name> {
    let type_names: Vec<_> = requires
        .iter()
        .filter_map(|s| match s {
            Selection::InlineFragment(f) => f.type_condition.clone(),
            _ => None,
        })
        .map(|s| Name::new_unchecked(s.into()))
        .collect();

    if type_names.len() == 1 {
        return type_names.first().cloned();
    } else {
        schema
            .implementers_map
            .iter()
            .find_map(|(abstract_type, implementing_types)| {
                if type_names
                    .iter()
                    .all(|t| implementing_types.iter().contains(t))
                {
                    Some(abstract_type.clone())
                } else {
                    None
                }
            })
    }
}

fn flatten_requires(requires: &[Selection]) -> HashSet<Name> {
    requires
        .iter()
        .flat_map(|s| match s {
            Selection::Field(f) => match &f.selections {
                None => {
                    if f.name == "__typename" {
                        HashSet::new()
                    } else {
                        HashSet::from([f.name.clone()])
                    }
                }
                Some(s) => flatten_requires(s)
                    .iter()
                    .map(|child| {
                        Name::new(format!("{}.{}", f.name, child)).expect("name should be valid")
                    })
                    .collect(),
            },
            Selection::InlineFragment(f) => flatten_requires(&f.selections),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use apollo_compiler::schema::Schema;
    use insta::assert_debug_snapshot;

    use super::*;
    use crate::plugins::connectors::Source;

    const SCHEMA: &str = include_str!("./testdata/test_supergraph.graphql");

    #[test]
    fn test_finder_field_for_connector() {
        let schema = Schema::parse(SCHEMA, "./testdata/test_supergraph.graphql").unwrap();
        let source = Source::new(&schema).unwrap().unwrap();

        let field_names = source
            .connectors()
            .values()
            .filter_map(finder_field_for_connector)
            .sorted()
            .collect::<Vec<_>>();

        assert_debug_snapshot!(&field_names, @r###"
        [
            "_EntityAcrossBoth_a_b",
            "_EntityAcrossBoth_a_b",
            "_EntityAcrossBoth_d",
            "_EntityInterface_id",
            "_Hello_id",
            "_Hello_id",
            "_Hello_relatedId",
            "_TestRequires_weight",
            "_TestingInterfaceObject_id",
            "_TestingInterfaceObject_id",
        ]
        "###);
    }

    #[test]
    fn test_finder_field_for_fetch_node() {
        let schema = crate::spec::Schema::parse(SCHEMA, &Default::default()).unwrap();
        let source =
            Source::new(&Schema::parse(SCHEMA, "./testdata/test_supergraph.graphql").unwrap())
                .unwrap()
                .unwrap();
        let connectors = source.connectors();
        let connectors = connectors.values().collect::<Vec<_>>();

        // out of order keys

        let requires: Vec<Selection> = serde_json::from_str(
            r#"
        [{
            "kind": "InlineFragment",
            "typeCondition": "EntityAcrossBoth",
            "selections": [
                {
                    "kind": "Field",
                    "name": "__typename"
                },
                {
                    "kind": "Field",
                    "name": "b"
                },
                {
                    "kind": "Field",
                    "name": "a"
                }
            ]
        }]
    "#,
        )
        .unwrap();

        let rpw = finder_field_for_fetch_node(&schema, &connectors, &requires).unwrap();
        assert_eq!(
            rpw.magic_finder_field,
            Some("_EntityAcrossBoth_a_b".to_string())
        );

        // @requires

        let requires: Vec<Selection> = serde_json::from_str(
            r#"
        [{
            "kind": "InlineFragment",
            "typeCondition": "TestRequires",
            "selections": [
                {
                    "kind": "Field",
                    "name": "__typename"
                },
                {
                    "kind": "Field",
                    "name": "id"
                },
                {
                    "kind": "Field",
                    "name": "weight"
                }
            ]
        }]
    "#,
        )
        .unwrap();

        let rpw = finder_field_for_fetch_node(&schema, &connectors, &requires).unwrap();
        assert_eq!(
            rpw.magic_finder_field,
            Some("_TestRequires_weight".to_string())
        );
        // @interface object

        let requires: Vec<Selection> = serde_json::from_str(
            r#"
        [{
            "kind": "InlineFragment",
            "typeCondition": "IOa",
            "selections": [
                {
                    "kind": "Field",
                    "name": "__typename"
                },
                {
                    "kind": "Field",
                    "name": "id"
                }
            ]
        }, {
            "kind": "InlineFragment",
            "typeCondition": "IOb",
            "selections": [
                {
                    "kind": "Field",
                    "name": "__typename"
                },
                {
                    "kind": "Field",
                    "name": "id"
                }
            ]
        }]
    "#,
        )
        .unwrap();

        let rpw = finder_field_for_fetch_node(&schema, &connectors, &requires).unwrap();
        assert_eq!(
            rpw.magic_finder_field,
            Some("_TestingInterfaceObject_id".to_string())
        );
    }
}
