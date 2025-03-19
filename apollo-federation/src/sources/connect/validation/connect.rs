//! Parsing and validation of `@connect` directives

use apollo_compiler::Name;
use apollo_compiler::Node;
use apollo_compiler::ast::Argument;
use apollo_compiler::ast::FieldDefinition;
use apollo_compiler::schema::Component;
use apollo_compiler::schema::ExtendedType;
use apollo_compiler::schema::ObjectType;
use itertools::Itertools;

use self::entity::validate_entity_arg;
use self::selection::get_seen_fields_from_selection;
use self::selection::validate_body_selection;
use super::Code;
use super::Message;
use super::coordinates::ConnectDirectiveCoordinate;
use super::coordinates::ConnectHTTPCoordinate;
use super::coordinates::FieldCoordinate;
use super::coordinates::HttpHeadersCoordinate;
use super::coordinates::connect_directive_name_coordinate;
use super::coordinates::source_name_value_coordinate;
use super::http::headers;
use super::http::method;
use super::source::SourceName;
use crate::sources::connect::spec::schema::CONNECT_BODY_ARGUMENT_NAME;
use crate::sources::connect::spec::schema::CONNECT_SOURCE_ARGUMENT_NAME;
use crate::sources::connect::spec::schema::HTTP_ARGUMENT_NAME;
use crate::sources::connect::validation::graphql::SchemaInfo;

mod entity;
mod selection;

