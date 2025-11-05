use std::collections::BTreeSet;
use std::sync::Arc;

use apollo_compiler::Name;
use apollo_compiler::Node;
use apollo_compiler::ast;
use apollo_compiler::collections::IndexMap;
use apollo_compiler::collections::IndexSet;
use apollo_compiler::executable;
use itertools::Itertools;
use petgraph::graph::EdgeIndex;

use crate::bail;
use crate::composition::satisfiability::validation_state::ValidationState;
use crate::ensure;
use crate::error::CompositionError;
use crate::error::FederationError;
use crate::merger::hints::HintCode;
use crate::operation::FieldSelection;
use crate::operation::InlineFragment;
use crate::operation::InlineFragmentSelection;
use crate::operation::Operation;
use crate::operation::SelectionId;
use crate::operation::SelectionSet;
use crate::query_graph::QueryGraphEdgeTransition;
use crate::query_graph::QueryGraphNodeType;
use crate::query_graph::graph_path::Unadvanceables;
use crate::query_graph::graph_path::transition::TransitionGraphPath;
use crate::schema::ValidFederationSchema;
use crate::schema::position::CompositeTypeDefinitionPosition;
use crate::schema::position::FieldDefinitionPosition;
use crate::schema::position::TypeDefinitionPosition;
use crate::supergraph::CompositionHint;
use crate::utils::MultiIndexMap;
use crate::utils::human_readable::HumanReadableListOptions;
use crate::utils::human_readable::HumanReadableListPrefix;
use crate::utils::human_readable::human_readable_list;
use crate::utils::human_readable::human_readable_subgraph_names;
use crate::utils::human_readable::human_readable_types;

/// Returns a satisfiability error in Ok case; Otherwise, returns another error in Err case.
pub(super) fn satisfiability_error(
    unsatisfiable_path: &TransitionGraphPath,
    _subgraphs_paths: &[&TransitionGraphPath],
    subgraphs_paths_unadvanceables: &[Unadvanceables],
    errors: &mut Vec<CompositionError>,
) -> Result<(), FederationError> {
    let witness = build_witness_operation(unsatisfiable_path)?;
    let operation = witness.to_string();
    let message = format!(
        "The following supergraph API query:\n\
         {operation}\n\
         cannot be satisfied by the subgraphs because:\n\
         {reasons}",
        reasons = display_reasons(subgraphs_paths_unadvanceables),
    );
    // PORT_NOTE: We're not using `_subgraphs_paths` parameter, since we didn't port
    //            the `ValidationError` class, yet.
    errors.push(CompositionError::SatisfiabilityError { message });
    Ok(())
}

pub(super) fn shareable_field_non_intersecting_runtime_types_error(
    invalid_state: &ValidationState,
    field_definition_position: &FieldDefinitionPosition,
    runtime_types_to_subgraphs: &IndexMap<Arc<BTreeSet<Name>>, IndexSet<Arc<str>>>,
    errors: &mut Vec<CompositionError>,
) -> Result<(), FederationError> {
    let witness = build_witness_operation(invalid_state.supergraph_path())?;
    let operation = witness.to_string();
    let type_strings = runtime_types_to_subgraphs
        .iter()
        .map(|(runtime_types, subgraphs)| {
            format!(
                " - in {}, {}",
                human_readable_subgraph_names(subgraphs.iter()),
                human_readable_list(
                    runtime_types
                        .iter()
                        .map(|runtime_type| format!("\"{runtime_type}\"")),
                    HumanReadableListOptions {
                        prefix: Some(HumanReadableListPrefix {
                            singular: "type",
                            plural: "types",
                        }),
                        empty_output: "no runtime type is defined",
                        ..Default::default()
                    }
                )
            )
        })
        .join(";\n");
    let field = field_definition_position
        .get(invalid_state.supergraph_path().graph().schema()?.schema())?;
    let message = format!(
        "For the following supergraph API query:\n\
        {}\n\
        Shared field \"{}\" return type \"{}\" has a non-intersecting set of possible runtime \
        types across subgraphs. Runtime types in subgraphs are:\n\
        {}.\n\
        This is not allowed as shared fields must resolve the same way in all subgraphs, and that \
        implies at least some common runtime types between the subgraphs.",
        operation,
        field_definition_position,
        field.ty.inner_named_type(),
        type_strings,
    );
    errors.push(CompositionError::ShareableHasMismatchedRuntimeTypes { message });
    Ok(())
}

