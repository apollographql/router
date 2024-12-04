use apollo_compiler::ast::FieldDefinition;
use apollo_compiler::collections::IndexSet;
use apollo_compiler::executable::Selection;
use apollo_compiler::parser::SourceMap;
use apollo_compiler::parser::SourceSpan;
use apollo_compiler::schema::Component;
use apollo_compiler::schema::ExtendedType;
use apollo_compiler::schema::ObjectType;
use apollo_compiler::Name;
use apollo_compiler::Node;
use itertools::Itertools;

use super::coordinates::ConnectDirectiveCoordinate;
use super::coordinates::ConnectHTTPCoordinate;
use super::coordinates::FieldCoordinate;
use super::coordinates::HttpHeadersCoordinate;
use super::entity::make_key_field_set_from_variables;
use super::entity::validate_entity_arg;
use super::http::headers;
use super::http::method;
use super::resolvable_key_fields;
use super::selection::validate_body_selection;
use super::selection::validate_selection;
use super::source_name::validate_source_name_arg;
use super::source_name::SourceName;
use super::Code;
use super::EntityKeyChecker;
use super::Message;
use crate::sources::connect::json_selection::ExternalVarPaths;
use crate::sources::connect::spec::schema::CONNECT_BODY_ARGUMENT_NAME;
use crate::sources::connect::spec::schema::CONNECT_SOURCE_ARGUMENT_NAME;
use crate::sources::connect::spec::schema::HTTP_ARGUMENT_NAME;
use crate::sources::connect::validation::graphql::SchemaInfo;
use crate::sources::connect::variable::VariableReference;
use crate::sources::connect::EntityResolver;
use crate::sources::connect::JSONSelection;
use crate::sources::connect::Namespace;
use crate::sources::connect::PathSelection;

pub(super) fn validate_extended_type<'s>(
    extended_type: &'s ExtendedType,
    schema: &'s SchemaInfo,
    all_source_names: &[SourceName],
    seen_fields: &mut IndexSet<(Name, Name)>,
    entity_checker: &mut EntityKeyChecker<'s>,
) -> Vec<Message> {
    match extended_type {
        ExtendedType::Object(object) => validate_object_fields(
            object,
            schema,
            all_source_names,
            seen_fields,
            entity_checker,
        ),
        ExtendedType::Union(union_type) => vec![validate_abstract_type(
            SourceSpan::recompose(union_type.location(), union_type.name.location()),
            &schema.sources,
            "union",
        )],
        ExtendedType::Interface(interface) => vec![validate_abstract_type(
            SourceSpan::recompose(interface.location(), interface.name.location()),
            &schema.sources,
            "interface",
        )],
        _ => Vec::new(),
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ObjectCategory {
    Query,
    Mutation,
    Other,
}

/// Make sure that any `@connect` directives on object fields are valid, and that all fields
/// are resolvable by some combination of `@connect` directives.
fn validate_object_fields<'s>(
    object: &'s Node<ObjectType>,
    schema: &'s SchemaInfo,
    source_names: &[SourceName],
    seen_fields: &mut IndexSet<(Name, Name)>,
    entity_checker: &mut EntityKeyChecker<'s>,
) -> Vec<Message> {
    if object.is_built_in() {
        return Vec::new();
    }

    let keys_and_directives = resolvable_key_fields(object, schema);

    let mut selections: Vec<(Name, Selection)> = keys_and_directives
        .flat_map(|(field_set, directive)| {
            // Add resolvable keys so we can compare them to entity connectors later
            entity_checker.add_key(&field_set, directive, &object.name);
            field_set
                .selection_set
                .selections
                .iter()
                .map(|selection| (object.name.clone(), selection.clone()))
                .collect::<Vec<_>>()
        })
        .collect();

    while !selections.is_empty() {
        // Mark resolvable key fields as "seen"
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
                "A subscription root type is not supported when using `@{connect_directive_name}`.",
                connect_directive_name = schema.connect_directive_name,
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
                schema,
                seen_fields,
                entity_checker,
            )
        })
        .collect()
}

