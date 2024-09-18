use std::sync::Arc;

use apollo_compiler::ast::FieldDefinition;
use apollo_compiler::collections::IndexMap;
use apollo_compiler::collections::IndexSet;
use apollo_compiler::executable::Selection;
use apollo_compiler::parser::FileId;
use apollo_compiler::parser::SourceFile;
use apollo_compiler::parser::SourceMap;
use apollo_compiler::parser::SourceSpan;
use apollo_compiler::schema::Component;
use apollo_compiler::schema::ExtendedType;
use apollo_compiler::schema::ObjectType;
use apollo_compiler::Name;
use apollo_compiler::Node;
use apollo_compiler::Schema;
use itertools::Itertools;

use super::coordinates::ConnectDirectiveCoordinate;
use super::coordinates::ConnectHTTPCoordinate;
use super::coordinates::HttpHeadersCoordinate;
use super::coordinates::HttpMethodCoordinate;
use super::entity::validate_entity_arg;
use super::http::headers;
use super::http::method;
use super::http::url;
use super::resolvable_key_fields;
use super::selection::validate_body_selection;
use super::selection::validate_selection;
use super::source_name::validate_source_name_arg;
use super::source_name::SourceName;
use super::Code;
use super::Message;
use crate::sources::connect::spec::schema::CONNECT_BODY_ARGUMENT_NAME;
use crate::sources::connect::spec::schema::CONNECT_SOURCE_ARGUMENT_NAME;
use crate::sources::connect::spec::schema::HTTP_ARGUMENT_NAME;

