use apollo_compiler::ast::Argument;
use apollo_compiler::ast::FieldDefinition;
use apollo_compiler::ast::InputValueDefinition;
use apollo_compiler::ast::Value;
use apollo_compiler::parser::SourceMap;
use apollo_compiler::schema::Component;
use apollo_compiler::schema::Directive;
use apollo_compiler::schema::ExtendedType;
use apollo_compiler::schema::InputObjectType;
use apollo_compiler::schema::ObjectType;
use apollo_compiler::Name;
use apollo_compiler::Node;
use apollo_compiler::Schema;

use super::coordinates::connect_directive_entity_argument_coordinate;
use super::extended_type::ObjectCategory;
use super::Code;
use super::Message;
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
                        "{coordinate} is invalid. Entities can only be non-list, nullable, object types.",
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
                            "{coordinate} is missing entity resolver arguments.",
                            coordinate = connect_directive_entity_argument_coordinate(
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
                    if let Some(message) = (ArgumentVisitor {
                        schema,
                        connect_directive_name,
                        entity_arg,
                        entity_arg_value,
                        object,
                        source_map,
                        field: &field.name,
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

#[derive(Clone, Copy, Debug)]
enum Group<'schema> {
    Root {
        field: &'schema Node<FieldDefinition>,
        entity_type: &'schema Node<ObjectType>,
    },
    Child {
        input_type: &'schema Node<InputObjectType>,
        entity_type: &'schema ExtendedType,
    },
}

#[derive(Clone, Copy, Debug)]
struct Field<'schema> {
    node: &'schema Node<InputValueDefinition>,
    input_type: &'schema ExtendedType,
    entity_type: &'schema ExtendedType,
}

/// Visitor for entity resolver arguments.
struct ArgumentVisitor<'schema> {
    schema: &'schema Schema,
    connect_directive_name: &'schema Name,
    entity_arg: &'schema Node<Argument>,
    entity_arg_value: &'schema Node<Value>,
    object: &'schema Node<ObjectType>,
    source_map: &'schema SourceMap,
    field: &'schema Name,
}

impl<'schema> GroupVisitor<Group<'schema>, Field<'schema>> for ArgumentVisitor<'schema> {
    fn try_get_group_for_field(
        &self,
        field: &Field<'schema>,
    ) -> Result<Option<Group<'schema>>, Self::Error> {
        Ok(
            if let ExtendedType::InputObject(input_object_type) = field.input_type {
                Some(Group::Child {
                    input_type: input_object_type,
                    entity_type: field.entity_type,
                })
            } else {
                None
            },
        )
    }

    fn enter_group(&mut self, group: &Group<'schema>) -> Result<Vec<Field<'schema>>, Self::Error> {
        match group {
            Group::Root { field, entity_type, .. } => field.arguments.iter().filter_map(|arg| {
                if let Some(input_type) = self.schema.types.get(arg.ty.inner_named_type()) {
                    if let Some(entity_type) = entity_type.fields.get(&*arg.name)
                        .and_then(|entity_field| self.schema.types.get(entity_field.ty.inner_named_type())) {
                        Some(Ok(Field {
                            node: arg,
                            input_type,
                            entity_type,
                        }))
                    } else {
                        Some(Err(Message {
                            code: Code::EntityResolverArgumentMismatch,
                            message: format!(
                                "{coordinate} has invalid entity resolver arguments. Argument `{arg_name}` does not exist as a field on entity type `{entity_type}`.",
                                coordinate = connect_directive_entity_argument_coordinate(
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
            }).collect(),
            Group::Child { input_type, entity_type, .. } => {
                if let ExtendedType::Object(entity_object_type) = entity_type {
                    input_type.fields.iter().filter_map(|(name, input_field)| {
                        if let Some(entity_field) = entity_object_type.fields.get(name) {
                            let entity_field_type = entity_field.ty.inner_named_type();
                            if let Some(input_type) = self.schema.types.get(input_field.ty.inner_named_type()) {
                                self.schema.types.get(entity_field_type).map(|entity_type| Ok(Field {
                                        node: input_field,
                                        input_type,
                                        entity_type,
                                    }))
                            } else {
                                // The input type is missing - this will be reported elsewhere, so just ignore
                                None
                            }
                        } else {
                            Some(Err(Message {
                                code: Code::EntityResolverArgumentMismatch,
                                message: format!(
                                    "{coordinate} has invalid entity resolver arguments. Field `{name}` on `{input_type}` does not exist on `{entity_type}`.",
                                    coordinate = connect_directive_entity_argument_coordinate(
                                        self.connect_directive_name,
                                        self.entity_arg_value.as_ref(),
                                        self.object,
                                        self.field,
                                    ),
                                    input_type = input_type.name,
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
            },
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
                    "{coordinate} has invalid entity resolver arguments. Mismatched type on field `{field_name}` - expected `{entity_type}` but found `{input_type}`.",
                    coordinate = connect_directive_entity_argument_coordinate(
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
