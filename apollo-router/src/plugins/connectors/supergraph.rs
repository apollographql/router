use std::collections::HashMap;
use std::sync::Arc;

use apollo_compiler::ast;
use apollo_compiler::ast::Selection;
// use apollo_compiler::name;
// use apollo_compiler::schema::ComponentName;
// use apollo_compiler::name;
use apollo_compiler::schema::EnumValueDefinition;
use apollo_compiler::schema::ExtendedType;
use apollo_compiler::schema::FieldDefinition;
use apollo_compiler::schema::InputValueDefinition;
use apollo_compiler::schema::Name;
// use apollo_compiler::schema::UnionType;
use apollo_compiler::Node;
use apollo_compiler::Schema;
// use indexmap::IndexSet;
use itertools::Itertools;

use super::connector::Connector;
use super::connector::ConnectorKind;
use super::join_spec_helpers::add_entities_field;
use super::join_spec_helpers::add_input_join_field_directive;
use super::join_spec_helpers::add_join_enum_value_directive;
use super::join_spec_helpers::add_join_field_directive;
use super::join_spec_helpers::add_join_implements;
use super::join_spec_helpers::add_join_type_directive;
use super::join_spec_helpers::add_join_union_member_directive;
use super::join_spec_helpers::make_any_scalar;

/// Generate a list of changes to apply to the new schame
pub(super) fn make_changes(
    connector: &Connector,
    schema: &Schema,
    subgraph_enum_map: &HashMap<String, String>,
) -> Result<Vec<Change>, ConnectorSupergraphError> {
    let graph_name = connector.name.clone();
    let origin_subgraph_name = subgraph_enum_map
        .get(connector.origin_subgraph.as_str())
        .ok_or_else(|| InvalidOuterSupergraph("missing origin subgraph".into()))?;

    match &connector.kind {
        // Root fields: add the parent type and the field, then recursively
        // add the selections
        ConnectorKind::RootField {
            field_name,
            output_type_name,
            parent_type_name,
        } => {
            let mut changes = vec![
                Change::Type {
                    name: parent_type_name.clone(),
                    graph_name: Arc::clone(&graph_name),
                    key: None,
                    is_interface_object: false,
                    implements: None,
                },
                Change::Field {
                    type_name: parent_type_name.clone(),
                    field_name: field_name.clone(),
                    graph_name: Arc::clone(&graph_name),
                },
            ];

            changes.extend(recurse_selection(
                origin_subgraph_name,
                Arc::clone(&graph_name),
                schema,
                output_type_name,
                schema
                    .types
                    .get(output_type_name)
                    .ok_or_else(|| MissingType(output_type_name.to_string()))?,
                None,
                &connector.output_selection,
            )?);

            let field_def = schema
                .type_field(parent_type_name, field_name)
                .map_err(|_| MissingField(parent_type_name.to_string(), field_name.to_string()))?;

            for arg in field_def.arguments.iter() {
                changes.extend(recurse_inputs(Arc::clone(&graph_name), schema, arg)?);
            }

            Ok(changes)
        }
        // Entity: add the type with appropriate keys, add a finder field,
        // recursively add the selections, and recursively add the key fields if necessary
        ConnectorKind::Entity {
            type_name,
            key,
            is_interface_object,
            ..
        } => {
            let mut changes = vec![Change::Type {
                name: type_name.clone(),
                graph_name: Arc::clone(&graph_name),
                key: Some(key.clone()),
                is_interface_object: *is_interface_object,
                implements: None,
            }];

            if let Some(finder_field) = connector.finder_field_name() {
                changes.push(Change::MagicFinder {
                    type_name: type_name.clone(),
                    field_name: Name::new_unchecked(finder_field.as_str().into()),
                    graph_name: Arc::clone(&graph_name),
                });
            }

            changes.extend(recurse_selection(
                origin_subgraph_name,
                Arc::clone(&graph_name),
                schema,
                type_name,
                schema
                    .types
                    .get(type_name)
                    .ok_or_else(|| MissingType(type_name.to_string()))?,
                Some(key.clone()), // if this is an entity interface, the implementing types need the key too
                &connector.output_selection,
            )?);

            // TODO need a test with a nested composite key
            // TODO mark key fields as external if necessary
            changes.extend(recurse_selection(
                origin_subgraph_name,
                graph_name,
                schema,
                type_name,
                schema
                    .types
                    .get(type_name)
                    .ok_or_else(|| MissingType(type_name.to_string()))?,
                None,
                &connector.input_selection,
            )?);

            Ok(changes)
        }
        // Entity field: add the parent entity type with appropriate keys,
        // add the field itself, add a finder field, recursively add the
        // selections, and recursively add the key fields if necessary
        ConnectorKind::EntityField {
            field_name,
            type_name,
            output_type_name,
            key,
            on_interface_object,
            ..
        } => {
            let mut changes = vec![
                Change::Type {
                    name: type_name.clone(),
                    graph_name: Arc::clone(&graph_name),
                    key: Some(key.clone()),
                    is_interface_object: *on_interface_object,
                    implements: None,
                },
                Change::Field {
                    type_name: type_name.clone(),
                    field_name: field_name.clone(),
                    graph_name: Arc::clone(&graph_name),
                },
            ];

            if let Some(finder_field) = connector.finder_field_name() {
                changes.push(Change::MagicFinder {
                    type_name: type_name.clone(),
                    field_name: Name::new_unchecked(finder_field.as_str().into()),
                    graph_name: Arc::clone(&graph_name),
                });
            }

            changes.extend(recurse_selection(
                origin_subgraph_name,
                Arc::clone(&graph_name),
                schema,
                output_type_name,
                schema
                    .types
                    .get(output_type_name)
                    .ok_or_else(|| MissingType(output_type_name.to_string()))?,
                Some(key.clone()), // if this is an entity interface, the implementing types need the key too
                &connector.output_selection,
            )?);

            // TODO need a test with a nested composite key
            // TODO mark key fields as external if necessary
            changes.extend(recurse_selection(
                origin_subgraph_name,
                Arc::clone(&graph_name),
                schema,
                type_name, // key fields are on the entity type, not the output type
                schema
                    .types
                    .get(type_name.as_str())
                    .ok_or_else(|| MissingType(type_name.to_string()))?,
                None,
                &connector.input_selection,
            )?);

            let field_def = schema
                .type_field(type_name, field_name)
                .map_err(|_| MissingField(type_name.to_string(), field_name.to_string()))?;

            for arg in field_def.arguments.iter() {
                changes.extend(recurse_inputs(Arc::clone(&graph_name), schema, arg)?);
            }

            Ok(changes)
        }
    }
}

