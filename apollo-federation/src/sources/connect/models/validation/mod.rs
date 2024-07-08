//! Validation of the `@source` and `@connect` directives.

// No panics allowed in this module
#![cfg_attr(
    not(test),
    deny(
        clippy::exit,
        clippy::panic,
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::unimplemented,
        clippy::todo
    )
)]

/* TODO:

## @connect

- http:
  - URL path template valid syntax
  - body is valid syntax
- selection
  - Valid syntax
- If entity: true
  - Field arguments must match fields in type (and @key if present)
  - Required for types with @key (eventually with @finder)

## HTTPHeaderMapping

- name: is unique

## Output selection

- Empty composite selection
- Leaf with selections

## Input selections (URL path templates and bodies)

- Missing $args
- Missing $this

*/
mod coordinates;
mod entity;
mod http_headers;
mod http_method;
mod http_url;
mod selection;
mod source_name;

use std::collections::HashMap;
use std::ops::Range;

use apollo_compiler::ast::FieldDefinition;
use apollo_compiler::ast::Value;
use apollo_compiler::schema::Component;
use apollo_compiler::schema::Directive;
use apollo_compiler::schema::ExtendedType;
use apollo_compiler::schema::ObjectType;
use apollo_compiler::Name;
use apollo_compiler::Node;
use apollo_compiler::NodeLocation;
use apollo_compiler::Schema;
use apollo_compiler::SourceMap;
use coordinates::connect_directive_coordinate;
use coordinates::connect_directive_http_coordinate;
use coordinates::connect_directive_url_coordinate;
use coordinates::source_base_url_argument_coordinate;
use coordinates::source_http_argument_coordinate;
use entity::validate_entity_arg;
use http_headers::validate_headers_arg;
use http_method::validate_http_method;
use itertools::Itertools;
use selection::validate_selection;
use source_name::validate_source_name_arg;
use source_name::SourceName;
use url::Url;

use crate::link::Import;
use crate::link::Link;
use crate::sources::connect::spec::schema::CONNECT_HEADERS_ARGUMENT_NAME;
use crate::sources::connect::spec::schema::CONNECT_HTTP_ARGUMENT_DELETE_METHOD_NAME;
use crate::sources::connect::spec::schema::CONNECT_HTTP_ARGUMENT_GET_METHOD_NAME;
use crate::sources::connect::spec::schema::CONNECT_HTTP_ARGUMENT_NAME;
use crate::sources::connect::spec::schema::CONNECT_HTTP_ARGUMENT_PATCH_METHOD_NAME;
use crate::sources::connect::spec::schema::CONNECT_HTTP_ARGUMENT_POST_METHOD_NAME;
use crate::sources::connect::spec::schema::CONNECT_HTTP_ARGUMENT_PUT_METHOD_NAME;
use crate::sources::connect::spec::schema::CONNECT_SOURCE_ARGUMENT_NAME;
use crate::sources::connect::spec::schema::SOURCE_BASE_URL_ARGUMENT_NAME;
use crate::sources::connect::spec::schema::SOURCE_DIRECTIVE_NAME_IN_SPEC;
use crate::sources::connect::spec::schema::SOURCE_HEADERS_ARGUMENT_NAME;
use crate::sources::connect::spec::schema::SOURCE_HTTP_ARGUMENT_NAME;
use crate::sources::connect::spec::schema::SOURCE_NAME_ARGUMENT_NAME;
use crate::sources::connect::ConnectSpecDefinition;
use crate::subgraph::database::federation_link_identity;
use crate::subgraph::spec::CONTEXT_DIRECTIVE_NAME;
use crate::subgraph::spec::FROM_CONTEXT_DIRECTIVE_NAME;
use crate::subgraph::spec::INTF_OBJECT_DIRECTIVE_NAME;