pub(super) fn shareable_field_mismatched_runtime_types_hint(
    state: &ValidationState,
    field_definition_position: &FieldDefinitionPosition,
    common_runtime_types: &BTreeSet<Name>,
    runtime_types_per_subgraphs: &IndexMap<Arc<str>, Arc<BTreeSet<Name>>>,
    hints: &mut Vec<CompositionHint>,
) -> Result<(), FederationError> {
    let witness = build_witness_operation(state.supergraph_path())?;
    let operation = witness.to_string();
    let all_subgraphs = state.current_subgraph_names()?;
    let subgraphs_with_type_not_in_intersection_string = all_subgraphs
        .iter()
        .map(|subgraph| {
            let Some(runtime_types) = runtime_types_per_subgraphs.get(subgraph) else {
                bail!("Unexpectedly no runtime types for path's tail's subgraph");
            };
            let types_to_not_implement = runtime_types
                .iter()
                .filter(|type_name| !common_runtime_types.contains(*type_name))
                .collect::<Vec<_>>();
            if types_to_not_implement.is_empty() {
                return Ok::<_, FederationError>(None);
            };
            Ok(Some(format!(
                " - subgraph \"{}\" should never resolve \"{}\" to an object of {}",
                subgraph,
                field_definition_position,
                human_readable_types(types_to_not_implement.into_iter()),
            )))
        })
        .process_results(|iter| iter.flatten().join(";\n"))?;
    let field =
        field_definition_position.get(state.supergraph_path().graph().schema()?.schema())?;
    let message = format!(
        "For the following supergraph API query:\n\
        {}\n\
        Shared field \"{}\" return type \"{}\" has different sets of possible runtime types across \
        subgraphs.\n\
        Since a shared field must be resolved the same way in all subgraphs, make sure that {} \
        only resolve \"{}\" to objects of {}. In particular:\n\
        {}.\n\
        Otherwise the @shareable contract will be broken.",
        operation,
        field_definition_position,
        field.ty,
        human_readable_subgraph_names(all_subgraphs.iter()),
        field_definition_position,
        human_readable_types(common_runtime_types.iter()),
        subgraphs_with_type_not_in_intersection_string,
    );
    hints.push(CompositionHint {
        message,
        code: HintCode::InconsistentRuntimeTypesForShareableReturn
            .code()
            .to_string(),
        locations: Default::default(), // TODO
    });
    Ok(())
}

fn build_witness_operation(witness: &TransitionGraphPath) -> Result<Operation, FederationError> {
    let root = witness.head_node()?;
    let Some(root_kind) = root.root_kind else {
        bail!("build_witness_operation: root kind is not set");
    };
    let schema = witness.schema_by_source(&root.source)?;
    let edges: Vec<_> = witness.iter().map(|item| item.0).collect();
    ensure!(
        !edges.is_empty(),
        "unsatisfiable_path should contain at least one edge/transition"
    );
    let Some(selection_set) = build_witness_next_step(schema, witness, &edges)? else {
        bail!("build_witness_operation: root selection set failed to build");
    };
    Ok(Operation {
        schema: schema.clone(),
        root_kind,
        name: None,
        selection_set,
        variables: Default::default(),
        directives: Default::default(),
    })
}

