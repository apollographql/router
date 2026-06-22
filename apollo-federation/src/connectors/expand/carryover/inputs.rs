use apollo_compiler::Name;
use apollo_compiler::Node;
use apollo_compiler::ast;
use apollo_compiler::ast::Argument;
use apollo_compiler::ast::Directive;
use apollo_compiler::ast::DirectiveLocation;
use apollo_compiler::ast::Value;
use apollo_compiler::collections::HashMap;
use apollo_compiler::collections::HashSet;
use apollo_compiler::name;
use apollo_compiler::schema::Component;
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
    subgraph_enum_replacements: &MultiMap<Name, Name>,
) -> Result<(), FederationError> {
    // The synthetic subgraphs created for each connector replace their original connector
    // subgraph, so any input type the original declared must be declared as a member of every
    // synthetic replacement (via `@join__type`, and `@join__enumValue` / `@join__field` for
    // members). Otherwise the expanded supergraph is internally inconsistent: subgraph extraction
    // copies executable directive definitions into *every* subgraph, so a synthetic subgraph that
    // is missing a directive argument's type extracts to an invalid subgraph (RH-1375). These are
    // the join__Graph enum values for the synthetic subgraphs.
    let synthetic_graphs: HashSet<Name> = subgraph_enum_replacements
        .iter_all()
        .flat_map(|(_, replacements)| replacements.iter().cloned())
        .collect();

    // Only types reached through an executable directive's arguments need their membership
    // reconciled when they already exist in the merged supergraph: those are the types extraction
    // copies into every subgraph. Types present for other reasons (e.g. consumed by a connector's
    // own fields) already carry correct membership from the merge.
    let executable_directive_types = executable_directive_input_types(from);

    for (name, ty) in &from.schema().types {
        match ty {
            ExtendedType::Scalar(node) => {
                let references = from.referencers().scalar_types.get(name);
                if references.is_none_or(|refs| refs.len() == 0) {
                    continue;
                }

                let pos = ScalarTypeDefinitionPosition {
                    type_name: name.clone(),
                };
                let node =
                    strip_invalid_join_directives_from_scalar(node, subgraph_enum_replacements);
                if to.schema().types.contains_key(name) {
                    if executable_directive_types.contains(name) {
                        add_synthetic_scalar_membership(to, &pos, &node, &synthetic_graphs)?;
                    }
                } else {
                    pos.pre_insert(to).ok();
                    pos.insert(to, node).ok();
                }
            }
            ExtendedType::Enum(node) => {
                let references = from.referencers().enum_types.get(name);
                if references.is_none_or(|refs| refs.len() == 0) {
                    continue;
                }

                let pos = EnumTypeDefinitionPosition {
                    type_name: name.clone(),
                };
                let node =
                    strip_invalid_join_directives_from_enum(node, subgraph_enum_replacements);
                if to.schema().types.contains_key(name) {
                    if executable_directive_types.contains(name) {
                        add_synthetic_enum_membership(to, &pos, &node, &synthetic_graphs)?;
                    }
                } else {
                    pos.pre_insert(to).ok();
                    pos.insert(to, node).ok();
                }
            }
            ExtendedType::InputObject(node) => {
                let references = from.referencers().input_object_types.get(name);
                if references.is_none_or(|refs| refs.len() == 0) {
                    continue;
                }

                let pos = InputObjectTypeDefinitionPosition {
                    type_name: name.clone(),
                };
                let node =
                    strip_invalid_join_directives_from_input_type(node, subgraph_enum_replacements);
                if to.schema().types.contains_key(name) {
                    if executable_directive_types.contains(name) {
                        add_synthetic_input_object_membership(to, &pos, &node, &synthetic_graphs)?;
                    }
                } else {
                    pos.pre_insert(to).ok();
                    pos.insert(to, node).ok();
                }
            }
            _ => {}
        }
    }

    Ok(())
}

