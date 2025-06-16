//! Validations for `@connect` on types/the `@connect(entity:)` argument.

use std::fmt;
use std::fmt::Display;

use apollo_compiler::Node;
use apollo_compiler::ast::Argument;
use apollo_compiler::ast::FieldDefinition;
use apollo_compiler::ast::InputValueDefinition;
use apollo_compiler::schema::ExtendedType;
use apollo_compiler::schema::InputObjectType;
use apollo_compiler::schema::ObjectType;

use super::Code;
use super::Message;
use super::ObjectCategory;
use crate::connectors::expand::visitors::FieldVisitor;
use crate::connectors::expand::visitors::GroupVisitor;
use crate::connectors::id::ConnectedElement;
use crate::connectors::spec::connect::CONNECT_ENTITY_ARGUMENT_NAME;
use crate::connectors::validation::coordinates::ConnectDirectiveCoordinate;
use crate::connectors::validation::graphql::SchemaInfo;

/// Applies additional validations to `@connect` if `entity` is `true`.
pub(super) fn validate_entity_arg(
    connect: ConnectDirectiveCoordinate,
    schema: &SchemaInfo,
) -> Result<(), Message> {
    let Some(entity_arg) = connect
        .directive
        .arguments
        .iter()
        .find(|arg| arg.name == CONNECT_ENTITY_ARGUMENT_NAME)
    else {
        return Ok(());
    };

    let entity_arg_value = &entity_arg.value;
    let Some(value) = entity_arg_value.to_bool() else {
        return Ok(()); // The default value is always okay
    };

    let coordinate = Coordinate { connect, value };

    let (field, category) = match (connect.element, value) {
        (ConnectedElement::Field { .. }, false) | (ConnectedElement::Type { .. }, true) => {
            // Explicit values set to the default are always okay
            return Ok(());
        }
        (ConnectedElement::Type { .. }, false) => {
            // `@connect` on a type is _always_ an entity resolver, so this is an error
            return Err(Message {
                code: Code::ConnectOnTypeMustBeEntity,
                message: format!(
                    "{coordinate} is invalid. `entity` can't be false for connectors on types."
                ),
                locations: entity_arg
                    .line_column_range(&schema.sources)
                    .into_iter()
                    .collect(),
            });
        }
        (
            // For `entity: true` on fields, we have additional checks we now need to run
            ConnectedElement::Field {
                field_def,
                parent_category,
                ..
            },
            true,
        ) => (field_def, parent_category),
    };

    if category != ObjectCategory::Query {
        return Err(Message {
            code: Code::EntityNotOnRootQuery,
            message: format!(
                "{coordinate} is invalid. Entity resolvers can only be declared on root `Query` fields.",
            ),
            locations: entity_arg
                .line_column_range(&schema.sources)
                .into_iter()
                .collect(),
        });
    }

    let Some(object_type) = schema.get_object(field.ty.inner_named_type()) else {
        return Err(Message {
            code: Code::EntityTypeInvalid,
            message: format!(
                "{coordinate} is invalid. Entity connectors must return object types.",
            ),
            locations: entity_arg
                .line_column_range(&schema.sources)
                .into_iter()
                .collect(),
        });
    };

    if field.ty.is_list() || field.ty.is_non_null() {
        return Err(Message {
            code: Code::EntityTypeInvalid,
            message: format!(
                "{coordinate} is invalid. Entity connectors must return non-list, nullable, object types. See https://go.apollo.dev/connectors/entity-rules",
            ),
            locations: entity_arg
                .line_column_range(&schema.sources)
                .into_iter()
                .collect(),
        });
    }

    if field.arguments.is_empty() {
        return Err(Message {
            code: Code::EntityResolverArgumentMismatch,
            message: format!(
                "`{coordinate}` must have arguments when using `entity: true`. See https://go.apollo.dev/connectors/entity-rules",
                coordinate = coordinate.connect.element,
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
        coordinate,
    }
    .walk(Group::Root {
        field,
        entity_type: object_type,
    })
    .map(|_| ())
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
    },
}

#[derive(Clone, Debug)]
struct Field<'schema> {
    node: &'schema Node<InputValueDefinition>,
    /// The object which has a field that we're comparing against
    object_type: &'schema ObjectType,
    /// The field definition of the input that correlates to a field on the entity
    input_field: &'schema ExtendedType,
    /// The field of the entity that we're comparing against, part of `object_type`
    entity_field: &'schema ExtendedType,
}

/// Visitor for entity resolver arguments.
/// This validates that the arguments match fields on the entity type.
///
/// Since input types may contain fields with subtypes, and the fields of those subtypes can be
/// part of composite keys, this potentially requires visiting a tree.
struct ArgumentVisitor<'schema> {
    schema: &'schema SchemaInfo<'schema>,
    entity_arg: &'schema Node<Argument>,
    coordinate: Coordinate<'schema>,
}