// Recursively build a selection set bottom-up.
fn build_witness_next_step(
    schema: &ValidFederationSchema,
    witness: &TransitionGraphPath,
    edges: &[EdgeIndex],
) -> Result<Option<SelectionSet>, FederationError> {
    match edges.split_first() {
        // Base case
        None => {
            // We're at the end of our counter-example, meaning that we're at a point of traversing the
            // supergraph where we know there is no valid equivalent subgraph traversals. That said, we
            // may well not be on a terminal vertex (the type may not be a leaf), meaning that
            // returning `None` may be invalid. In that case, we instead return an empty SelectionSet.
            // This is, strictly speaking, equally invalid, but we use this as a convention to means
            // "there is supposed to be a selection but we don't have it" and the code in
            // `SelectionSet.toSelectionNode` (FED-577) handles this an prints an ellipsis (a '...').
            //
            // Note that, as an alternative, we _could_ generate a random valid witness: while the
            // current type is not terminal we would randomly pick a valid choice (if it's an abstract
            // type, we'd "cast" to any implementation; if it's an object, we'd pick the first field
            // and recurse on its type). However, while this would make sure our "witness" is always a
            // fully valid query, this is probably less user friendly in practice because you'd have to
            // follow the query manually to figure out at which point the query stop being satisfied by
            // subgraphs. Putting the ellipsis instead make it immediately clear after which part of
            // the query there is an issue.
            let QueryGraphNodeType::SchemaType(type_pos) = &witness.tail_node()?.type_ else {
                bail!("build_witness_next_step: tail type is not a schema type");
            };
            // Note that vertex types are named output types, so if it's not a leaf it is guaranteed to
            // be selectable.
            Ok(
                match CompositeTypeDefinitionPosition::try_from(type_pos.clone()) {
                    Ok(composite_type_pos) => {
                        Some(SelectionSet::empty(schema.clone(), composite_type_pos))
                    }
                    _ => None,
                },
            )
        }

        // Recursive case
        Some((edge_index, rest)) => {
            let sub_selection = build_witness_next_step(schema, witness, rest)?;
            let edge = witness.edge_weight(*edge_index)?;
            let (parent_type, selection) = match &edge.transition {
                QueryGraphEdgeTransition::Downcast {
                    source: _,
                    from_type_position,
                    to_type_position,
                } => {
                    let inline_fragment = InlineFragment {
                        schema: schema.clone(),
                        parent_type_position: from_type_position.clone(),
                        type_condition_position: Some(to_type_position.clone()),
                        directives: Default::default(),
                        selection_id: SelectionId::new(),
                    };
                    let Some(sub_selection) = sub_selection else {
                        bail!("build_witness_next_step: sub_selection is None");
                    };
                    (
                        from_type_position.clone(),
                        InlineFragmentSelection::new(inline_fragment, sub_selection).into(),
                    )
                }
                QueryGraphEdgeTransition::FieldCollection {
                    source: _,
                    field_definition_position,
                    is_part_of_provides: _,
                } => {
                    let parent_type_pos = field_definition_position.parent();
                    let Some(field) = FieldSelection::from_field(
                        &build_witness_field(schema, field_definition_position)?,
                        &parent_type_pos,
                        &Default::default(),
                        schema,
                        &|| Ok(()), // never cancels
                    )?
                    else {
                        bail!("build_witness_next_step: field is None");
                    };
                    let field = field.with_updated_selection_set(sub_selection);
                    (parent_type_pos.clone(), field.into())
                }
                _ => {
                    // Witnesses are build from a path on the supergraph, so we shouldn't have any of those edges.
                    bail!("Invalid edge {edge} found in supergraph path");
                }
            };
            Ok(Some(SelectionSet::from_selection(parent_type, selection)))
        }
    }
}

fn build_witness_field(
    schema: &ValidFederationSchema,
    field_definition_position: &FieldDefinitionPosition,
) -> Result<executable::Field, FederationError> {
    let field_def = field_definition_position.get(schema.schema())?;
    let result = executable::Field::new(field_def.name.clone(), field_def.node.clone());
    let args = field_def
        .arguments
        .iter()
        .filter_map(|arg_def| {
            // PORT_NOTE: JS implementation didn't skip optional arguments. Rust version skips them
            //            for brevity.
            if !arg_def.is_required() {
                return None;
            }
            let arg_value = match generate_witness_value(schema, arg_def) {
                Ok(value) => value,
                Err(e) => {
                    return Some(Err(e));
                }
            };
            Some(Ok(Node::new(ast::Argument {
                name: arg_def.name.clone(),
                value: arg_value,
            })))
        })
        .collect::<Result<Vec<_>, _>>()?;
    if args.is_empty() {
        Ok(result)
    } else {
        Ok(result.with_arguments(args))
    }
}

