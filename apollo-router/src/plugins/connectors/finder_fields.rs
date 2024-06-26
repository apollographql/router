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