/// Validate the connectors-related directives `@source` and `@connect`.
///
/// This function attempts to collect as many validation errors as possible, so it does not bail
/// out as soon as it encounters one.
pub fn validate(schema: Schema) -> Vec<Message> {
    let connect_identity = ConnectSpecDefinition::identity();
    let Some((link, link_directive)) = Link::for_identity(&schema, &connect_identity) else {
        return Vec::new(); // There are no connectors-related directives to validate
    };

    let mut messages = check_conflicting_directives(&schema);

    let source_map = &schema.sources;
    let source_directive_name = ConnectSpecDefinition::source_directive_name(&link);
    let connect_directive_name = ConnectSpecDefinition::connect_directive_name(&link);
    let source_directives: Vec<SourceDirective> = schema
        .schema_definition
        .directives
        .iter()
        .filter(|directive| directive.name == source_directive_name)
        .map(|directive| validate_source(directive, source_map))
        .collect();

    let mut valid_source_names = HashMap::new();
    let all_source_names = source_directives
        .iter()
        .map(|directive| directive.name.clone())
        .collect_vec();
    for directive in source_directives {
        messages.extend(directive.errors);
        match directive.name.into_value_or_error(source_map) {
            Err(error) => messages.push(error),
            Ok(name) => valid_source_names
                .entry(name)
                .or_insert_with(Vec::new)
                .extend(Location::from_node(
                    directive.directive.node.location(),
                    source_map,
                )),
        }
    }
    for (name, locations) in valid_source_names {
        if locations.len() > 1 {
            messages.push(Message {
                message: format!("Every `@{source_directive_name}({SOURCE_NAME_ARGUMENT_NAME}:)` must be unique. Found duplicate name {name}."),
                code: Code::DuplicateSourceName,
                locations,
            });
        }
    }
    let connect_errors = schema
        .types
        .values()
        .filter_map(|extended_type| match extended_type {
            ExtendedType::Object(object) => Some(object),
            _ => None,
        })
        .flat_map(|object| {
            validate_object_fields(
                object,
                &schema,
                &connect_directive_name,
                &source_directive_name,
                &all_source_names,
            )
        });
    messages.extend(connect_errors);
    if source_directive_name == DEFAULT_SOURCE_DIRECTIVE_NAME
        && messages
            .iter()
            .any(|error| error.code == Code::NoSourcesDefined)
    {
        messages.push(Message {
            code: Code::NoSourceImport,
            message: format!("The `@{SOURCE_DIRECTIVE_NAME_IN_SPEC}` directive is not imported. Try adding `@{SOURCE_DIRECTIVE_NAME_IN_SPEC}` to `import` for `@{link_name}(url: \"{connect_identity}\")`", link_name=link_directive.name),
            locations: Location::from_node(link_directive.location(), source_map)
                .into_iter()
                .collect(),
        });
    }
    messages
}

fn check_conflicting_directives(schema: &Schema) -> Vec<Message> {
    let Some((fed_link, fed_link_directive)) =
        Link::for_identity(schema, &federation_link_identity())
    else {
        return Vec::new();
    };

    // TODO: make the `Link` code retain locations directly instead of reparsing stuff for validation
    let imports = fed_link_directive
        .argument_by_name(&name!("import"))
        .and_then(|arg| arg.as_list())
        .into_iter()
        .flatten()
        .filter_map(|value| Import::from_value(value).ok().map(|import| (value, import)))
        .collect_vec();

    let disallowed_imports = [
        INTF_OBJECT_DIRECTIVE_NAME,
        CONTEXT_DIRECTIVE_NAME,
        FROM_CONTEXT_DIRECTIVE_NAME,
    ];
    fed_link
        .imports
        .into_iter()
        .filter_map(|import| {
            disallowed_imports
                .contains(&import.element)
                .then(|| Message {
                    code: Code::UnsupportedFederationDirective,
                    message: format!(
                        "The directive `@{import}` is not supported when using connectors.",
                        import = import.alias.as_ref().unwrap_or(&import.element)
                    ),
                    locations: imports
                        .iter()
                        .find_map(|(value, reparsed)| {
                            (*reparsed == *import)
                                .then(|| Location::from_node(value.location(), &schema.sources))
                        })
                        .flatten()
                        .into_iter()
                        .collect(),
                })
        })
        .collect()
}

const DEFAULT_SOURCE_DIRECTIVE_NAME: &str = "connect__source";
const DEFAULT_CONNECT_DIRECTIVE_NAME: &str = "connect__connect";

