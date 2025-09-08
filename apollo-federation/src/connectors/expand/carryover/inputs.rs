use apollo_compiler::Name;
use apollo_compiler::Node;
use apollo_compiler::ast;
use apollo_compiler::ast::Value;
use apollo_compiler::collections::HashMap;
use apollo_compiler::name;
use apollo_compiler::schema::DirectiveList;
use apollo_compiler::schema::EnumType;
use apollo_compiler::schema::ExtendedType;
use apollo_compiler::schema::InputObjectType;
use apollo_compiler::schema::ScalarType;
use itertools::Itertools;
use multimap::MultiMap;

use crate::error::FederationError;
use crate::schema::FederationSchema;
use crate::schema::position::EnumTypeDefinitionPosition;
use crate::schema::position::InputObjectTypeDefinitionPosition;
use crate::schema::position::ScalarTypeDefinitionPosition;

/// merge.rs doesn't have any logic for `@composeDirective` directives, so we
/// need to carry those directives AND their associated input types over into
/// the new supergraph.
///
/// However, we can't just copy the definitions as-is, because their join__*
/// directives may reference subgraphs that no longer exist (were replaced by
/// "expanded" subgraphs/connectors). Each time we encounter a join__* directive
/// with a `graph:` argument referring to a missing subgraph, we'll need to
/// replace it with **one or more** new directives, one for each "expanded"
/// subgraph.
pub(super) fn copy_input_types(
    from: &FederationSchema,
    to: &mut FederationSchema,
    subgraph_name_replacements: &MultiMap<&str, String>,
) -> Result<(), FederationError> {
    let from_join_graph_enum = from
        .schema()
        .get_enum(&name!(join__Graph))
        .ok_or_else(|| FederationError::internal("Cannot find join__graph enum"))?;
    let to_join_graph_enum = to
        .schema()
        .get_enum(&name!(join__Graph))
        .ok_or_else(|| FederationError::internal("Cannot find join__graph enum"))?;
    let subgraph_enum_replacements = subgraph_replacements(
        from_join_graph_enum,
        to_join_graph_enum,
        subgraph_name_replacements,
    )
    .map_err(|e| FederationError::internal(format!("Failed to get subgraph replacements: {e}")))?;

    for (name, ty) in &from.schema().types {
        if to.schema().types.contains_key(name) {
            continue;
        }
        match ty {
            ExtendedType::Scalar(node) => {
                let references = from.referencers().scalar_types.get(name);
                if references.is_none_or(|refs| refs.len() == 0) {
                    continue;
                }

                let pos = ScalarTypeDefinitionPosition {
                    type_name: node.name.clone(),
                };
                let node =
                    strip_invalid_join_directives_from_scalar(node, &subgraph_enum_replacements);
                pos.pre_insert(to).ok();
                pos.insert(to, node).ok();
            }
            ExtendedType::Enum(node) => {
                let references = from.referencers().enum_types.get(name);
                if references.is_none_or(|refs| refs.len() == 0) {
                    continue;
                }

                let pos = EnumTypeDefinitionPosition {
                    type_name: node.name.clone(),
                };
                let node =
                    strip_invalid_join_directives_from_enum(node, &subgraph_enum_replacements);
                pos.pre_insert(to).ok();
                pos.insert(to, node).ok();
            }
            ExtendedType::InputObject(node) => {
                let references = from.referencers().input_object_types.get(name);
                if references.is_none_or(|refs| refs.len() == 0) {
                    continue;
                }

                let pos = InputObjectTypeDefinitionPosition {
                    type_name: node.name.clone(),
                };
                let node = strip_invalid_join_directives_from_input_type(
                    node,
                    &subgraph_enum_replacements,
                );
                pos.pre_insert(to).ok();
                pos.insert(to, node).ok();
            }
            _ => {}
        }
    }

    Ok(())
}

/// Given an original join__Graph enum:
/// ```graphql
/// enum join__Graph {
///  REGULAR_SUBGRAPH @join__graph(name: "regular-subgraph")
///  CONNECTORS_SUBGRAPH @join__graph(name: "connectors-subgraph")
/// }
/// ```
///
/// and a new join__Graph enum:
/// ```graphql
/// enum join__Graph {
///  REGULAR_SUBGRAPH @join__graph(name: "regular-subgraph")
///  CONNECTORS_SUBGRAPH_QUERY_USER_0 @join__graph(name: "connectors-subgraph_Query_user_0")
///  CONNECTORS_SUBGRAPH_QUERY_USERS_0 @join__graph(name: "connectors-subgraph_Query_users_0")
/// }
/// ```
///
/// and a map of original subgraph names to new subgraph names:
/// ```ignore
/// {
///   "connectors-subgraph" => vec!["connectors-subgraph_Query_user_0", "connectors-subgraph_Query_users_0"]
/// }
/// ```
///
/// Return a map of enum value replacements:
/// ```ignore
/// {
///   "CONNECTORS_SUBGRAPH" => vec!["CONNECTORS_SUBGRAPH_QUERY_USER_0", "CONNECTORS_SUBGRAPH_QUERY_USERS_0"],
/// }
/// ```
fn subgraph_replacements(
    from_join_graph_enum: &EnumType,
    to_join_graph_enum: &EnumType,
    replaced_subgraph_names: &MultiMap<&str, String>,
) -> Result<MultiMap<Name, Name>, String> {
    let mut replacements = MultiMap::new();

    fn subgraph_names_to_enum_values(enum_type: &EnumType) -> Result<HashMap<&str, &Name>, &str> {
        enum_type
            .values
            .iter()
            .map(|(name, value)| {
                value
                    .directives
                    .iter()
                    .find(|d| d.name == name!(join__graph))
                    .and_then(|d| {
                        d.arguments
                            .iter()
                            .find(|a| a.name == name!(name))
                            .and_then(|a| a.value.as_str())
                    })
                    .ok_or("no name argument on join__graph")
                    .map(|new_subgraph_name| (new_subgraph_name, name))
            })
            .try_collect()
    }

    let new_subgraph_names_to_enum_values = subgraph_names_to_enum_values(to_join_graph_enum)?;

    let original_subgraph_names_to_enum_values =
        subgraph_names_to_enum_values(from_join_graph_enum)?;

    for (original_subgraph_name, new_subgraph_names) in replaced_subgraph_names.iter_all() {
        if let Some(original_enum_value) = original_subgraph_names_to_enum_values
            .get(original_subgraph_name)
            .cloned()
        {
            for new_subgraph_name in new_subgraph_names {
                if let Some(new_enum_value) = new_subgraph_names_to_enum_values
                    .get(new_subgraph_name.as_str())
                    .cloned()
                {
                    replacements.insert(original_enum_value.clone(), new_enum_value.clone());
                }
            }
        }
    }

    Ok(replacements)
}