fn validate_field<'s>(
    field: &'s Component<FieldDefinition>,
    category: ObjectCategory,
    source_names: &[SourceName],
    object: &'s Node<ObjectType>,
    schema: &'s SchemaInfo,
    seen_fields: &mut IndexSet<(Name, Name)>,
    entity_checker: &mut EntityKeyChecker<'s>,
) -> Vec<Message> {
    let source_map = &schema.sources;
    let mut errors = Vec::new();
    let connect_directives = field
        .directives
        .iter()
        .filter(|directive| directive.name == *schema.connect_directive_name)
        .collect_vec();

    if connect_directives.is_empty() {
        match category {
            ObjectCategory::Query => errors.push(get_missing_connect_directive_message(
                Code::QueryFieldMissingConnect,
                field,
                object,
                source_map,
                schema.connect_directive_name,
            )),
            ObjectCategory::Mutation => errors.push(get_missing_connect_directive_message(
                Code::MutationFieldMissingConnect,
                field,
                object,
                source_map,
                schema.connect_directive_name,
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
        let field_coordinate = FieldCoordinate { object, field };
        let connect_coordinate = ConnectDirectiveCoordinate {
            connect_directive_name: schema.connect_directive_name,
            directive: connect_directive,
            field_coordinate,
        };

        let json_selection = match validate_selection(connect_coordinate, schema, seen_fields) {
            Ok(json) => Some(json),
            Err(err) => {
                errors.push(err);
                None
            }
        };

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
            return errors;
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

        let body_selection = if let Some((_, body)) = http_arg
            .iter()
            .find(|(name, _)| name == &CONNECT_BODY_ARGUMENT_NAME)
        {
            match validate_body_selection(
                connect_directive,
                connect_coordinate,
                object,
                field,
                schema,
                body,
            ) {
                Ok(selection) => Some(selection),
                Err(err) => {
                    errors.push(err);
                    None
                }
            }
        } else {
            None
        };

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

            if let Some((template, coordinate)) = url_template.clone() {
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
        } else if let Some((template, coordinate)) = url_template.clone() {
            if template.base.is_none() {
                errors.push(Message {
                    code: Code::RelativeConnectUrlWithoutSource,
                    message: format!(
                        "{coordinate} specifies the relative URL {raw_value}, but no `{CONNECT_SOURCE_ARGUMENT_NAME}` is defined. Either use an absolute URL including scheme (e.g. https://), or add a `@{source_directive_name}`.",
                        raw_value = coordinate.node,
                        source_directive_name = schema.source_directive_name,
                    ),
                    locations: coordinate.node.line_column_range(source_map).into_iter().collect()
                })
            }
        }

        errors.extend(headers::validate_arg(
            http_arg,
            schema,
            HttpHeadersCoordinate::Connect {
                connect: connect_coordinate,
                object: &object.name,
                field: &field.name,
            },
        ));

        let all_variables = json_selection
            .as_ref()
            .map(extract_params_from_selection)
            .into_iter()
            .flatten()
            .chain(
                body_selection
                    .as_ref()
                    .map(extract_params_from_selection)
                    .into_iter()
                    .flatten(),
            )
            .chain(
                url_template
                    .as_ref()
                    .map(|(u, _)| u.variables().cloned())
                    .into_iter()
                    .flatten(),
            )
            .collect_vec();

        errors.extend(
            validate_entity_arg(
                field,
                connect_directive,
                object,
                schema,
                category,
                &all_variables,
                entity_checker,
            )
            .err(),
        );

        if category == ObjectCategory::Other {
            match make_key_field_set_from_variables(
                schema,
                &object.name,
                &all_variables,
                EntityResolver::Implicit,
            ) {
                Ok(Some(field_set)) => {
                    entity_checker.add_connector(&field_set);
                }
                Err(err) => errors.push(err),
                _ => {}
            };
        }
    }
    errors
}

fn validate_abstract_type(
    node: Option<SourceSpan>,
    source_map: &SourceMap,
    keyword: &str,
) -> Message {
    Message {
        code: Code::ConnectorsUnsupportedAbstractType,
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

/// Extract all seen parameters from a JSONSelection
///
/// TODO: this is copied from expand/mod.rs
fn extract_params_from_selection(
    selection: &JSONSelection,
) -> impl Iterator<Item = VariableReference<Namespace>> + '_ {
    selection
        .external_var_paths()
        .into_iter()
        .flat_map(PathSelection::variable_reference)
}