/// A "change" is a unit of work that can be applied to a schema. Each connector
/// produces a set of changes to include types, fields, and applies
/// `@join__` directives appropriately so that the query planner can extract
/// subgraphs for each connector.
#[derive(Debug)]
pub(super) enum Change {
    /// Include a type in the schema and add the `@join__type` directive
    Type {
        name: Name,
        graph_name: Arc<String>,
        key: Option<String>,
        is_interface_object: bool,
        implements: Option<Name>,
    },
    /// Include a field on a type in the schema and add the `@join__field` directive
    /// TODO: currently assumes that the type already exists (order matters!)
    Field {
        type_name: Name,
        field_name: Name,
        graph_name: Arc<String>,
    },
    InputField {
        type_name: Name,
        field_name: Name,
        graph_name: Arc<String>,
    },
    /// Add a special field to Query that we can use instead of `_entities`
    MagicFinder {
        type_name: Name,
        field_name: Name,
        graph_name: Arc<String>,
    },
    /// Add an enum value
    EnumValue {
        enum_name: Name,
        value_name: Name,
        graph_name: Arc<String>,
    },
    /// Union member
    UnionMember {
        union_name: Name,
        member_name: Name,
        graph_name: Arc<String>,
    },
}

impl Change {
    /// Apply this change to a schema, generating or modifying types and fields
    pub(super) fn apply_to(
        &self,
        original_schema: &Schema,
        schema: &mut Schema,
    ) -> Result<(), ConnectorSupergraphError> {
        match self {
            Change::Type {
                name,
                graph_name,
                key,
                is_interface_object,
                implements,
            } => {
                let ty = upsert_type(original_schema, schema, name)?;
                if !ty.is_built_in() {
                    add_join_type_directive(
                        ty,
                        graph_name,
                        key.clone(),
                        Some(*is_interface_object),
                    );
                }
                if let Some(implements) = implements {
                    add_join_implements(ty, graph_name, implements);
                }
            }
            Change::Field {
                type_name,
                field_name,
                graph_name,
            } => {
                if let Some(field) = upsert_field(original_schema, schema, type_name, field_name)? {
                    add_join_field_directive(field, graph_name);
                }
            }
            Change::InputField {
                type_name,
                field_name,
                graph_name,
            } => {
                let field = upsert_input_field(original_schema, schema, type_name, field_name)?;
                add_input_join_field_directive(field, graph_name);
            }
            Change::MagicFinder {
                type_name,
                graph_name,
                field_name,
            } => {
                {
                    let arg_ty = add_type(schema, "_Any", make_any_scalar())?;
                    add_join_type_directive(arg_ty, graph_name, None, None);
                }

                let ty = upsert_type(original_schema, schema, "Query")?;
                add_join_type_directive(ty, graph_name, None, None);

                add_entities_field(ty, graph_name, field_name, type_name);
            }
            Change::EnumValue {
                enum_name,
                value_name,
                graph_name,
            } => {
                let ty = upsert_type(original_schema, schema, enum_name)?;
                add_join_type_directive(ty, graph_name, None, None);
                if let ExtendedType::Enum(enm) = ty {
                    let value = enm.make_mut().values.entry(value_name.clone()).or_insert(
                        EnumValueDefinition {
                            description: Default::default(),
                            value: value_name.clone(),
                            directives: Default::default(),
                        }
                        .into(),
                    );
                    let value = value.make_mut();
                    add_join_enum_value_directive(value, graph_name);
                }
            }

            Change::UnionMember {
                union_name,
                member_name,
                graph_name,
            } => {
                let ty = upsert_type(original_schema, schema, union_name)?;
                match ty {
                    ExtendedType::Union(un) => {
                        let un = un.make_mut();
                        un.members.insert(member_name.clone().into());
                    }
                    _ => {
                        return Err(Invariant(
                            "Cannot add union member to non-union type".into(),
                        ))
                    }
                }
                add_join_union_member_directive(ty, graph_name, member_name);
            }
        }
        Ok(())
    }
}

