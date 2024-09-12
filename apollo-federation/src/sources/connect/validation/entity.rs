use apollo_compiler::ast::Argument;
use apollo_compiler::ast::FieldDefinition;
use apollo_compiler::ast::InputValueDefinition;
use apollo_compiler::ast::Value;
use apollo_compiler::executable::FieldSet;
use apollo_compiler::executable::Selection;
use apollo_compiler::parser::Parser;
use apollo_compiler::parser::SourceMap;
use apollo_compiler::schema::Component;
use apollo_compiler::schema::Directive;
use apollo_compiler::schema::ExtendedType;
use apollo_compiler::schema::InputObjectType;
use apollo_compiler::schema::ObjectType;
use apollo_compiler::validation::Valid;
use apollo_compiler::Name;
use apollo_compiler::Node;
use apollo_compiler::Schema;

use super::coordinates::connect_directive_entity_argument_coordinate;
use super::coordinates::field_with_connect_directive_entity_true_coordinate;
use super::extended_type::ObjectCategory;
use super::Code;
use super::Message;
use crate::link::federation_spec_definition::FEDERATION_FIELDS_ARGUMENT_NAME;
use crate::link::federation_spec_definition::FEDERATION_KEY_DIRECTIVE_NAME_IN_SPEC;
use crate::link::federation_spec_definition::FEDERATION_RESOLVABLE_ARGUMENT_NAME;
use crate::sources::connect::expand::visitors::FieldVisitor;
use crate::sources::connect::expand::visitors::GroupVisitor;
use crate::sources::connect::spec::schema::CONNECT_ENTITY_ARGUMENT_NAME;

pub(super) fn validate_entity_arg(
    field: &Component<FieldDefinition>,
    connect_directive: &Node<Directive>,
    object: &Node<ObjectType>,
    schema: &Schema,
    source_map: &SourceMap,
    category: ObjectCategory,
) -> Vec<Message> {
    let mut messages = vec![];
    let connect_directive_name = &connect_directive.name;

    if let Some(entity_arg) = connect_directive
        .arguments
        .iter()
        .find(|arg| arg.name == CONNECT_ENTITY_ARGUMENT_NAME)
    {
        let entity_arg_value = &entity_arg.value;
        if entity_arg_value
            .to_bool()
            .is_some_and(|entity_arg_value| entity_arg_value)
        {
            if category != ObjectCategory::Query {
                messages.push(Message {
                    code: Code::EntityNotOnRootQuery,
                    message: format!(
                        "{coordinate} is invalid. Entity resolvers can only be declared on root `Query` fields.",
                        coordinate = connect_directive_entity_argument_coordinate(connect_directive_name, entity_arg_value.as_ref(), object, &field.name)
                    ),
                    locations: entity_arg.line_column_range(source_map)
                        .into_iter()
                        .collect(),
                })
                // TODO: Allow interfaces
            } else if field.ty.is_list()
                || schema.get_object(field.ty.inner_named_type()).is_none()
                || field.ty.is_non_null()
            {
                messages.push(Message {
                    code: Code::EntityTypeInvalid,
                    message: format!(
                        "{coordinate} is invalid. Entity connectors must return non-list, nullable, object types.",
                        coordinate = connect_directive_entity_argument_coordinate(
                            connect_directive_name,
                            entity_arg_value.as_ref(),
                            object,
                            &field.name
                        )
                    ),
                    locations: entity_arg
                        .line_column_range(source_map)
                        .into_iter()
                        .collect(),
                })
            }

            // Validate the arguments to the entity resolver (but if the field was determined to be
            // invalid for an entity resolver above, don't bother validating the field arguments).
            if messages.is_empty() {
                if field.arguments.is_empty() {
                    messages.push(Message {
                        code: Code::EntityResolverArgumentMismatch,
                        message: format!(
                            "{coordinate} must have arguments. See https://preview-docs.apollographql.com/graphos/connectors/directives/#rules-for-entity-true",
                            coordinate = field_with_connect_directive_entity_true_coordinate(
                                connect_directive_name,
                                entity_arg_value.as_ref(),
                                object,
                                &field.name,
                            ),
                        ),
                        locations: entity_arg
                            .line_column_range(source_map)
                            .into_iter()
                            .collect(),
                    });
                } else if let Some(object_type) = schema.get_object(field.ty.inner_named_type()) {
                    let key_fields = object_type
                        .directives
                        .iter()
                        .filter(|directive| directive.name == FEDERATION_KEY_DIRECTIVE_NAME_IN_SPEC)
                        .filter(|directive| {
                            directive
                                .arguments
                                .iter()
                                .find(|arg| arg.name == FEDERATION_RESOLVABLE_ARGUMENT_NAME)
                                .and_then(|arg| arg.value.to_bool())
                                .unwrap_or(true)
                        })
                        .filter_map(|directive| {
                            directive
                                .arguments
                                .iter()
                                .find(|arg| arg.name == FEDERATION_FIELDS_ARGUMENT_NAME)
                        })
                        .map(|fields| &*fields.value)
                        .filter_map(|key_fields| key_fields.as_str())
                        .filter_map(|fields| {
                            Parser::new()
                                .parse_field_set(
                                    Valid::assume_valid_ref(schema),
                                    object_type.name.clone(),
                                    fields.to_string(),
                                    "",
                                )
                                .ok()
                        })
                        .collect();

                    if let Some(message) = (ArgumentVisitor {
                        schema,
                        connect_directive_name,
                        entity_arg,
                        entity_arg_value,
                        object,
                        source_map,
                        field: &field.name,
                        key_fields,
                    })
                    .walk(Group::Root {
                        field,
                        entity_type: object_type,
                    })
                    .err()
                    {
                        messages.push(message);
                    }
                };
            }
        }
    }

    messages
}