fn validate_source(directive: &Component<Directive>, sources: &SourceMap) -> SourceDirective {
    let name = SourceName::from_directive(directive);
    let mut errors = Vec::new();

    if let Some(http_arg) = directive
        .argument_by_name(&SOURCE_HTTP_ARGUMENT_NAME)
        .and_then(|arg| arg.as_object())
    {
        // Validate URL argument
        if let Some(url_value) = http_arg
            .iter()
            .find_map(|(key, value)| (key == &SOURCE_BASE_URL_ARGUMENT_NAME).then_some(value))
        {
            if let Some(url_error) = parse_url(
                url_value,
                &source_base_url_argument_coordinate(&directive.name),
                sources,
            )
            .err()
            {
                errors.push(url_error);
            }
        }

        // Validate headers argument
        if let Some(headers) = http_arg
            .iter()
            .find_map(|(key, value)| (key == &SOURCE_HEADERS_ARGUMENT_NAME).then_some(value))
        {
            let header_errors = validate_headers_arg(
                &directive.name,
                &format!(
                    "{}.{}",
                    SOURCE_HTTP_ARGUMENT_NAME, SOURCE_HEADERS_ARGUMENT_NAME
                ),
                headers,
                sources,
                None,
                None,
            );

            if !header_errors.is_empty() {
                errors.extend(header_errors);
            }
        }
    } else {
        errors.push(Message {
            code: Code::GraphQLError,
            message: format!(
                "{coordinate} must have a `{SOURCE_HTTP_ARGUMENT_NAME}` argument.",
                coordinate = source_http_argument_coordinate(&directive.name),
            ),
            locations: Location::from_node(directive.location(), sources)
                .into_iter()
                .collect(),
        })
    }

    SourceDirective {
        name,
        errors,
        directive: directive.clone(),
    }
}

/// A `@source` directive along with any errors related to it.
struct SourceDirective {
    name: SourceName,
    errors: Vec<Message>,
    directive: Component<Directive>,
}

/// Make sure that any `@connect` directives on object fields are valid, and that all fields
/// are resolvable by some combination of `@connect` directives.
fn validate_object_fields(
    object: &Node<ObjectType>,
    schema: &Schema,
    connect_directive_name: &Name,
    source_directive_name: &Name,
    source_names: &[SourceName],
) -> Vec<Message> {
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
            locations: Location::from_node(object.location(), source_map)
                .into_iter()
                .collect(),
        }];
    }
    let object_category = if schema
        .schema_definition
        .query
        .as_ref()
        .is_some_and(|query| query.name == object.name)
    {
        ObjectCategory::Query
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
            )
        })
        .collect()
}