fn upsert_field<'a>(
    source: &Schema,
    dest: &'a mut Schema,
    type_name: &Name,
    field_name: &Name,
) -> Result<Option<&'a mut FieldDefinition>, ConnectorSupergraphError> {
    let new_ty = dest
        .types
        .get_mut(type_name)
        .ok_or_else(|| MissingType(type_name.to_string()))?;

    if let Ok(field) = source.type_field(type_name, field_name) {
        let new_field = match new_ty {
            ExtendedType::Object(ref mut ty) => ty
                .make_mut()
                .fields
                .entry(field_name.clone())
                .or_insert_with(|| clean_copy_of_field(field).into()),
            ExtendedType::Interface(ref mut ty) => ty
                .make_mut()
                .fields
                .entry(field_name.clone())
                .or_insert_with(|| clean_copy_of_field(field).into()),
            _ => {
                return Err(Invariant(
                    "Cannot copy field into non-composite type".into(),
                ))
            }
        };

        Ok(Some(new_field.make_mut()))
    } else {
        Ok(None)
    }
}

fn upsert_input_field<'a>(
    source: &Schema,
    dest: &'a mut Schema,
    type_name: &Name,
    field_name: &Name,
) -> Result<&'a mut InputValueDefinition, ConnectorSupergraphError> {
    let new_ty = dest
        .types
        .get_mut(type_name)
        .ok_or_else(|| MissingType(type_name.to_string()))?;

    let ty = source
        .get_input_object(type_name)
        .ok_or_else(|| MissingType(type_name.to_string()))?;

    let field = ty
        .fields
        .get(field_name)
        .ok_or_else(|| MissingField(type_name.to_string(), field_name.to_string()))?;

    let new_field = match new_ty {
        ExtendedType::InputObject(ref mut ty) => ty
            .make_mut()
            .fields
            .entry(field_name.clone())
            .or_insert_with(|| clean_copy_of_input_field(field).into()),
        _ => {
            return Err(Invariant(
                "Cannot copy field into non-composite type".into(),
            ))
        }
    };

    Ok(new_field.make_mut())
}