impl<'schema> GroupVisitor<Group<'schema>, Field<'schema>> for ArgumentVisitor<'schema> {
    fn try_get_group_for_field(
        &self,
        field: &Field<'schema>,
    ) -> Result<Option<Group<'schema>>, Self::Error> {
        Ok(
            // Each input type within an argument to the entity field is another group to visit
            if let ExtendedType::InputObject(input_object_type) = field.input_field {
                Some(Group::Child {
                    input_type: input_object_type,
                    entity_type: field.entity_field,
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
                ..
            } => self.enter_child_group(input_type, entity_type),
        }
    }

    fn exit_group(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }
}

impl<'schema> FieldVisitor<Field<'schema>> for ArgumentVisitor<'schema> {
    type Error = Message;

    fn visit(&mut self, field: Field<'schema>) -> Result<(), Self::Error> {
        let ok = match field.input_field {
            ExtendedType::InputObject(_) => field.entity_field.is_object(),
            ExtendedType::Scalar(_) | ExtendedType::Enum(_) => {
                field.input_field == field.entity_field
            }
            _ => true,
        };
        if ok {
            Ok(())
        } else {
            Err(Message {
                code: Code::EntityResolverArgumentMismatch,
                message: format!(
                    "`{coordinate}({field_name}:)` is of type `{input_type}`, but must match `{object}.{field_name}` of type `{entity_type}` because `entity` is `true`.",
                    coordinate = self.coordinate.connect.element,
                    field_name = field.node.name.as_str(),
                    object = field.object_type.name,
                    input_type = field.input_field.name(),
                    entity_type = field.entity_field.name(),
                ),
                locations: field
                    .node
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
    ) -> Result<Vec<Field<'schema>>, <Self as FieldVisitor<Field<'schema>>>::Error> {
        // At the root level, visit each argument to the entity field
        field.arguments.iter().filter_map(|arg| {
            if let Some(input_type) = self.schema.types.get(arg.ty.inner_named_type()) {
                // Check that the argument has a corresponding field on the entity type
                if let Some(entity_field) = entity_type.fields.get(&*arg.name)
                    .and_then(|entity_field| self.schema.types.get(entity_field.ty.inner_named_type())) {
                    Some(Ok(Field {
                        node: arg,
                        input_field: input_type,
                        entity_field,
                        object_type: entity_type,
                    }))
                } else {
                    Some(Err(Message {
                        code: Code::EntityResolverArgumentMismatch,
                        message: format!(
                            "`{coordinate}` has invalid arguments. Argument `{arg_name}` does not have a matching field `{arg_name}` on type `{entity_type}`.",
                            coordinate = self.coordinate.connect.element,
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
    ) -> Result<Vec<Field<'schema>>, <Self as FieldVisitor<Field<'schema>>>::Error> {
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
                    object_type: entity_object_type,
                    input_field: input_type,
                    entity_field: entity_type,
                }))
            } else {
                // The input type field does not have a corresponding field on the entity type
                Some(Err(Message {
                    code: Code::EntityResolverArgumentMismatch,
                    message: format!(
                        "`{coordinate}` has invalid arguments. Field `{name}` on `{input_type}` does not have a matching field `{name}` on `{entity_type}`.",
                        coordinate = self.coordinate.connect.element,
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

/// Contains info about a `@connect(entity:)` argument location so it can be displayed in error
/// messages.
#[derive(Clone, Copy)]
struct Coordinate<'schema> {
    connect: ConnectDirectiveCoordinate<'schema>,
    value: bool,
}

impl Display for Coordinate<'_> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let Self {
            connect: ConnectDirectiveCoordinate { directive, element },
            value,
        } = self;
        write!(
            f,
            "`@{connect_directive_name}({CONNECT_ENTITY_ARGUMENT_NAME}: {value})` on `{element}`",
            connect_directive_name = directive.name,
            value = value,
            element = element,
        )
    }
}