fn generate_witness_value(
    schema: &ValidFederationSchema,
    value_def: &ast::InputValueDefinition,
) -> Result<Node<ast::Value>, FederationError> {
    // Note: We always generate a non-null value, even if the value's type is nullable.
    let value = match value_def.ty.as_ref() {
        executable::Type::Named(type_name) | executable::Type::NonNullNamed(type_name) => {
            let type_pos = schema.get_type(type_name.clone())?;
            match type_pos {
                TypeDefinitionPosition::Scalar(scalar_type_pos) => {
                    match scalar_type_pos.type_name.as_str() {
                        "Int" => ast::Value::Int(0.into()),
                        #[allow(clippy::approx_constant)]
                        "Float" => ast::Value::Float((3.14).into()),
                        "Boolean" => ast::Value::Boolean(true),
                        "String" => ast::Value::String("A string value".to_string()),
                        // Users probably expect a particular format of ID at any particular place,
                        // but we have zero info on the context, so we just throw a string that
                        // hopefully make things clear.
                        "ID" => ast::Value::String("<any id>".to_string()),
                        // It's a custom scalar, but we don't know anything about that scalar so
                        // providing some random string. This will technically probably not be a
                        // valid value for that scalar, but hopefully that won't be enough to throw
                        // users off.
                        _ => ast::Value::String("<some value>".to_string()),
                    }
                }
                TypeDefinitionPosition::Enum(enum_type_pos) => {
                    let enum_type = enum_type_pos.get(schema.schema())?;
                    let Some((first_value, _)) = enum_type.values.first() else {
                        bail!("generate_witness_value: enum type has no values");
                    };
                    ast::Value::Enum(first_value.clone())
                }
                TypeDefinitionPosition::InputObject(input_object_type_pos) => {
                    let object_type = input_object_type_pos.get(schema.schema())?;
                    let fields = object_type
                        .fields
                        .iter()
                        .filter_map(|(field_name, field_def)| {
                            // We don't bother with non-mandatory fields.
                            if !field_def.is_required() {
                                return None;
                            }

                            let field_value = match generate_witness_value(schema, field_def) {
                                Ok(value) => value,
                                Err(e) => {
                                    return Some(Err(e));
                                }
                            };
                            Some(Ok((field_name.clone(), field_value)))
                        })
                        .collect::<Result<Vec<_>, _>>()?;
                    ast::Value::Object(fields)
                }
                _ => bail!("generate_witness_value: unexpected value type"),
            }
        }
        executable::Type::List(_item_type) | executable::Type::NonNullList(_item_type) => {
            ast::Value::List(vec![])
        }
    };
    Ok(Node::new(value))
}