fn upsert_type<'a>(
    source: &Schema,
    dest: &'a mut Schema,
    name: &str,
) -> Result<&'a mut ExtendedType, ConnectorSupergraphError> {
    let original = source
        .types
        .get(name)
        .ok_or_else(|| MissingType(name.to_string()))?;

    if source
        .root_operation(apollo_compiler::executable::OperationType::Query)
        .map(|op| op.as_str() == name)
        .unwrap_or(false)
    {
        dest.schema_definition.make_mut().query = Some(ast::Name::new(name)?.into());
    }

    if source
        .root_operation(apollo_compiler::executable::OperationType::Mutation)
        .map(|op| op.as_str() == name)
        .unwrap_or(false)
    {
        dest.schema_definition.make_mut().mutation = Some(ast::Name::new(name)?.into());
    }

    let ty = dest
        .types
        .entry(ast::Name::new(name)?)
        .or_insert_with(|| clean_copy_of_type(original));

    Ok(ty)
}

fn add_type<'a>(
    dest: &'a mut Schema,
    name: &str,
    ty: ExtendedType,
) -> Result<&'a mut ExtendedType, ConnectorSupergraphError> {
    Ok(dest
        .types
        .entry(ast::Name::new(name)?)
        .or_insert_with(|| ty))
}

fn clean_copy_of_field(f: &FieldDefinition) -> FieldDefinition {
    let mut f = f.clone();
    f.directives.clear();
    f
}

fn clean_copy_of_input_field(f: &InputValueDefinition) -> InputValueDefinition {
    let mut f = f.clone();
    f.directives.clear();
    f
}

fn clean_copy_of_type(ty: &ExtendedType) -> ExtendedType {
    match ty.clone() {
        ExtendedType::Object(mut ty) => {
            let ty = ty.make_mut();
            ty.directives.clear();
            ty.fields.clear();
            ExtendedType::Object(ty.clone().into())
        }
        ExtendedType::Interface(mut ty) => {
            let ty = ty.make_mut();
            ty.directives.clear();
            ty.fields.clear();
            ExtendedType::Interface(ty.clone().into())
        }
        ExtendedType::Union(mut ty) => {
            let ty = ty.make_mut();
            ty.directives.clear();
            ty.members.clear();
            ExtendedType::Union(ty.clone().into())
        }
        ExtendedType::Enum(mut ty) => {
            let ty = ty.make_mut();
            ty.directives.clear();
            ty.values.clear();
            ExtendedType::Enum(ty.clone().into())
        }
        ExtendedType::Scalar(mut ty) => {
            let ty = ty.make_mut();
            ty.directives.clear();
            ExtendedType::Scalar(ty.clone().into())
        }
        ExtendedType::InputObject(mut ty) => {
            let ty = ty.make_mut();
            ty.directives.clear();
            ty.fields.clear();
            ExtendedType::InputObject(ty.clone().into())
        }
    }
}