#[derive(Clone, Debug)]
enum Group<'schema> {
    Root {
        field: &'schema Node<FieldDefinition>,
        entity_type: &'schema Node<ObjectType>,
    },
    Child {
        input_type: &'schema Node<InputObjectType>,
        entity_type: &'schema ExtendedType,
        key_selection: Vec<Selection>,
        root_entity_type: &'schema Name,
    },
}

#[derive(Clone, Debug)]
struct Field<'schema> {
    node: &'schema Node<InputValueDefinition>,
    input_type: &'schema ExtendedType,
    entity_type: &'schema ExtendedType,
    key_selection: Vec<Selection>,
    root_entity_type: &'schema Name,
}

/// Visitor for entity resolver arguments. This validates that three thing match:
///
/// * The input type fields
/// * The entity type fields
/// * The entity type key fields from the `@key` directive, if present
///
/// Since input types may contain fields with subtypes, and the fields of those subtypes can be
/// part of composite keys, this potentially requires visiting a tree.
struct ArgumentVisitor<'schema> {
    schema: &'schema Schema,
    connect_directive_name: &'schema Name,
    entity_arg: &'schema Node<Argument>,
    entity_arg_value: &'schema Node<Value>,
    object: &'schema Node<ObjectType>,
    source_map: &'schema SourceMap,
    field: &'schema Name,
    key_fields: Vec<FieldSet>,
}

impl<'schema> GroupVisitor<Group<'schema>, Field<'schema>> for ArgumentVisitor<'schema> {
    fn try_get_group_for_field(
        &self,
        field: &Field<'schema>,
    ) -> Result<Option<Group<'schema>>, Self::Error> {
        Ok(
            // Each input type within an argument to the entity field is another group to visit
            if let ExtendedType::InputObject(input_object_type) = field.input_type {
                Some(Group::Child {
                    input_type: input_object_type,
                    entity_type: field.entity_type,
                    key_selection: field.key_selection.clone(),
                    root_entity_type: field.root_entity_type,
                })
            } else {
                None
            },
        )
    }

    fn enter_group(&mut self, group: &Group<'schema>) -> Result<Vec<Field<'schema>>, Self::Error> {
        match group {
            Group::Root {
                field, entity_type, ..
            } => self.enter_root_group(field, entity_type),
            Group::Child {
                input_type,
                entity_type,
                key_selection,
                root_entity_type,
                ..
            } => self.enter_child_group(input_type, entity_type, key_selection, root_entity_type),
        }
    }

    fn exit_group(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }
}