fn validate_field(
    field: &Component<FieldDefinition>,
    category: ObjectCategory,
    source_names: &[SourceName],
    object: &Node<ObjectType>,
    connect_directive_name: &Name,
    source_directive_name: &Name,
    schema: &Schema,
) -> Vec<Message> {
    let source_map = &schema.sources;
    let mut errors = Vec::new();
    let Some(connect_directive) = field
        .directives
        .iter()
        .find(|directive| directive.name == *connect_directive_name)
    else {
        if category == ObjectCategory::Query {
            errors.push(Message {
                code: Code::QueryFieldMissingConnect,
                message: format!(
                    "The field `{object_name}.{field}` has no `@{connect_directive_name}` directive.",
                    field = field.name,
                    object_name = object.name,
                ),
                locations: Location::from_node(field.location(), source_map)
                    .into_iter()
                    .collect(),
            });
        }
        return errors;
    };

    errors.extend(validate_selection(field, connect_directive, object, schema));

    errors.extend(validate_entity_arg(
        field,
        connect_directive,
        object,
        schema,
        source_map,
        category,
    ));

    let Some((http_arg, http_arg_location)) = connect_directive
        .argument_by_name(&CONNECT_HTTP_ARGUMENT_NAME)
        .and_then(|arg| Some((arg.as_object()?, arg.location())))
    else {
        errors.push(Message {
            code: Code::GraphQLError,
            message: format!(
                "{coordinate} must have a `{CONNECT_HTTP_ARGUMENT_NAME}` argument.",
                coordinate =
                    connect_directive_coordinate(connect_directive_name, object, &field.name),
            ),
            locations: Location::from_node(connect_directive.location(), source_map)
                .into_iter()
                .collect(),
        });
        return errors;
    };

    let http_methods: Vec<_> = http_arg
        .iter()
        .filter(|(method, _)| {
            [
                CONNECT_HTTP_ARGUMENT_GET_METHOD_NAME,
                CONNECT_HTTP_ARGUMENT_POST_METHOD_NAME,
                CONNECT_HTTP_ARGUMENT_PUT_METHOD_NAME,
                CONNECT_HTTP_ARGUMENT_PATCH_METHOD_NAME,
                CONNECT_HTTP_ARGUMENT_DELETE_METHOD_NAME,
            ]
            .contains(method)
        })
        .collect();

    errors.extend(validate_http_method(
        &http_methods,
        connect_directive_http_coordinate(connect_directive_name, object, &field.name),
        http_arg_location,
        source_map,
    ));

    let http_arg_url = http_methods.first().map(|(http_method, url)| {
        (
            url,
            connect_directive_url_coordinate(
                connect_directive_name,
                http_method,
                object,
                &field.name,
            ),
        )
    });

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

        if let Some((url, url_coordinate)) = http_arg_url {
            if parse_url(url, &url_coordinate, source_map).is_ok() {
                errors.push(Message {
                    code: Code::AbsoluteConnectUrlWithSource,
                    message: format!(
                        "{url_coordinate} contains the absolute URL {url} while also specifying a `{CONNECT_SOURCE_ARGUMENT_NAME}`. Either remove the `{CONNECT_SOURCE_ARGUMENT_NAME}` argument or change the URL to a path.",
                    ),
                    locations: Location::from_node(url.location(), source_map)
                        .into_iter()
                        .collect(),
                });
            }
        }
    } else if let Some((url, url_coordinate)) = http_arg_url {
        if let Some(err) = parse_url(url, &url_coordinate, source_map).err() {
            // Attempt to detect if they were using a relative path without a source, no way to be perfect with this
            if url
                .as_str()
                .is_some_and(|url| url.starts_with('/') || url.ends_with('/'))
            {
                errors.push(Message {
                    code: Code::RelativeConnectUrlWithoutSource,
                    message: format!(
                        "{url_coordinate} specifies the relative URL {url}, but no `{CONNECT_SOURCE_ARGUMENT_NAME}` is defined. Either use an absolute URL, or add a `@{source_directive_name}`."),
                    locations: Location::from_node(url.location(), source_map).into_iter().collect()
                });
            } else {
                errors.push(err);
            }
        }
    }

    if let Some(headers) = http_arg
        .iter()
        .find(|(key, _)| key == &CONNECT_HEADERS_ARGUMENT_NAME)
        .map(|(_, value)| value)
    {
        errors.extend(validate_headers_arg(
            connect_directive_name,
            &format!("{CONNECT_HTTP_ARGUMENT_NAME}.{CONNECT_HEADERS_ARGUMENT_NAME}"),
            headers,
            source_map,
            Some(&object.name),
            Some(&field.name),
        ));
    }

    errors
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ObjectCategory {
    Query,
    Other,
}

fn parse_url(value: &Node<Value>, coordinate: &str, sources: &SourceMap) -> Result<Url, Message> {
    let str_value = require_value_is_str(value, coordinate, sources)?;
    let url = Url::parse(str_value).map_err(|inner| Message {
        code: Code::InvalidUrl,
        message: format!("The value {value} for {coordinate} is not a valid URL: {inner}.",),
        locations: Location::from_node(value.location(), sources)
            .into_iter()
            .collect(),
    })?;
    let scheme = url.scheme();
    if scheme != "http" && scheme != "https" {
        return Err(Message {
            code: Code::SourceScheme,
            message: format!(
                "The value {value} for {coordinate} must be http or https, got {scheme}.",
            ),
            locations: Location::from_node(value.location(), sources)
                .into_iter()
                .collect(),
        });
    }
    Ok(url)
}

fn require_value_is_str<'a>(
    value: &'a Node<Value>,
    coordinate: &str,
    sources: &SourceMap,
) -> Result<&'a str, Message> {
    value.as_str().ok_or_else(|| Message {
        code: Code::GraphQLError,
        message: format!("The value for {coordinate} must be a string.",),
        locations: Location::from_node(value.location(), sources)
            .into_iter()
            .collect(),
    })
}

type DirectiveName = Name;

#[derive(Debug)]
pub struct Message {
    /// A unique, per-error code to allow consuming tools to take specific actions. These codes
    /// should not change once stabilized.
    pub code: Code,
    /// A human-readable message describing the error. These messages are not stable, tools should
    /// not rely on them remaining the same.
    ///
    /// # Formatting messages
    /// 1. Messages should be complete sentences, starting with capitalization as appropriate and
    /// ending with punctuation.
    /// 2. When referring to elements of the schema, use
    /// [schema coordinates](https://github.com/graphql/graphql-wg/blob/main/rfcs/SchemaCoordinates.md)
    /// with any additional information added as required for clarity (e.g., the value of an arg).
    /// 3. When referring to code elements (including schema coordinates), surround them with
    /// backticks. This clarifies that `Type.field` is not ending a sentence with its period.
    pub message: String,
    pub locations: Vec<Range<Location>>,
}

