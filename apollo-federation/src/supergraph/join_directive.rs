//! @join__directive extraction
use std::sync::Arc;

use apollo_compiler::ast::Argument;
use apollo_compiler::ast::Directive;
use apollo_compiler::collections::IndexMap;
use apollo_compiler::schema::Component;
use apollo_compiler::Name;
use apollo_compiler::Node;
use itertools::Itertools;

use super::get_subgraph;
use super::subgraph::FederationSubgraphs;
use crate::error::FederationError;
use crate::link::DEFAULT_LINK_NAME;
use crate::schema::position::ObjectFieldDefinitionPosition;
use crate::schema::position::TypeDefinitionPosition;
use crate::schema::FederationSchema;
use crate::sources::connect::ConnectSpecDefinition;

static JOIN_DIRECTIVE: &str = "join__directive";

/// Converts `@join__directive(graphs: [A], name: "foo")` to `@foo` in the A subgraph.
/// If the directive is a link directive on the schema definition, we also need
/// to update the metadata and add the imported definitions.
pub(super) fn extract(
    supergraph_schema: &FederationSchema,
    subgraphs: &mut FederationSubgraphs,
    graph_enum_value_name_to_subgraph_name: &IndexMap<Name, Arc<str>>,
) -> Result<(), FederationError> {
    let join_directives = match supergraph_schema
        .referencers()
        .get_directive(JOIN_DIRECTIVE)
    {
        Ok(directives) => directives,
        Err(_) => {
            // No join directives found, nothing to do.
            return Ok(());
        }
    };

    if let Some(schema_def_pos) = &join_directives.schema {
        let schema_def = schema_def_pos.get(supergraph_schema.schema());
        let directives = schema_def
            .directives
            .iter()
            .filter_map(|d| {
                if d.name == JOIN_DIRECTIVE {
                    Some(to_real_directive(d))
                } else {
                    None
                }
            })
            .collect_vec();

        // TODO: Do we need to handle the link directive being renamed?
        let (links, others) = directives
            .into_iter()
            .partition::<Vec<_>, _>(|(d, _)| d.name == DEFAULT_LINK_NAME);

        // After adding links, we'll check the link against a safelist of
        // specs and check_or_add the spec definitions if necessary.
        for (link_directive, subgraph_enum_values) in links {
            for subgraph_enum_value in subgraph_enum_values {
                let subgraph = get_subgraph(
                    subgraphs,
                    graph_enum_value_name_to_subgraph_name,
                    &subgraph_enum_value,
                )?;

                schema_def_pos.insert_directive(
                    &mut subgraph.schema,
                    Component::new(link_directive.clone()),
                )?;

                if ConnectSpecDefinition::from_directive(&link_directive)?.is_some() {
                    ConnectSpecDefinition::check_or_add(&mut subgraph.schema)?;
                }
            }
        }

        // Other directives are added normally.
        for (directive, subgraph_enum_values) in others {
            for subgraph_enum_value in subgraph_enum_values {
                let subgraph = get_subgraph(
                    subgraphs,
                    graph_enum_value_name_to_subgraph_name,
                    &subgraph_enum_value,
                )?;

                schema_def_pos
                    .insert_directive(&mut subgraph.schema, Component::new(directive.clone()))?;
            }
        }
    }

    for object_field_pos in &join_directives.object_fields {
        let object_field = object_field_pos.get(supergraph_schema.schema())?;
        let directives = object_field
            .directives
            .iter()
            .filter_map(|d| {
                if d.name == JOIN_DIRECTIVE {
                    Some(to_real_directive(d))
                } else {
                    None
                }
            })
            .collect_vec();

        for (directive, subgraph_enum_values) in directives {
            for subgraph_enum_value in subgraph_enum_values {
                let subgraph = get_subgraph(
                    subgraphs,
                    graph_enum_value_name_to_subgraph_name,
                    &subgraph_enum_value,
                )?;

                object_field_pos
                    .insert_directive(&mut subgraph.schema, Node::new(directive.clone()))?;
            }
        }
    }

    for intf_field_pos in &join_directives.interface_fields {
        let intf_field = intf_field_pos.get(supergraph_schema.schema())?;
        let directives = intf_field
            .directives
            .iter()
            .filter_map(|d| {
                if d.name == JOIN_DIRECTIVE {
                    Some(to_real_directive(d))
                } else {
                    None
                }
            })
            .collect_vec();

        for (directive, subgraph_enum_values) in directives {
            for subgraph_enum_value in subgraph_enum_values {
                let subgraph = get_subgraph(
                    subgraphs,
                    graph_enum_value_name_to_subgraph_name,
                    &subgraph_enum_value,
                )?;

                if subgraph
                    .schema
                    .try_get_type(intf_field_pos.type_name.clone())
                    .map(|t| matches!(t, TypeDefinitionPosition::Interface(_)))
                    .unwrap_or_default()
                {
                    intf_field_pos
                        .insert_directive(&mut subgraph.schema, Node::new(directive.clone()))?;
                } else {
                    // In the subgraph it's defined as an object with @interfaceObject
                    let object_field_pos = ObjectFieldDefinitionPosition {
                        type_name: intf_field_pos.type_name.clone(),
                        field_name: intf_field_pos.field_name.clone(),
                    };
                    object_field_pos
                        .insert_directive(&mut subgraph.schema, Node::new(directive.clone()))?;
                }
            }
        }
    }

    // TODO
    // - join_directives.directive_arguments
    // - join_directives.enum_types
    // - join_directives.enum_values
    // - join_directives.input_object_fields
    // - join_directives.input_object_types
    // - join_directives.interface_field_arguments
    // - join_directives.interface_types
    // - join_directives.object_field_arguments
    // - join_directives.object_types
    // - join_directives.scalar_types
    // - join_directives.union_types

    Ok(())
}

fn to_real_directive(directive: &Node<Directive>) -> (Directive, Vec<Name>) {
    let subgraph_enum_values = directive
        .specified_argument_by_name("graphs")
        .and_then(|arg| arg.as_list())
        .map(|list| {
            list.iter()
                .map(|node| {
                    Name::new(
                        node.as_enum()
                            .expect("join__directive(graphs:) value is an enum")
                            .as_str(),
                    )
                    .expect("join__directive(graphs:) value is a valid name")
                })
                .collect()
        })
        .expect("join__directive(graphs:) missing");

    let name = directive
        .specified_argument_by_name("name")
        .expect("join__directive(name:) is present")
        .as_str()
        .expect("join__directive(name:) is a string");

    let arguments = directive
        .specified_argument_by_name("args")
        .and_then(|a| a.as_object())
        .map(|args| {
            args.iter()
                .map(|(k, v)| {
                    Argument {
                        name: k.clone(),
                        value: v.clone(),
                    }
                    .into()
                })
                .collect()
        })
        .unwrap_or_default();

    let directive = Directive {
        name: Name::new(name).expect("join__directive(name:) is a valid name"),
        arguments,
    };

    (directive, subgraph_enum_values)
}