impl<'schema> FieldVisitor<Field<'schema>> for ArgumentVisitor<'schema> {
    type Error = Message;

    fn visit(&mut self, field: Field<'schema>) -> Result<(), Self::Error> {
        let ok = match field.input_type {
            ExtendedType::InputObject(_) => field.entity_type.is_object(),
            ExtendedType::Scalar(_) | ExtendedType::Enum(_) => {
                field.input_type == field.entity_type
            }
            _ => true,
        };
        if ok {
            Ok(())
        } else {
            Err(Message {
                code: Code::EntityResolverArgumentMismatch,
                message: format!(
                    "{coordinate} has invalid arguments. Mismatched type on field `{field_name}` - expected `{entity_type}` but found `{input_type}`.",
                    coordinate = field_with_connect_directive_entity_true_coordinate(
                        self.connect_directive_name,
                        self.entity_arg_value.as_ref(),
                        self.object,
                        self.field,
                    ),
                    field_name = field.node.name.as_str(),
                    input_type = field.input_type.name(),
                    entity_type = field.entity_type.name(),
                ),
                locations: field.node
                    .line_column_range(self.source_map)
                    .into_iter()
                    .chain(self.entity_arg.line_column_range(self.source_map))
                    .collect(),
            })
        }
    }
}

impl<'schema> ArgumentVisitor<'schema> {
    fn enter_root_group(
        &mut self,
        field: &'schema Node<FieldDefinition>,
        entity_type: &'schema Node<ObjectType>,
    ) -> Result<
        Vec<Field<'schema>>,
        <ArgumentVisitor<'schema> as FieldVisitor<Field<'schema>>>::Error,
    > {
        // At the root level, visit each argument to the entity field
        field.arguments.iter().filter_map(|arg| {
            if let Some(input_type) = self.schema.types.get(arg.ty.inner_named_type()) {

                // If the entity type has one or more `@key` directives, check that the argument
                // corresponds to a field in the keys
                let key_fields: Vec<&Vec<Selection>> = self.key_fields.iter()
                    .map(|fields| &fields.selection_set.selections)
                    .collect();
                let key_selection = if key_fields.is_empty() {
                    vec![]
                } else {
                    match self.find_key(key_fields, &arg.name, arg, None, &entity_type.name) {
                        Ok(selection) => selection,
                        Err(message) => return Some(Err(message)),
                    }
                };

                // Check that the argument has a corresponding field on the entity type
                let root_entity_type = &entity_type.name;
                if let Some(entity_type) = entity_type.fields.get(&*arg.name)
                    .and_then(|entity_field| self.schema.types.get(entity_field.ty.inner_named_type())) {
                    Some(Ok(Field {
                        node: arg,
                        input_type,
                        entity_type,
                        root_entity_type,
                        key_selection,
                    }))
                } else {
                    Some(Err(Message {
                        code: Code::EntityResolverArgumentMismatch,
                        message: format!(
                            "{coordinate} has invalid arguments. Argument `{arg_name}` does not have a matching field `{arg_name}` on type `{entity_type}`.",
                            coordinate = field_with_connect_directive_entity_true_coordinate(
                                self.connect_directive_name,
                                self.entity_arg_value.as_ref(),
                                self.object,
                                &field.name
                            ),
                            arg_name = &*arg.name,
                            entity_type = entity_type.name,
                        ),
                        locations: arg
                            .line_column_range(self.source_map)
                            .into_iter()
                            .chain(self.entity_arg.line_column_range(self.source_map))
                            .collect(),
                    }))
                }
            } else {
                // The input type is missing - this will be reported elsewhere, so just ignore
                None
            }
        }).collect()
    }

