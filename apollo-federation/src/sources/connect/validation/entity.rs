use apollo_compiler::ast::Argument;
use apollo_compiler::ast::FieldDefinition;
use apollo_compiler::ast::InputValueDefinition;
use apollo_compiler::ast::Value;
use apollo_compiler::schema::Component;
use apollo_compiler::schema::Directive;
use apollo_compiler::schema::ExtendedType;
use apollo_compiler::schema::InputObjectType;
use apollo_compiler::schema::ObjectType;
use apollo_compiler::Name;
use apollo_compiler::Node;

use super::coordinates::connect_directive_entity_argument_coordinate;
use super::coordinates::field_with_connect_directive_entity_true_coordinate;
use super::extended_type::ObjectCategory;
use super::Code;
use super::Message;
use crate::sources::connect::expand::visitors::FieldVisitor;
use crate::sources::connect::expand::visitors::GroupVisitor;
use crate::sources::connect::spec::schema::CONNECT_ENTITY_ARGUMENT_NAME;
use crate::sources::connect::validation::graphql::SchemaInfo;

/// Applies additional validations to `@connect` if `entity` is `true`.
///
/// TODO: use the same code as expansion to generate the automatic key. Return that here so that
/// the upper level can confirm all explicit `@key`s match an automatic key
pub(super) fn validate_entity_arg(
    field: &Component<FieldDefinition>,
    connect_directive: &Node<Directive>,
    object: &Node<ObjectType>,
    schema: &SchemaInfo,
    category: ObjectCategory,
) -> Result<(), Message> {
    let connect_directive_name = &connect_directive.name;

    let Some(entity_arg) = connect_directive
        .arguments
        .iter()
        .find(|arg| arg.name == CONNECT_ENTITY_ARGUMENT_NAME)
    else {
        return Ok(());
    };
    let entity_arg_value = &entity_arg.value;
    if !entity_arg_value.to_bool().unwrap_or_default() {
        // This is not an entity resolver
        return Ok(());
    }

    if category != ObjectCategory::Query {
        return Err(
            Message {
                code: Code::EntityNotOnRootQuery,
                message: format!(
                    "{coordinate} is invalid. Entity resolvers can only be declared on root `Query` fields.",
                    coordinate = connect_directive_entity_argument_coordinate(connect_directive_name, entity_arg_value.as_ref(), object, &field.name)
                ),
                locations: entity_arg.line_column_range(&schema.sources)
                    .into_iter()
                    .collect(),
            }
        );
    }

    let Some(object_type) = schema.get_object(field.ty.inner_named_type()) else {
        return Err(Message {
            code: Code::EntityTypeInvalid,
            message: format!(
                "{coordinate} is invalid. Entity connectors must return object types.",
                coordinate = connect_directive_entity_argument_coordinate(
                    connect_directive_name,
                    entity_arg_value.as_ref(),
                    object,
                    &field.name
                )
            ),
            locations: entity_arg
                .line_column_range(&schema.sources)
                .into_iter()
                .collect(),
        });
    };

    if field.ty.is_list() || field.ty.is_non_null() {
        return Err(
            Message {
                code: Code::EntityTypeInvalid,
                message: format!(
                    "{coordinate} is invalid. Entity connectors must return non-list, nullable, object types. See https://go.apollo.dev/connectors/directives/#rules-for-entity-true",
                    coordinate = connect_directive_entity_argument_coordinate(
                        connect_directive_name,
                        entity_arg_value.as_ref(),
                        object,
                        &field.name
                    )
                ),
                locations: entity_arg
                    .line_column_range(&schema.sources)
                    .into_iter()
                    .collect(),
            }
        );
    }

    if field.arguments.is_empty() {
        return Err(Message {
            code: Code::EntityResolverArgumentMismatch,
            message: format!(
                "{coordinate} must have arguments. See https://go.apollo.dev/connectors/directives/#rules-for-entity-true",
                coordinate = field_with_connect_directive_entity_true_coordinate(
                    connect_directive_name,
                    entity_arg_value.as_ref(),
                    object,
                    &field.name,
                ),
            ),
            locations: entity_arg
                .line_column_range(&schema.sources)
                .into_iter()
                .collect(),
        });
    }

    ArgumentVisitor {
        schema,
        entity_arg,
        entity_arg_value,
        object,
        field: &field.name,
    }
    .walk(Group::Root {
        field,
        entity_type: object_type,
    })
}

