use apollo_compiler::ast::FieldDefinition;
use apollo_compiler::parser::SourceMap;
use apollo_compiler::schema::Component;
use apollo_compiler::schema::Directive;
use apollo_compiler::schema::ObjectType;
use apollo_compiler::Node;
use apollo_compiler::Schema;

use super::coordinates::connect_directive_entity_argument_coordinate;
use super::extended_type::ObjectCategory;
use super::Code;
use super::Message;
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
            } else if field.ty.is_list() || schema.get_object(field.ty.inner_named_type()).is_none()
            {
                messages.push(Message {
                    code: Code::EntityTypeInvalid,
                    message: format!(
                        "{coordinate} is invalid. Entities can only be non-list, object types.",
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
        }
    }

    messages
}