/// Executable directive locations; mirrors `EXECUTABLE_DIRECTIVE_LOCATIONS` in `supergraph/mod.rs`,
/// which governs which directive definitions extraction copies into every subgraph.
const EXECUTABLE_DIRECTIVE_LOCATIONS: [DirectiveLocation; 8] = [
    DirectiveLocation::Query,
    DirectiveLocation::Mutation,
    DirectiveLocation::Subscription,
    DirectiveLocation::Field,
    DirectiveLocation::FragmentDefinition,
    DirectiveLocation::FragmentSpread,
    DirectiveLocation::InlineFragment,
    DirectiveLocation::VariableDefinition,
];

/// The input types referenced — transitively, through input-object fields — by the arguments of
/// the supergraph's executable directive definitions. Extraction copies executable directive
/// definitions into every extracted subgraph, so these types must exist in every subgraph,
/// including the synthetic connector subgraphs (RH-1375).
fn executable_directive_input_types(from: &FederationSchema) -> HashSet<Name> {
    let mut referenced = HashSet::default();
    let mut stack: Vec<Name> = Vec::new();
    for directive in from.schema().directive_definitions.values() {
        if directive.is_built_in() {
            continue;
        }
        if directive
            .locations
            .iter()
            .any(|location| EXECUTABLE_DIRECTIVE_LOCATIONS.contains(location))
        {
            for argument in &directive.arguments {
                stack.push(argument.ty.inner_named_type().clone());
            }
        }
    }
    while let Some(name) = stack.pop() {
        if !referenced.insert(name.clone()) {
            continue;
        }
        if let Some(ExtendedType::InputObject(input_object)) = from.schema().types.get(&name) {
            for field in input_object.fields.values() {
                stack.push(field.ty.inner_named_type().clone());
            }
        }
    }
    referenced
}

/// The `graph:` argument of a `@join__*` directive, if present.
fn directive_graph(directive: &Directive) -> Option<&Name> {
    directive
        .arguments
        .iter()
        .find(|a| a.name == name!(graph))
        .and_then(|a| a.value.as_enum())
}

/// Clone a `@join__*` directive, replacing its `graph:` argument with `graph`.
fn directive_with_graph(directive: &Node<Directive>, graph: &Name) -> Node<Directive> {
    let mut directive = directive.clone();
    if let Some(argument) = directive
        .make_mut()
        .arguments
        .iter_mut()
        .find(|a| a.name == name!(graph))
    {
        argument.make_mut().value = Value::Enum(graph.clone()).into();
    }
    directive
}

/// The graphs named by a type's `@join__type` directives.
fn join_type_graphs(type_directives: &DirectiveList) -> HashSet<Name> {
    type_directives
        .iter()
        .filter(|d| d.name == name!(join__type))
        .filter_map(|d| directive_graph(d).cloned())
        .collect()
}

/// The synthetic connector subgraphs this type should additionally belong to: the graphs named by
/// the type-level `@join__type` directives that `strip_invalid_join_directives_*` rewrote onto the
/// synthetic subgraphs (i.e. the replacements of the connector subgraph the type came from), minus
/// any the existing definition already declares (a type the connector itself uses already carries
/// correct synthetic membership from the merge).
fn synthetic_membership_graphs(
    desired_type_directives: &DirectiveList,
    existing_type_directives: &DirectiveList,
    synthetic_graphs: &HashSet<Name>,
) -> Vec<Name> {
    let existing = join_type_graphs(existing_type_directives);
    desired_type_directives
        .iter()
        .filter(|d| d.name == name!(join__type))
        .filter_map(|d| directive_graph(d))
        .filter(|graph| synthetic_graphs.contains(*graph) && !existing.contains(*graph))
        .cloned()
        .collect()
}