/// Given a list of directives and a directive name like `@join__type` or `@join__enumValue`,
/// replace the `graph:` argument with a new directive for each subgraph name in the
/// `replaced_subgraph_names` map.
fn replace_join_enum(
    directives: &DirectiveList,
    directive_name: &Name,
    replaced_subgraph_names: &MultiMap<Name, Name>,
) -> DirectiveList {
    let mut new_directives = DirectiveList::new();
    for d in directives.iter() {
        if &d.name == directive_name {
            let Some(graph_arg) = d
                .arguments
                .iter()
                .find(|a| a.name == name!(graph))
                .and_then(|a| a.value.as_enum())
            else {
                continue;
            };

            let Some(replacements) = replaced_subgraph_names.get_vec(graph_arg) else {
                new_directives.push(d.clone());
                continue;
            };

            for replacement in replacements {
                let mut new_directive = d.clone();
                let new_directive = new_directive.make_mut();
                if let Some(a) = new_directive
                    .arguments
                    .iter_mut()
                    .find(|a| a.name == name!(graph))
                {
                    let a = a.make_mut();
                    a.value = Value::Enum(replacement.clone()).into();
                };
                new_directives.push(new_directive.clone());
            }
        } else {
            new_directives.push(d.clone());
        }
    }
    new_directives
}

/// Unfortunately, there are two different DirectiveList types, so this
/// function is duplicated.
fn replace_join_enum_ast(
    directives: &ast::DirectiveList,
    directive_name: &Name,
    replaced_subgraph_names: &MultiMap<Name, Name>,
) -> ast::DirectiveList {
    let mut new_directives = ast::DirectiveList::new();
    for d in directives.iter() {
        if &d.name == directive_name {
            let Some(graph_arg) = d
                .arguments
                .iter()
                .find(|a| a.name == name!(graph))
                .and_then(|a| a.value.as_enum())
            else {
                continue;
            };

            let Some(replacements) = replaced_subgraph_names.get_vec(graph_arg) else {
                new_directives.push(d.clone());
                continue;
            };

            for replacement in replacements {
                let mut new_directive = d.clone();
                let new_directive = new_directive.make_mut();
                if let Some(a) = new_directive
                    .arguments
                    .iter_mut()
                    .find(|a| a.name == name!(graph))
                {
                    let a = a.make_mut();
                    a.value = Value::Enum(replacement.clone()).into();
                };
                new_directives.push(new_directive.clone());
            }
        } else {
            new_directives.push(d.clone());
        }
    }
    new_directives
}

fn strip_invalid_join_directives_from_input_type(
    node: &InputObjectType,
    replaced_subgraph_names: &MultiMap<Name, Name>,
) -> Node<InputObjectType> {
    let mut node = node.clone();

    node.directives = replace_join_enum(
        &node.directives,
        &name!(join__type),
        replaced_subgraph_names,
    );

    for (_, field) in node.fields.iter_mut() {
        let field = field.make_mut();
        field.directives = replace_join_enum_ast(
            &field.directives,
            &name!(join__field),
            replaced_subgraph_names,
        );
    }

    node.into()
}

fn strip_invalid_join_directives_from_enum(
    node: &EnumType,
    replaced_subgraph_names: &MultiMap<Name, Name>,
) -> Node<EnumType> {
    let mut node = node.clone();

    node.directives = replace_join_enum(
        &node.directives,
        &name!(join__type),
        replaced_subgraph_names,
    );

    for (_, value) in node.values.iter_mut() {
        let value = value.make_mut();
        value.directives = replace_join_enum_ast(
            &value.directives,
            &name!(join__enumValue),
            replaced_subgraph_names,
        );
    }
    node.into()
}

fn strip_invalid_join_directives_from_scalar(
    node: &ScalarType,
    replaced_subgraph_names: &MultiMap<Name, Name>,
) -> Node<ScalarType> {
    let mut node = node.clone();

    node.directives = replace_join_enum(
        &node.directives,
        &name!(join__type),
        replaced_subgraph_names,
    );

    node.into()
}