fn display_reasons(reasons: &[Unadvanceables]) -> String {
    let mut by_subgraph = MultiIndexMap::new();
    for reason in reasons {
        for unadvanceable in reason.iter() {
            by_subgraph.insert(unadvanceable.source_subgraph(), unadvanceable)
        }
    }
    by_subgraph
        .iter()
        .filter_map(|(subgraph, reasons)| {
            let (first, rest) = reasons.split_first()?;
            let details = if rest.is_empty() {
                format!(r#" {}."#, first.details())
            } else {
                // We put all the reasons into a set because it's possible multiple paths of the
                // algorithm had the same "dead end". Typically, without this, there is cases where we
                // end up with multiple "cannot find field x" messages (for the same "x").
                let all_details = reasons
                    .iter()
                    .map(|reason| reason.details())
                    .collect::<IndexSet<_>>();
                let mut formatted_details = vec!["".to_string()]; // to add a newline
                formatted_details
                    .extend(all_details.iter().map(|details| format!("  - {details}.")));
                formatted_details.join("\n")
            };
            Some(format!(r#"- from subgraph "{subgraph}":{details}"#))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use apollo_compiler::parser::Parser;
    use insta::assert_snapshot;
    use petgraph::graph::NodeIndex;
    use petgraph::visit::EdgeRef;

    use super::*;
    use crate::query_graph::QueryGraph;
    use crate::query_graph::build_query_graph::build_query_graph;
    use crate::query_graph::condition_resolver::ConditionResolution;
    use crate::schema::position::SchemaRootDefinitionKind;

    // A helper function that enumerates transition paths.
    fn build_graph_paths(
        query_graph: &Arc<QueryGraph>,
        op_kind: SchemaRootDefinitionKind,
        depth_limit: usize,
    ) -> Result<Vec<TransitionGraphPath>, FederationError> {
        let nodes_by_kind = query_graph.root_kinds_to_nodes()?;
        let root_node_idx = nodes_by_kind[&op_kind];
        let curr_graph_path = TransitionGraphPath::new(query_graph.clone(), root_node_idx)?;
        build_graph_paths_recursive(query_graph, curr_graph_path, root_node_idx, depth_limit)
    }

    fn build_graph_paths_recursive(
        query_graph: &Arc<QueryGraph>,
        curr_path: TransitionGraphPath,
        curr_node_idx: NodeIndex,
        depth_limit: usize,
    ) -> Result<Vec<TransitionGraphPath>, FederationError> {
        if depth_limit == 0 {
            return Ok(vec![]);
        }

        let mut paths = vec![curr_path.clone()];
        for edge_ref in query_graph.out_edges(curr_node_idx) {
            let edge = edge_ref.weight();
            match &edge.transition {
                QueryGraphEdgeTransition::FieldCollection { .. }
                | QueryGraphEdgeTransition::Downcast { .. } => {
                    let new_path = curr_path
                        .add(
                            edge.transition.clone(),
                            edge_ref.id(),
                            trivial_condition(),
                            None,
                        )
                        .expect("adding edge to path");

                    // Recursively build paths from the new node
                    let new_paths = build_graph_paths_recursive(
                        query_graph,
                        new_path,
                        edge_ref.target(),
                        depth_limit - 1,
                    )?;
                    paths.extend(new_paths);
                }
                _ => {}
            }
        }
        Ok(paths)
    }

    fn trivial_condition() -> ConditionResolution {
        ConditionResolution::Satisfied {
            cost: 0.0,
            path_tree: None,
            context_map: None,
        }
    }

    fn parse_schema(schema_and_operation: &str) -> ValidFederationSchema {
        let schema = Parser::new()
            .parse_schema(schema_and_operation, "test.graphql")
            .expect("parsing schema")
            .validate()
            .expect("validating schema");
        ValidFederationSchema::new(schema).expect("creating valid federation schema")
    }

    #[test]
    fn test_build_witness_operation() {
        let schema_str = r#"
            type Query
            {
                t: T
                i: I
            }

            interface I
            {
                id: ID!
            }

            enum E { A B C }

            input MyInput {
                intInput: Int!
                enumInput: E!
                optionalInput: String
            }

            type T implements I
            {
                id: ID!
                someField(
                    numArg: Int!, floatArg: Float!, strArg: String!, boolArg: Boolean!,
                    listArg: [Int!]!, enumArg: E!, myInputArg: MyInput!, optionalArg: String
                ): String
            }
        "#;

        let schema = parse_schema(schema_str);
        let query_graph = Arc::new(
            build_query_graph("test".into(), schema.clone(), Default::default())
                .expect("building query graph"),
        );
        let result: Vec<_> = build_graph_paths(&query_graph, SchemaRootDefinitionKind::Query, 3)
            .expect("building graph paths")
            .iter()
            .filter(|path| path.iter().count() > 0)
            .map(|path| {
                let witness = build_witness_operation(path).expect("building witness operation");
                format!("{path}: {witness}")
            })
            .collect();
        assert_snapshot!(result.join("\n\n"), @r###"
        Query(test) --[t]--> T(test) (types: [T]): {
          t {
            ...
          }
        }

        Query(test) --[t]--> T(test) --[id]--> ID(test): {
          t {
            id
          }
        }

        Query(test) --[t]--> T(test) --[someField]--> String(test): {
          t {
            someField(boolArg: true, enumArg: A, floatArg: 3.14, listArg: [], myInputArg: {enumInput: A, intInput: 0}, numArg: 0, strArg: "A string value")
          }
        }

        Query(test) --[t]--> T(test) --[__typename]--> String(test): {
          t {
            __typename
          }
        }

        Query(test) --[i]--> I(test) (types: [T]): {
          i {
            ...
          }
        }

        Query(test) --[i]--> I(test) --[... on T]--> T(test) (types: [T]): {
          i {
            ... on T {
              ...
            }
          }
        }

        Query(test) --[__typename]--> String(test): {
          __typename
        }
        "###);
    }
}