/// A 0-indexed line/column reference to SDL source
#[derive(Clone, Copy, Debug)]
pub struct Location {
    pub line: usize,
    pub column: usize,
}

impl Location {
    // TODO: This is a ripoff of GraphQLLocation::from_node in apollo_compiler, contribute it back
    fn from_node(node: Option<NodeLocation>, sources: &SourceMap) -> Option<Range<Self>> {
        let node = node?;
        let source = sources.get(&node.file_id())?;
        let start = source
            .get_line_column(node.offset())
            .map(|(line, column)| Location { line, column })?;
        let end = source
            .get_line_column(node.end_offset())
            .map(|(line, column)| Location { line, column })?;
        Some(Range { start, end })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Code {
    /// A problem with GraphQL syntax or semantics was found. These will usually be caught before
    /// this validation process.
    GraphQLError,
    DuplicateSourceName,
    InvalidSourceName,
    EmptySourceName,
    InvalidUrl,
    SourceScheme,
    SourceNameMismatch,
    SubscriptionInConnectors,
    QueryFieldMissingConnect,
    /// The `@connect` is using a `source`, but the URL is absolute. This is trouble because
    /// the `@source` URL will be joined with the `@connect` URL, so the `@connect` URL should
    /// actually be a path only.
    AbsoluteConnectUrlWithSource,
    /// The `@connect` directive is using a relative URL (path only) but does not define a `source`.
    /// This is just a specialization of [`Self::InvalidUrl`] that provides a better suggestion for
    /// the user.
    RelativeConnectUrlWithoutSource,
    /// This is a specialization of [`Self::SourceNameMismatch`] that provides a better suggestion.
    NoSourcesDefined,
    /// The subgraph doesn't import the `@source` directive. This isn't necessarily a problem, but
    /// is likely a mistake.
    NoSourceImport,
    /// The `@connect` directive has multiple HTTP methods when only one is allowed.
    MultipleHttpMethods,
    /// The `@connect` directive is missing an HTTP method.
    MissingHttpMethod,
    /// The `entity` argument should only be used on the root `Query` field.
    EntityNotOnRootQuery,
    /// The `entity` argument should only be used with non-list, object types.
    EntityTypeInvalid,
    /// A syntax error in `selection`
    InvalidJsonSelection,
    /// A cycle was detected within a `selection`
    CircularReference,
    /// A field was selected but is not defined on the type
    SelectedFieldNotFound,
    /// A group selection (`a { b }`) was used, but the field is not an object
    GroupSelectionIsNotObject,
    /// Invalid header name
    /// The `name` and `as` mappings should be valid, HTTP header names.
    InvalidHttpHeaderName,
    /// The `value` mapping should be either a valid HTTP header value or list of valid HTTP header values.
    InvalidHttpHeaderValue,
    /// The `name` mapping must be unique for all headers.
    HttpHeaderNameCollision,
    /// Header mappings cannot include both `as` and `value` properties.
    InvalidHttpHeaderMapping,
    /// Certain directives are not allowed when using connectors
    UnsupportedFederationDirective,
}

impl Code {
    pub const fn severity(&self) -> Severity {
        match self {
            Self::NoSourceImport => Severity::Warning,
            _ => Severity::Error,
        }
    }
}

/// Given the [`Code`] of a [`Message`], how important is that message?
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Severity {
    /// This is an error, validation as failed.
    Error,
    /// The user probably wants to know about this, but it doesn't halt composition.
    Warning,
}

#[cfg(test)]
mod test_validate_source {
    use std::fs::read_to_string;

    use insta::assert_snapshot;
    use insta::glob;

    use super::*;

    #[test]
    fn validation_tests() {
        insta::with_settings!({prepend_module_to_snapshot => false}, {
            glob!("test_data", "validation/*.graphql", |path| {
                let schema = read_to_string(path).unwrap();
                let schema = Schema::parse(schema, "test.graphql").unwrap();
                let errors = validate(schema);
                assert_snapshot!(format!("{:?}", errors));
            });
        });
    }
}