fn recurse_selection(
    origin_graph: &str,
    graph_name: Arc<String>,
    schema: &Schema,
    type_name: &Name,
    ty: &ExtendedType,
    // If we're adding an entity interface, we must ensure that any implementing
    // types have the same key for the subgraph to be valid
    parent_entity_interface_key: Option<String>,
    selections: &Vec<Selection>,
) -> Result<Vec<Change>, ConnectorSupergraphError> {
    let mut mutations = Vec::new();

    mutations.push(Change::Type {
        name: type_name.clone(),
        graph_name: Arc::clone(&graph_name),
        key: None,
        is_interface_object: false,
        implements: None,
    });

    match ty {
        ExtendedType::Object(obj) => {
            for selection in selections {
                match selection {
                    Selection::Field(selection) => {
                        let field = obj.fields.get(&selection.name).ok_or_else(|| {
                            MissingField(selection.name.to_string(), type_name.to_string())
                        })?;

                        let field_type_name = field.ty.inner_named_type();

                        mutations.push(Change::Field {
                            type_name: type_name.clone(),
                            field_name: selection.name.clone(),
                            graph_name: Arc::clone(&graph_name),
                        });

                        let field_type = schema
                            .types
                            .get(field_type_name)
                            .ok_or_else(|| MissingType(field_type_name.to_string()))?;

                        if field_type.is_enum() {
                            mutations.extend(enum_values_for_graph(
                                field_type,
                                origin_graph,
                                Arc::clone(&graph_name),
                            ));
                        }

                        if !selection.selection_set.is_empty() {
                            mutations.extend(recurse_selection(
                                origin_graph,
                                Arc::clone(&graph_name),
                                schema,
                                field_type_name,
                                field_type,
                                None,
                                &selection.selection_set,
                            )?);
                        }
                    }
                    Selection::FragmentSpread(_) => todo!(),
                    Selection::InlineFragment(_) => todo!(),
                }
            }
        }
        ExtendedType::Interface(obj) => {
            let implementors_map = schema.implementers_map();
            let possible_types = implementors_map
                .get(type_name)
                .ok_or(MissingPossibleTypes(type_name.to_string()))?
                .iter()
                .flat_map(|name| schema.types.get(name))
                .filter(|ty| {
                    ty.directives().iter().any(|d| {
                        d.name == "join__type"
                            && d.argument_by_name("graph")
                                .and_then(|a| a.as_enum())
                                .map(|e| *e == origin_graph)
                                .unwrap_or(false)
                    })
                })
                .sorted_by_key(|ty| ty.name().to_string())
                .collect::<Vec<_>>();

            for selection in selections {
                match selection {
                    Selection::Field(selection) => {
                        if let Some(field) = obj.fields.get(&selection.name) {
                            let field_type_name = field.ty.inner_named_type();

                            mutations.push(Change::Field {
                                type_name: type_name.clone(),
                                field_name: selection.name.clone(),
                                graph_name: Arc::clone(&graph_name),
                            });

                            if !selection.selection_set.is_empty() {
                                let field_type = schema
                                    .types
                                    .get(field_type_name)
                                    .ok_or(MissingType(field_type_name.to_string()))?;

                                mutations.extend(recurse_selection(
                                    origin_graph,
                                    Arc::clone(&graph_name),
                                    schema,
                                    field_type_name,
                                    field_type,
                                    None,
                                    &selection.selection_set,
                                )?);
                            }
                        }

                        for possible_type in possible_types.iter() {
                            match possible_type {
                                ExtendedType::Object(obj) => {
                                    if let Some(field) = obj.fields.get(&selection.name) {
                                        {
                                            mutations.push(Change::Type {
                                                name: possible_type.name().clone(),
                                                graph_name: Arc::clone(&graph_name),

                                                key: parent_entity_interface_key.clone(),
                                                is_interface_object: false,
                                                implements: Some(type_name.clone()),
                                            });

                                            let field_type_name = field.ty.inner_named_type();

                                            mutations.push(Change::Field {
                                                type_name: possible_type.name().clone(),
                                                field_name: selection.name.clone(),
                                                graph_name: Arc::clone(&graph_name),
                                            });

                                            let field_type = schema
                                                .types
                                                .get(field_type_name)
                                                .ok_or(MissingType(field_type_name.to_string()))?;

                                            if field_type.is_enum() {
                                                mutations.extend(enum_values_for_graph(
                                                    field_type,
                                                    origin_graph,
                                                    Arc::clone(&graph_name),
                                                ));
                                            }

                                            if !selection.selection_set.is_empty() {
                                                mutations.extend(recurse_selection(
                                                    origin_graph,
                                                    Arc::clone(&graph_name),
                                                    schema,
                                                    field_type_name,
                                                    field_type,
                                                    None,
                                                    &selection.selection_set,
                                                )?);
                                            }
                                        }
                                    }
                                }
                                ExtendedType::Interface(_) => {
                                    return Err(Unsupported("interfaces on interfaces".into()))
                                }
                                _ => {}
                            }
                        }
                    }

                    Selection::FragmentSpread(_) => {
                        return Err(Unsupported(
                            "fragment spreads in connector selections".into(),
                        ))
                    }
                    Selection::InlineFragment(_) => {
                        return Err(Unsupported(
                            "inline fragments in connector selections".into(),
                        ))
                    }
                }
            }
        }
        ExtendedType::Union(un) => {
            let member_types = un
                .directives
                .iter()
                .filter_map(|d| {
                    if d.name == "join__unionMember"
                        && d.argument_by_name("graph")
                            .and_then(|a| a.as_enum())
                            .map(|e| *e == origin_graph)
                            .unwrap_or(false)
                    {
                        d.argument_by_name("member")
                            .and_then(|a| a.as_str())
                            .and_then(|name| schema.types.get(name))
                    } else {
                        None
                    }
                })
                .collect::<Vec<_>>();

            for selection in selections {
                match selection {
                    Selection::Field(selection) => {
                        for member_type in member_types.iter() {
                            if let ExtendedType::Object(obj) = member_type {
                                if let Some(field) = obj.fields.get(&selection.name) {
                                    mutations.push(Change::UnionMember {
                                        union_name: un.name.clone(),
                                        member_name: obj.name.clone(),
                                        graph_name: Arc::clone(&graph_name),
                                    });

                                    mutations.push(Change::Type {
                                        name: member_type.name().clone(),
                                        graph_name: Arc::clone(&graph_name),
                                        key: None,
                                        is_interface_object: false,
                                        implements: None,
                                    });

                                    let field_type_name = field.ty.inner_named_type();

                                    mutations.push(Change::Field {
                                        type_name: member_type.name().clone(),
                                        field_name: selection.name.clone(),
                                        graph_name: Arc::clone(&graph_name),
                                    });

                                    let field_type = schema
                                        .types
                                        .get(field_type_name)
                                        .ok_or(MissingType(field_type_name.to_string()))?;

                                    if field_type.is_enum() {
                                        mutations.extend(enum_values_for_graph(
                                            field_type,
                                            origin_graph,
                                            Arc::clone(&graph_name),
                                        ));
                                    }

                                    if !selection.selection_set.is_empty() {
                                        mutations.extend(recurse_selection(
                                            origin_graph,
                                            Arc::clone(&graph_name),
                                            schema,
                                            field_type_name,
                                            field_type,
                                            None,
                                            &selection.selection_set,
                                        )?);
                                    }
                                }
                            }
                        }
                    }
                    Selection::FragmentSpread(_) => {
                        return Err(Unsupported(
                            "fragment spreads in connector selections".into(),
                        ))
                    }
                    Selection::InlineFragment(_) => {
                        return Err(Unsupported(
                            "inline fragments in connector selections".into(),
                        ))
                    }
                }
            }
        }
        ExtendedType::InputObject(_) => {
            return Err(InvalidSelection("input object in selection".into()))
        }
        _ => {} // hit a scalar and we're done
    }

    Ok(mutations)
}