/// For each member directive (`@join__enumValue` / `@join__field`) the existing definition already
/// carries for a *real* subgraph, plan an equivalent directive for each synthetic graph that
/// doesn't have one yet. Cloning the existing directive preserves its other arguments (e.g.
/// `@join__field`'s `type:`). A member with no such directive relies on type-level `@join__type`
/// membership and needs nothing added. Returns `(member name, directives to add)` pairs.
fn member_membership_additions<'a>(
    members: impl Iterator<Item = (&'a Name, &'a ast::DirectiveList)>,
    member_directive_name: &Name,
    synthetic_graphs: &[Name],
) -> Vec<(Name, Vec<Node<Directive>>)> {
    let mut additions = Vec::new();
    for (member_name, directives) in members {
        let existing: Vec<&Node<Directive>> = directives
            .iter()
            .filter(|d| &d.name == member_directive_name)
            .collect();
        let Some(template) = existing.first() else {
            continue;
        };
        let existing_graphs: HashSet<&Name> =
            existing.iter().filter_map(|d| directive_graph(d)).collect();
        let to_add: Vec<Node<Directive>> = synthetic_graphs
            .iter()
            .filter(|graph| !existing_graphs.contains(*graph))
            .map(|graph| directive_with_graph(template, graph))
            .collect();
        if !to_add.is_empty() {
            additions.push((member_name.clone(), to_add));
        }
    }
    additions
}

/// When a directive-referenced input type is already in the merged supergraph (contributed by a
/// non-connector subgraph, so it only carries that subgraph's `@join__type` membership), add the
/// membership for the synthetic connector subgraphs that replaced the original connector subgraph.
/// `desired` is the original type with its type-level membership already rewritten onto the
/// synthetic subgraphs by the `strip_invalid_join_directives_*` helpers.
fn add_synthetic_scalar_membership(
    to: &mut FederationSchema,
    pos: &ScalarTypeDefinitionPosition,
    desired: &Node<ScalarType>,
    synthetic_graphs: &HashSet<Name>,
) -> Result<(), FederationError> {
    let graphs = match to.schema().types.get(&pos.type_name) {
        Some(ExtendedType::Scalar(scalar)) => {
            synthetic_membership_graphs(&desired.directives, &scalar.directives, synthetic_graphs)
        }
        _ => return Ok(()),
    };
    for graph in graphs {
        pos.insert_directive(to, join_type_directive(&graph))?;
    }
    Ok(())
}

/// See [`add_synthetic_scalar_membership`]. Enum values carry their own `@join__enumValue`
/// membership, so we mirror each value's existing membership onto the synthetic subgraphs.
fn add_synthetic_enum_membership(
    to: &mut FederationSchema,
    pos: &EnumTypeDefinitionPosition,
    desired: &Node<EnumType>,
    synthetic_graphs: &HashSet<Name>,
) -> Result<(), FederationError> {
    let (graphs, value_additions) = match to.schema().types.get(&pos.type_name) {
        Some(ExtendedType::Enum(enum_type)) => {
            let graphs = synthetic_membership_graphs(
                &desired.directives,
                &enum_type.directives,
                synthetic_graphs,
            );
            let value_additions = member_membership_additions(
                enum_type
                    .values
                    .iter()
                    .map(|(name, value)| (name, &value.directives)),
                &name!(join__enumValue),
                &graphs,
            );
            (graphs, value_additions)
        }
        _ => return Ok(()),
    };
    for graph in &graphs {
        pos.insert_directive(to, join_type_directive(graph))?;
    }
    for (value_name, directives) in value_additions {
        let value_pos = pos.value(value_name);
        for directive in directives {
            value_pos.insert_directive(to, directive)?;
        }
    }
    Ok(())
}

/// See [`add_synthetic_scalar_membership`]. Input object fields carry their own `@join__field`
/// membership, so we mirror each field's existing membership onto the synthetic subgraphs.
fn add_synthetic_input_object_membership(
    to: &mut FederationSchema,
    pos: &InputObjectTypeDefinitionPosition,
    desired: &Node<InputObjectType>,
    synthetic_graphs: &HashSet<Name>,
) -> Result<(), FederationError> {
    let (graphs, field_additions) = match to.schema().types.get(&pos.type_name) {
        Some(ExtendedType::InputObject(input_object)) => {
            let graphs = synthetic_membership_graphs(
                &desired.directives,
                &input_object.directives,
                synthetic_graphs,
            );
            let field_additions = member_membership_additions(
                input_object
                    .fields
                    .iter()
                    .map(|(name, field)| (name, &field.directives)),
                &name!(join__field),
                &graphs,
            );
            (graphs, field_additions)
        }
        _ => return Ok(()),
    };
    for graph in &graphs {
        pos.insert_directive(to, join_type_directive(graph))?;
    }
    for (field_name, directives) in field_additions {
        let field_pos = pos.field(field_name);
        for directive in directives {
            field_pos.insert_directive(to, directive)?;
        }
    }
    Ok(())
}