#[derive(Clone, Debug)]
enum Group<'schema> {
    /// The entity itself, we're matching argument names & types to these fields
    Root {
        field: &'schema Node<FieldDefinition>,
        entity_type: &'schema Node<ObjectType>,
    },
    /// A child field of the entity we're matching against an input type.
    Child {
        input_type: &'schema Node<InputObjectType>,
        entity_type: &'schema ExtendedType,
        root_entity_type: &'schema Name,
    },
}

#[derive(Clone, Debug)]
struct Field<'schema> {
    node: &'schema Node<InputValueDefinition>,
    input_type: &'schema ExtendedType,
    entity_type: &'schema ExtendedType,
    root_entity_type: &'schema Name,
}

/// Visitor for entity resolver arguments.
/// This validates that the arguments match fields on the entity type.
///
/// Since input types may contain fields with subtypes, and the fields of those subtypes can be
/// part of composite keys, this potentially requires visiting a tree.
struct ArgumentVisitor<'schema> {
    schema: &'schema SchemaInfo<'schema>,
    entity_arg: &'schema Node<Argument>,
    entity_arg_value: &'schema Node<Value>,
    object: &'schema Node<ObjectType>,
    field: &'schema Name,
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
                root_entity_type,
                ..
            } => self.enter_child_group(input_type, entity_type, root_entity_type),
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
                        self.schema.connect_directive_name,
                        self.entity_arg_value.as_ref(),
                        self.object,
                        self.field,
                    ),
                    field_name = field.node.name.as_str(),
                    input_type = field.input_type.name(),
                    entity_type = field.entity_type.name(),
                ),
                locations: field.node
                    .line_column_range(&self.schema.sources)
                    .into_iter()
                    .chain(self.entity_arg.line_column_range(&self.schema.sources))
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
                // Check that the argument has a corresponding field on the entity type
                let root_entity_type = &entity_type.name;
                if let Some(entity_type) = entity_type.fields.get(&*arg.name)
                    .and_then(|entity_field| self.schema.types.get(entity_field.ty.inner_named_type())) {
                    Some(Ok(Field {
                        node: arg,
                        input_type,
                        entity_type,
                        root_entity_type,
                    }))
                } else {
                    Some(Err(Message {
                        code: Code::EntityResolverArgumentMismatch,
                        message: format!(
                            "{coordinate} has invalid arguments. Argument `{arg_name}` does not have a matching field `{arg_name}` on type `{entity_type}`.",
                            coordinate = field_with_connect_directive_entity_true_coordinate(
                                self.schema.connect_directive_name,
                                self.entity_arg_value.as_ref(),
                                self.object,
                                &field.name
                            ),
                            arg_name = &*arg.name,
                            entity_type = entity_type.name,
                        ),
                        locations: arg
                            .line_column_range(&self.schema.sources)
                            .into_iter()
                            .chain(self.entity_arg.line_column_range(&self.schema.sources))
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
        root_entity_type: &'schema Name,
    ) -> Result<
        Vec<Field<'schema>>,
        <ArgumentVisitor<'schema> as FieldVisitor<Field<'schema>>>::Error,
    > {
        // At the child level, visit each field on the input type
        let ExtendedType::Object(entity_object_type) = entity_type else {
            // Entity type was not an object type - this will be reported by field visitor
            return Ok(Vec::new());
        };
        child_input_type.fields.iter().filter_map(|(name, input_field)| {
            if let Some(entity_field) = entity_object_type.fields.get(name) {
                let entity_field_type = entity_field.ty.inner_named_type();
                let input_type = self.schema.types.get(input_field.ty.inner_named_type())?;

                self.schema.types.get(entity_field_type).map(|entity_type| Ok(Field {
                    node: input_field,
                    input_type,
                    entity_type,
                    root_entity_type,
                }))
            } else {
                // The input type field does not have a corresponding field on the entity type
                Some(Err(Message {
                    code: Code::EntityResolverArgumentMismatch,
                    message: format!(
                        "{coordinate} has invalid arguments. Field `{name}` on `{input_type}` does not have a matching field `{name}` on `{entity_type}`.",
                        coordinate = field_with_connect_directive_entity_true_coordinate(
                            self.schema.connect_directive_name,
                            self.entity_arg_value.as_ref(),
                            self.object,
                            self.field,
                        ),
                        input_type = child_input_type.name,
                        entity_type = entity_object_type.name,
                    ),
                    locations: input_field
                        .line_column_range(&self.schema.sources)
                        .into_iter()
                        .chain(self.entity_arg.line_column_range(&self.schema.sources))
                        .collect(),
                }))
            }
        }).collect()
    }
}