fn recurse_inputs(
    graph_name: Arc<String>,
    schema: &Schema,
    input_value_def: &Node<InputValueDefinition>,
) -> Result<Vec<Change>, ConnectorSupergraphError> {
    let mut changes = Vec::new();

    let output_type_name = input_value_def.ty.inner_named_type();

    let ty = schema
        .types
        .get(output_type_name.as_str())
        .ok_or_else(|| MissingType(output_type_name.to_string()))?;

    if !ty.is_built_in() {
        changes.push(Change::Type {
            name: output_type_name.clone(),
            graph_name: Arc::clone(&graph_name),
            key: None,
            is_interface_object: false,
            implements: None,
        });
    }

    match ty {
        ExtendedType::InputObject(obj) => {
            for field in obj.fields.values() {
                changes.push(Change::InputField {
                    type_name: output_type_name.clone(),
                    field_name: field.name.clone(),
                    graph_name: Arc::clone(&graph_name),
                });
                changes.extend(recurse_inputs(
                    Arc::clone(&graph_name),
                    schema,
                    &field.node,
                )?);
            }
        }
        ExtendedType::Enum(enm) => {
            for value in enm.values.values() {
                changes.push(Change::EnumValue {
                    enum_name: ty.name().clone(),
                    value_name: value.value.clone(),
                    graph_name: Arc::clone(&graph_name),
                });
            }
        }
        _ => {}
    }

    Ok(changes)
}