/// A bare `@join__type(graph: <graph>)` type-level membership directive.
fn join_type_directive(graph: &Name) -> Component<Directive> {
    Component::new(Directive {
        name: name!(join__type),
        arguments: vec![Node::new(Argument {
            name: name!(graph),
            value: Node::new(Value::Enum(graph.clone())),
        })],
    })
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
pub(super) fn subgraph_replacements(
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

pub(super) fn subgraph_names_to_enum_values(
    enum_type: &EnumType,
) -> Result<HashMap<&str, &Name>, &str> {
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

const JOIN_DIRECTIVE_GRAPHS_ARGUMENT_NAME: Name = name!(graphs);

/// Given a @join__directive, this will replace the original subgraph name with
/// the "subgraphs" generated for each connector in the "expansion" process.
///
/// @join__directive(graphs: [CONNECTORS])
/// becomes
/// @join__directive(graphs: [CONNECTORS_QUERY_FOO_0, CONNECTORS_QUERY_FOO_1])
///
/// We don't want to include directives to the connect spec.
pub(super) fn replace_join_directive_graphs_argument(
    directive: &Node<Directive>,
    replaced_subgraph_names: &MultiMap<Name, Name>,
    connect_directive_names: &HashMap<&str, [Name; 2]>,
) -> Option<Node<Directive>> {
    // Get the graphs argument (which should be a list)
    let Some(graphs_list) = directive
        .arguments
        .iter()
        .find(|a| a.name == JOIN_DIRECTIVE_GRAPHS_ARGUMENT_NAME)
        .and_then(|a| a.value.as_list())
    else {
        // No graphs argument or not a list, return as-is
        return Some(directive.clone());
    };

    // Process each enum value in the graphs list
    let mut new_graph_values = Vec::new();
    for graph_value in graphs_list {
        if let Some(graph_enum) = graph_value.as_enum() {
            // if the serialized directive is a connect or source directive in
            // this subgraph (considering renames), then we'll skip this entirely
            if let Some(names_to_ignore) = connect_directive_names.get(graph_enum.as_str()) {
                let names_to_ignore: Vec<&str> =
                    names_to_ignore.iter().map(|n| n.as_str()).collect();
                if directive
                    .specified_argument_by_name("name")
                    .and_then(|v| v.as_str())
                    .map(|v| names_to_ignore.contains(&v))
                    .unwrap_or(false)
                {
                    return None;
                }
            }

            // Check if this graph needs replacement
            if let Some(replacements) = replaced_subgraph_names.get_vec(graph_enum) {
                // Add all replacement values
                for replacement in replacements {
                    new_graph_values.push(Node::new(Value::Enum(replacement.clone())));
                }
            } else {
                // Keep the original value
                new_graph_values.push(graph_value.clone());
            }
        } else {
            // Not an enum value, keep as-is (shouldn't happen in valid schema)
            new_graph_values.push(graph_value.clone());
        }
    }

    // If the graphs list is empty after processing, don't carry over the directive.
    // This filters out the buggy @join__directive(graphs: [], name: "link", ...)
    // that was created by the license enforcement code before our fix.
    // The correct directive with populated graphs will be created separately.
    if new_graph_values.is_empty() {
        return None;
    }

    // Create a new directive with the updated graphs argument
    let mut new_directive = directive.clone();
    if let Some(graphs_arg) = new_directive
        .make_mut()
        .arguments
        .iter_mut()
        .find(|a| a.name == JOIN_DIRECTIVE_GRAPHS_ARGUMENT_NAME)
    {
        let graphs_arg = graphs_arg.make_mut();
        graphs_arg.value = Value::List(new_graph_values).into();
    }

    Some(new_directive)
}