pub(super) fn fields_seen_by_all_connects(
    schema: &SchemaInfo,
    all_source_names: &[SourceName],
) -> Result<Vec<(Name, Name)>, Vec<Message>> {
    let mut messages = Vec::new();
    let mut seen_fields = Vec::new();

    for object in schema
        .types
        .values()
        .filter_map(|extended_type| {
            if let ExtendedType::Object(node) = extended_type {
                Some(node)
            } else {
                None
            }
        })
        .filter(|object| !object.is_built_in())
    {
        match fields_seen_by_object_connectors(object, schema, all_source_names) {
            Ok(fields) => seen_fields.extend(fields),
            Err(errs) => messages.extend(errs),
        }
    }
    if messages.is_empty() {
        Ok(seen_fields)
    } else {
        Err(messages)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ObjectCategory {
    Query,
    Mutation,
    Other,
}

/// Make sure that any `@connect` directives on object fields are valid
fn fields_seen_by_object_connectors(
    object: &Node<ObjectType>,
    schema: &SchemaInfo,
    source_names: &[SourceName],
) -> Result<Vec<(Name, Name)>, Vec<Message>> {
    let object_category = if schema
        .schema_definition
        .query
        .as_ref()
        .is_some_and(|query| query.name == object.name)
    {
        ObjectCategory::Query
    } else if schema
        .schema_definition
        .mutation
        .as_ref()
        .is_some_and(|mutation| mutation.name == object.name)
    {
        ObjectCategory::Mutation
    } else {
        ObjectCategory::Other
    };
    let mut seen_fields = Vec::new();
    let mut messages = Vec::new();
    for field in object.fields.values() {
        match fields_seen_by_connector(field, object_category, source_names, object, schema) {
            Ok(fields) => seen_fields.extend(fields),
            Err(errs) => messages.extend(errs),
        }
    }
    if messages.is_empty() {
        Ok(seen_fields)
    } else {
        Err(messages)
    }
}

fn fields_seen_by_connector(
    field: &Component<FieldDefinition>,
    category: ObjectCategory,
    source_names: &[SourceName],
    object: &Node<ObjectType>,
    schema: &SchemaInfo,
) -> Result<Vec<(Name, Name)>, Vec<Message>> {
    let source_map = &schema.sources;
    let mut errors = Vec::new();
    let connect_directives = field
        .directives
        .iter()
        .filter(|directive| directive.name == *schema.connect_directive_name())
        .collect_vec();

    if connect_directives.is_empty() {
        return Ok(Vec::new());
    }

    // mark the field with a @connect directive as seen
    let mut seen_fields = vec![(object.name.clone(), field.name.clone())];

    // direct recursion isn't allowed, like a connector on User.friends: [User]
    if matches!(category, ObjectCategory::Other) && &object.name == field.ty.inner_named_type() {
        errors.push(Message {
            code: Code::CircularReference,
            message: format!(
                "Direct circular reference detected in `{}.{}: {}`. For more information, see https://go.apollo.dev/connectors/limitations#circular-references",
                object.name,
                field.name,
                field.ty
            ),
            locations: field.line_column_range(source_map).into_iter().collect(),
        });
    }

    for connect_directive in connect_directives {
        let field_coordinate = FieldCoordinate { object, field };
        let connect_coordinate = ConnectDirectiveCoordinate {
            directive: connect_directive,
            field_coordinate,
        };

        match get_seen_fields_from_selection(connect_coordinate, schema) {
            Ok(seen) => seen_fields.extend(seen),
            Err(error) => errors.push(error),
        }

        errors
            .extend(validate_entity_arg(field, connect_directive, object, schema, category).err());

        let Some((http_arg, http_arg_node)) = connect_directive
            .specified_argument_by_name(&HTTP_ARGUMENT_NAME)
            .and_then(|arg| Some((arg.as_object()?, arg)))
        else {
            errors.push(Message {
                code: Code::GraphQLError,
                message: format!(
                    "{connect_coordinate} must have a `{HTTP_ARGUMENT_NAME}` argument."
                ),
                locations: connect_directive
                    .line_column_range(source_map)
                    .into_iter()
                    .collect(),
            });
            return Err(errors);
        };

        let url_template = match method::validate(
            http_arg,
            ConnectHTTPCoordinate::from(connect_coordinate),
            http_arg_node,
            schema,
        ) {
            Ok(method) => Some(method),
            Err(errs) => {
                errors.extend(errs);
                None
            }
        };

        if let Some((_, body)) = http_arg
            .iter()
            .find(|(name, _)| name == &CONNECT_BODY_ARGUMENT_NAME)
        {
            errors.extend(validate_body_selection(
                connect_directive,
                connect_coordinate,
                object,
                field,
                schema,
                body,
            ));
        }

        if let Some(source_name) = connect_directive
            .arguments
            .iter()
            .find(|arg| arg.name == CONNECT_SOURCE_ARGUMENT_NAME)
        {
            errors.extend(validate_source_name_arg(
                &field.name,
                &object.name,
                source_name,
                source_names,
                schema,
            ));

            if let Some((template, coordinate)) = url_template {
                if template.base.is_some() {
                    errors.push(Message {
                        code: Code::AbsoluteConnectUrlWithSource,
                        message: format!(
                            "{coordinate} contains the absolute URL {raw_value} while also specifying a `{CONNECT_SOURCE_ARGUMENT_NAME}`. Either remove the `{CONNECT_SOURCE_ARGUMENT_NAME}` argument or change the URL to a path.",
                            raw_value = coordinate.node
                        ),
                        locations: coordinate.node.line_column_range(source_map)
                            .into_iter()
                            .collect(),
                    })
                }
            }
        } else if let Some((template, coordinate)) = url_template {
            if template.base.is_none() {
                errors.push(Message {
                    code: Code::RelativeConnectUrlWithoutSource,
                    message: format!(
                        "{coordinate} specifies the relative URL {raw_value}, but no `{CONNECT_SOURCE_ARGUMENT_NAME}` is defined. Either use an absolute URL including scheme (e.g. https://), or add a `@{source_directive_name}`.",
                        raw_value = coordinate.node,
                        source_directive_name = schema.source_directive_name(),
                    ),
                    locations: coordinate.node.line_column_range(source_map).into_iter().collect()
                })
            }
        }

        errors.extend(headers::validate_arg(
            http_arg,
            HttpHeadersCoordinate::Connect {
                connect: connect_coordinate,
                object: &object.name,
                field: &field.name,
            },
            schema,
        ));
    }
    if errors.is_empty() {
        Ok(seen_fields)
    } else {
        Err(errors)
    }
}

pub(super) fn validate_source_name_arg(
    field_name: &Name,
    object_name: &Name,
    source_name: &Node<Argument>,
    source_names: &[SourceName],
    schema: &SchemaInfo,
) -> Vec<Message> {
    let mut messages = vec![];

    if source_names.iter().all(|name| name != &source_name.value) {
        // TODO: Pick a suggestion that's not just the first defined source
        let qualified_directive = connect_directive_name_coordinate(
            schema.connect_directive_name(),
            &source_name.value,
            object_name,
            field_name,
        );
        if let Some(first_source_name) = source_names.first() {
            messages.push(Message {
                code: Code::SourceNameMismatch,
                message: format!(
                    "{qualified_directive} does not match any defined sources. Did you mean \"{first_source_name}\"?",
                    first_source_name = first_source_name.as_str(),
                ),
                locations: source_name.line_column_range(&schema.sources)
                    .into_iter()
                    .collect(),
            });
        } else {
            messages.push(Message {
                code: Code::NoSourcesDefined,
                message: format!(
                    "{qualified_directive} specifies a source, but none are defined. Try adding {coordinate} to the schema.",
                    coordinate = source_name_value_coordinate(schema.source_directive_name(), &source_name.value),
                ),
                locations: source_name.line_column_range(&schema.sources)
                    .into_iter()
                    .collect(),
            });
        }
    }

    messages
}