#[derive(Debug, thiserror::Error, displaydoc::Display)]
pub(crate) enum ConnectorSupergraphError {
    /// Invalid outer supergraph: {0}
    InvalidOuterSupergraph(String),

    /// Missing field {0} on type {1}
    MissingField(String, String),

    /// Missing type {0}
    MissingType(String),

    /// Missing possible types for {0}
    MissingPossibleTypes(String),

    /// Unsupported: {0}
    Unsupported(String),

    /// Invalid connector selection: {0}
    InvalidSelection(String),

    /// Invariant failed: {0}
    Invariant(String),

    /// Invalid GraphQL name
    InvalidName(#[from] apollo_compiler::ast::InvalidNameError),

    /// Invalid inner supergraph: {0}
    InvalidInnerSupergraph(apollo_compiler::validation::WithErrors<apollo_compiler::Schema>),
}
use ConnectorSupergraphError::*;

impl PartialEq for ConnectorSupergraphError {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::InvalidOuterSupergraph(l0), Self::InvalidOuterSupergraph(r0)) => l0 == r0,
            (Self::MissingField(l0, l1), Self::MissingField(r0, r1)) => l0 == r0 && l1 == r1,
            (Self::MissingType(l0), Self::MissingType(r0)) => l0 == r0,
            (Self::MissingPossibleTypes(l0), Self::MissingPossibleTypes(r0)) => l0 == r0,
            (Self::Unsupported(l0), Self::Unsupported(r0)) => l0 == r0,
            (Self::InvalidSelection(l0), Self::InvalidSelection(r0)) => l0 == r0,
            (Self::Invariant(l0), Self::Invariant(r0)) => l0 == r0,
            (Self::InvalidName(l0), Self::InvalidName(r0)) => l0 == r0,
            // WithErrors doesn't implement PartialEq
            (Self::InvalidInnerSupergraph(l0), Self::InvalidInnerSupergraph(r0)) => {
                l0.to_string() == r0.to_string()
            }
            _ => false,
        }
    }
}

/// Given an enum definition, find all the values that are associated with the origin
/// subgraph. Return a list of enum value inclusions for the connector subgraph.
fn enum_values_for_graph(
    ty: &ExtendedType,
    origin_graph: &str,
    graph_name: Arc<String>,
) -> Vec<Change> {
    let mut results = Vec::new();

    if let ExtendedType::Enum(enm) = ty {
        for value in enm.values.values() {
            let has_join = value.directives.iter().any(|d| {
                d.name == "join__enumValue"
                    && d.argument_by_name("graph")
                        .and_then(|a| a.as_enum())
                        .map(|e| *e == origin_graph)
                        .unwrap_or(false)
            });
            if has_join {
                results.push(Change::EnumValue {
                    enum_name: ty.name().clone(),
                    value_name: value.value.clone(),
                    graph_name: Arc::clone(&graph_name),
                });
            }
        }
    }

    results
}

#[cfg(test)]
mod tests {
    use apollo_compiler::Schema;
    use insta::assert_snapshot;

    use crate::plugins::connectors::Source;
    use crate::spec::Schema as RouterSchema;

    const SCHEMA: &str = include_str!("./testdata/test_supergraph.graphql");

    #[tokio::test]
    async fn it_works() {
        let schema = Schema::parse_and_validate(SCHEMA, "outer.graphql").unwrap();

        let source = Source::new(&schema).unwrap().unwrap();
        let inner = source.supergraph();

        // new supergraph can be parsed into subgraphs
        let _ = RouterSchema::parse(inner.serialize().to_string().as_str(), &Default::default())
            .unwrap();

        assert_snapshot!(inner.serialize().to_string());
    }
}