    fn enter_child_group(
        &mut self,
        child_input_type: &'schema Node<InputObjectType>,
        entity_type: &'schema ExtendedType,
        key_selections: &[Selection],
        root_entity_type: &'schema Name,
    ) -> Result<
        Vec<Field<'schema>>,
        <ArgumentVisitor<'schema> as FieldVisitor<Field<'schema>>>::Error,
    > {
        // At the child level, visit each field on the input type
        if let ExtendedType::Object(entity_object_type) = entity_type {
            child_input_type.fields.iter().filter_map(|(name, input_field)| {
                if let Some(entity_field) = entity_object_type.fields.get(name) {
                    let entity_field_type = entity_field.ty.inner_named_type();
                    if let Some(input_type) = self.schema.types.get(input_field.ty.inner_named_type()) {

                        // Check that the field on the input type corresponds to a key field
                        let selections: Vec<&Vec<Selection>> = key_selections
                            .iter()
                            .filter_map(|key_selection| key_selection.as_field())
                            .map(|field| &field.selection_set.selections)
                            .collect();
                        let key_selection = if selections.is_empty() {
                            vec![]
                        } else {
                            match self.find_key(selections, name, input_field, Some(&child_input_type.name), root_entity_type) {
                                Ok(selections) => selections,
                                Err(message) => return Some(Err(message)),
                            }
                        };

                        self.schema.types.get(entity_field_type).map(|entity_type| Ok(Field {
                            node: input_field,
                            input_type,
                            entity_type,
                            root_entity_type,
                            key_selection,
                        }))
                    } else {
                        // The input type is missing - this will be reported elsewhere, so just ignore
                        None
                    }
                } else {
                    // The input type field does not have a corresponding field on the entity type
                    Some(Err(Message {
                        code: Code::EntityResolverArgumentMismatch,
                        message: format!(
                            "{coordinate} has invalid arguments. Field `{name}` on `{input_type}` does not have a matching field `{name}` on `{entity_type}`.",
                            coordinate = field_with_connect_directive_entity_true_coordinate(
                                self.connect_directive_name,
                                self.entity_arg_value.as_ref(),
                                self.object,
                                self.field,
                            ),
                            input_type = child_input_type.name,
                            entity_type = entity_object_type.name,
                        ),
                        locations: input_field
                            .line_column_range(self.source_map)
                            .into_iter()
                            .chain(self.entity_arg.line_column_range(self.source_map))
                            .collect(),
                    }))
                }
            }).collect()
        } else {
            // Entity type was not an object type - this will be reported by field visitor
            Ok(vec![])
        }
    }

    fn find_key(
        &self,
        selections: Vec<&Vec<Selection>>,
        name: &str,
        node: &Node<InputValueDefinition>,
        input_type: Option<&Name>,
        entity_type: &Name,
    ) -> Result<Vec<Selection>, <ArgumentVisitor<'schema> as FieldVisitor<Field<'schema>>>::Error>
    {
        let matches: Vec<Selection> = selections
            .into_iter()
            .filter_map(|selections| {
                selections
                    .iter()
                    .find(|selection| {
                        selection
                            .as_field()
                            .map(|field| field.name == *name)
                            .unwrap_or(false)
                    })
                    .cloned()
            })
            .collect();
        if matches.is_empty() {
            Err(Message {
                code: Code::EntityResolverArgumentMismatch,
                message: format!(
                    "{coordinate} has invalid arguments. {name} does not match a field in a `@key` on type `{entity_type}`.",
                    name = match input_type {
                        Some(input_type) => format!("Field `{name}` on input type `{input_type}`"),
                        None => format!("Argument `{name}`"),
                    },
                    coordinate = field_with_connect_directive_entity_true_coordinate(
                        self.connect_directive_name,
                        self.entity_arg_value.as_ref(),
                        self.object,
                        self.field,
                    ),
                ),
                locations: node
                    .line_column_range(self.source_map)
                    .into_iter()
                    .chain(self.entity_arg.line_column_range(self.source_map))
                    .collect(),
            })
        } else {
            Ok(matches)
        }
    }
}