pub(super) fn validate_extended_type(
    extended_type: &ExtendedType,
    schema: &Schema,
    connect_directive_name: &Name,
    source_directive_name: &Name,
    all_source_names: &[SourceName],
    source_map: &Arc<IndexMap<FileId, Arc<SourceFile>>>,
    seen_fields: &mut IndexSet<(Name, Name)>,
) -> Vec<Message> {
    match extended_type {
        ExtendedType::Object(object) => validate_object_fields(
            object,
            schema,
            connect_directive_name,
            source_directive_name,
            all_source_names,
            seen_fields,
        ),
        ExtendedType::Union(union_type) => vec![validate_abstract_type(
            SourceSpan::recompose(union_type.location(), union_type.name.location()),
            source_map,
            "union",
        )],
        ExtendedType::Interface(interface) => vec![validate_abstract_type(
            SourceSpan::recompose(interface.location(), interface.name.location()),
            source_map,
            "interface",
        )],
        _ => Vec::new(),
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ObjectCategory {
    Query,
    Mutation,
    Other,
}

/// Make sure that any `@connect` directives on object fields are valid, and that all fields
/// are resolvable by some combination of `@connect` directives.
fn validate_object_fields(
    object: &Node<ObjectType>,
    schema: &Schema,
    connect_directive_name: &Name,
    source_directive_name: &Name,
    source_names: &[SourceName],
    seen_fields: &mut IndexSet<(Name, Name)>,
) -> Vec<Message> {
    if object.is_built_in() {
        return Vec::new();
    }

    // Mark resolvable key fields as seen
    let mut selections: Vec<(Name, Selection)> = resolvable_key_fields(object, schema)
        .flat_map(|field_set| {
            field_set
                .selection_set
                .selections
                .iter()
                .map(|selection| (object.name.clone(), selection.clone()))
                .collect::<Vec<_>>()
        })
        .collect();
    while !selections.is_empty() {
        if let Some((type_name, selection)) = selections.pop() {
            if let Some(field) = selection.as_field() {
                let t = (type_name, field.name.clone());
                if !seen_fields.contains(&t) {
                    seen_fields.insert(t);
                    field.selection_set.selections.iter().for_each(|selection| {
                        selections.push((field.ty().inner_named_type().clone(), selection.clone()));
                    });
                }
            }
        }
    }

    let source_map = &schema.sources;
    let is_subscription = schema
        .schema_definition
        .subscription
        .as_ref()
        .is_some_and(|sub| sub.name == object.name);
    if is_subscription {
        return vec![Message {
            code: Code::SubscriptionInConnectors,
            message: format!(
                "A subscription root type is not supported when using `@{connect_directive_name}`."
            ),
            locations: object.line_column_range(source_map).into_iter().collect(),
        }];
    }

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
    object
        .fields
        .values()
        .flat_map(|field| {
            validate_field(
                field,
                object_category,
                source_names,
                object,
                connect_directive_name,
                source_directive_name,
                schema,
                seen_fields,
            )
        })
        .collect()
}

#[allow(clippy::too_many_arguments)]
fn validate_field(
    field: &Component<FieldDefinition>,
    category: ObjectCategory,
    source_names: &[SourceName],
    object: &Node<ObjectType>,
    connect_directive_name: &Name,
    source_directive_name: &Name,
    schema: &Schema,
    seen_fields: &mut IndexSet<(Name, Name)>,
) -> Vec<Message> {
    let source_map = &schema.sources;
    let mut errors = Vec::new();
    let connect_directives = field
        .directives
        .iter()
        .filter(|directive| directive.name == *connect_directive_name)
        .collect_vec();

    if connect_directives.is_empty() {
        match category {
            ObjectCategory::Query => errors.push(get_missing_connect_directive_message(
                Code::QueryFieldMissingConnect,
                field,
                object,
                source_map,
                connect_directive_name,
            )),
            ObjectCategory::Mutation => errors.push(get_missing_connect_directive_message(
                Code::MutationFieldMissingConnect,
                field,
                object,
                source_map,
                connect_directive_name,
            )),
            _ => (),
        }

        return errors;
    };

    // mark the field with a @connect directive as seen
    seen_fields.insert((object.name.clone(), field.name.clone()));

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
        errors.extend(validate_selection(
            field,
            connect_directive,
            object,
            schema,
            seen_fields,
        ));

        errors.extend(validate_entity_arg(
            field,
            connect_directive,
            object,
            schema,
            source_map,
            category,
        ));

        let coordinate = ConnectDirectiveCoordinate {
            connect_directive_name,
            object_name: &object.name,
            field_name: &field.name,
        };

        let Some((http_arg, http_arg_node)) = connect_directive
            .argument_by_name(&HTTP_ARGUMENT_NAME)
            .and_then(|arg| Some((arg.as_object()?, arg)))
        else {
            errors.push(Message {
                code: Code::GraphQLError,
                message: format!("{coordinate} must have a `{HTTP_ARGUMENT_NAME}` argument."),
                locations: connect_directive
                    .line_column_range(source_map)
                    .into_iter()
                    .collect(),
            });
            return errors;
        };

        let http_method = match method::validate(
            http_arg,
            ConnectHTTPCoordinate::from(coordinate),
            http_arg_node,
            source_map,
        ) {
            Ok(method) => Some(method),
            Err(err) => {
                errors.push(err);
                None
            }
        };

        if let Some((_, body)) = http_arg
            .iter()
            .find(|(name, _)| name == &CONNECT_BODY_ARGUMENT_NAME)
        {
            if let Err(err) =
                validate_body_selection(connect_directive, object, field, schema, body)
            {
                errors.push(err);
            }
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
                source_map,
                source_names,
                source_directive_name,
                &connect_directive.name,
            ));

            if let Some((http_method, url)) = http_method {
                let coordinate = HttpMethodCoordinate {
                    connect: coordinate,
                    http_method,
                };
                if let Err(err) = url::validate_template(url, coordinate, source_map).and_then(|template| {
                if template.base.is_some() {
                    Err(Message {
                        code: Code::AbsoluteConnectUrlWithSource,
                        message: format!(
                            "{coordinate} contains the absolute URL {url} while also specifying a `{CONNECT_SOURCE_ARGUMENT_NAME}`. Either remove the `{CONNECT_SOURCE_ARGUMENT_NAME}` argument or change the URL to a path.",
                        ),
                        locations: url.line_column_range(source_map)
                            .into_iter()
                            .collect(),
                    })
                } else {
                    Ok(())
                }
            }) {
                errors.push(err);
                }
            }
        } else if let Some((http_method, url)) = http_method {
            let coordinate = HttpMethodCoordinate {
                connect: coordinate,
                http_method,
            };
            if let Err(err) = url::validate_template(url, coordinate, source_map).and_then(|template| {
                if template.base.is_none() {
                Err(Message {
                        code: Code::RelativeConnectUrlWithoutSource,
                        message: format!(
                            "{coordinate} specifies the relative URL {url}, but no `{CONNECT_SOURCE_ARGUMENT_NAME}` is defined. Either use an absolute URL including scheme (http://), or add a `@{source_directive_name}`."),
                        locations: url.line_column_range(source_map).into_iter().collect()
                    })
                } else {
                    Ok(())
            }
        }) {
            errors.push(err);
        }
        }

        errors.extend(
            headers::validate_arg(
                http_arg,
                source_map,
                HttpHeadersCoordinate::Connect {
                    directive_name: connect_directive_name,
                    object: &object.name,
                    field: &field.name,
                },
            )
            .into_iter()
            .flatten(),
        );
    }
    errors
}

fn validate_abstract_type(
    node: Option<SourceSpan>,
    source_map: &SourceMap,
    keyword: &str,
) -> Message {
    Message {
        code: Code::UnsupportedAbstractType,
        message: format!("Abstract schema types, such as `{keyword}`, are not supported when using connectors. You can check out our documentation at https://go.apollo.dev/connectors/best-practices#abstract-schema-types-are-unsupported."),
        locations: node.and_then(|location| location.line_column_range(source_map))
            .into_iter()
            .collect(),
    }
}

fn get_missing_connect_directive_message(
    code: Code,
    field: &Component<FieldDefinition>,
    object: &Node<ObjectType>,
    source_map: &SourceMap,
    connect_directive_name: &Name,
) -> Message {
    Message {
        code,
        message: format!(
            "The field `{object_name}.{field}` has no `@{connect_directive_name}` directive.",
            field = field.name,
            object_name = object.name,
        ),
        locations: field.line_column_range(source_map).into_iter().collect(),
    }
}
