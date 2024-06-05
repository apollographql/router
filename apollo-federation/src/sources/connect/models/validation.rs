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
  - Has one path (GET, POST, etc)
  - URL path template valid syntax
  - body is valid syntax
- selection
  - Valid syntax
- If entity: true
  - Must be on Query type
  - Field arguments must match fields in type (and @key if present)
  - Required for types with @key (eventually with @finder)

## HTTPHeaderMapping

- name: is unique
- name: is a valid header name
- as: is a valid header name
- value: is a list of valid header values
- as: and value: cannot both be present

## Output selection

- Empty composite selection
- Leaf with selections

## Input selections (URL path templates and bodies)

- Missing $args
- Missing $this

*/

use std::collections::HashMap;
use std::fmt::Display;
use std::ops::Range;

use apollo_compiler::ast::FieldDefinition;
use apollo_compiler::ast::Name;
use apollo_compiler::ast::Value;
use apollo_compiler::schema::Component;
use apollo_compiler::schema::Directive;
use apollo_compiler::schema::ExtendedType;
use apollo_compiler::schema::ObjectType;
use apollo_compiler::Node;
use apollo_compiler::NodeLocation;
use apollo_compiler::Schema;
use apollo_compiler::SourceMap;
use itertools::Itertools;
use url::Url;

use crate::link::Link;
use crate::sources::connect::spec::schema::CONNECT_HTTP_ARGUMENT_NAME;
use crate::sources::connect::spec::schema::CONNECT_SOURCE_ARGUMENT_NAME;
use crate::sources::connect::spec::schema::SOURCE_BASE_URL_ARGUMENT_NAME;
use crate::sources::connect::spec::schema::SOURCE_DIRECTIVE_NAME_IN_SPEC;
use crate::sources::connect::spec::schema::SOURCE_HTTP_ARGUMENT_NAME;
use crate::sources::connect::spec::schema::SOURCE_NAME_ARGUMENT_NAME;
use crate::sources::connect::ConnectSpecDefinition;

/// Validate the connectors-related directives `@source` and `@connect`.
///
/// This function attempts to collect as many validation errors as possible, so it does not bail
/// out as soon as it encounters one.
pub fn validate(schema: Schema) -> Vec<Message> {
    let connect_identity = ConnectSpecDefinition::identity();
    let Some((link, link_directive)) =
        schema
            .schema_definition
            .directives
            .iter()
            .find_map(|directive| {
                let link = Link::from_directive_application(directive).ok()?;
                if link.url.identity == connect_identity {
                    Some((link, directive))
                } else {
                    None
                }
            })
    else {
        return vec![]; // There are no connectors-related directives to validate
    };
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

    let mut messages = Vec::new();
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

const DEFAULT_SOURCE_DIRECTIVE_NAME: &str = "connect__source";
const DEFAULT_CONNECT_DIRECTIVE_NAME: &str = "connect__connect";

fn validate_source(directive: &Component<Directive>, sources: &SourceMap) -> SourceDirective {
    let name = SourceName::from_directive(directive);
    let url_error = directive
        .arguments
        .iter()
        .find(|arg| arg.name == SOURCE_HTTP_ARGUMENT_NAME)
        .and_then(|arg| {
            let value = arg.value.as_object()?.iter().find_map(|(name, value)| {
                (*name == SOURCE_BASE_URL_ARGUMENT_NAME).then_some(value)
            })?;
            parse_url(
                value,
                &source_base_url_argument_coordinate(&directive.name),
                sources,
            )
            .err()
        });
    let mut errors = Vec::new();
    if let Some(url_error) = url_error {
        errors.push(url_error);
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
                &object.name,
                source_map,
                connect_directive_name,
                source_directive_name,
            )
        })
        .collect()
}

fn validate_field(
    field: &Component<FieldDefinition>,
    category: ObjectCategory,
    source_names: &[SourceName],
    object_name: &Name,
    source_map: &SourceMap,
    connect_directive_name: &Name,
    source_directive_name: &Name,
) -> Vec<Message> {
    let _connect_directive_name = connect_directive_name.as_str();
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
                ),
                locations: Location::from_node(field.location(), source_map)
                    .into_iter()
                    .collect(),
            });
        }
        // TODO: return errors when a field is not resolvable by some combination of `@connect`
        return errors;
    };
    let Some((http_verb, url)) = connect_directive
        .argument_by_name(&CONNECT_HTTP_ARGUMENT_NAME)
        .and_then(
            |http_arg| http_arg.as_object()?.first(), // TODO: Error if more than one verb is defined
        )
    else {
        errors.push(Message {
            code: Code::GraphQLError,
            message: format!(
                "{coordinate} must have a `{CONNECT_HTTP_ARGUMENT_NAME}` argument.",
                coordinate =
                    connect_directive_coordinate(connect_directive_name, object_name, &field.name),
            ),
            locations: Location::from_node(connect_directive.location(), source_map)
                .into_iter()
                .collect(),
        });
        return errors;
    };
    let url_coordinate = connect_directive_url_coordinate(
        connect_directive_name,
        http_verb,
        object_name,
        &field.name,
    );
    if let Some(source_name) = connect_directive
        .arguments
        .iter()
        .find(|arg| arg.name == CONNECT_SOURCE_ARGUMENT_NAME)
    {
        if source_names.iter().all(|name| name != &source_name.value) {
            // TODO: Pick a suggestion that's not just the first defined source
            let qualified_directive = connect_directive_name_coordinate(
                connect_directive_name,
                &source_name.value,
                object_name,
                &field.name,
            );
            if let Some(first_source_name) = source_names.first() {
                errors.push(Message {
                    code: Code::SourceNameMismatch,
                    message: format!(
                        "{qualified_directive} does not match any defined sources. Did you mean {first_source_name}?",
                    ),
                    locations: Location::from_node(source_name.location(), source_map)
                        .into_iter()
                        .collect(),
                });
            } else {
                errors.push(Message {
                    code: Code::NoSourcesDefined,
                    message: format!(
                        "{qualified_directive} specifies a source, but none are defined. Try adding {coordinate} to the schema.",
                        coordinate = source_name_value_coordinate(source_directive_name, &source_name.value),
                    ),
                    locations: Location::from_node(source_name.location(), source_map)
                        .into_iter()
                        .collect(),
                });
            }
        }
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
    } else if let Some(err) = parse_url(url, &url_coordinate, source_map).err() {
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

fn connect_directive_coordinate(
    connect_directive_name: &Name,
    object: &Name,
    field: &Name,
) -> String {
    format!("`@{connect_directive_name}` on `{object}.{field}`")
}

fn connect_directive_name_coordinate(
    connect_directive_name: &Name,
    source: &Node<Value>,
    object: &Name,
    field: &Name,
) -> String {
    format!("`@{connect_directive_name}({CONNECT_SOURCE_ARGUMENT_NAME}: {source})` on `{object}.{field}`")
}

fn connect_directive_url_coordinate(
    connect_directive_name: &Name,
    http_verb: &Name,
    object: &Name,
    field: &Name,
) -> String {
    format!("`{http_verb}` in `@{connect_directive_name}({CONNECT_HTTP_ARGUMENT_NAME}:)` on `{object}.{field}`")
}

fn source_name_argument_coordinate(source_directive_name: &DirectiveName) -> String {
    format!("`@{source_directive_name}({SOURCE_NAME_ARGUMENT_NAME}:)`")
}

fn source_name_value_coordinate(
    source_directive_name: &DirectiveName,
    value: &Node<Value>,
) -> String {
    format!("`@{source_directive_name}({SOURCE_NAME_ARGUMENT_NAME}: {value})`")
}

fn source_base_url_argument_coordinate(source_directive_name: &DirectiveName) -> String {
    format!("`@{source_directive_name}({SOURCE_BASE_URL_ARGUMENT_NAME}:)`")
}

/// The `name` argument of a `@source` directive.
#[derive(Clone, Debug)]
enum SourceName {
    /// A perfectly reasonable source name.
    Valid {
        value: Node<Value>,
        directive_name: DirectiveName,
    },
    /// Contains invalid characters, so it will have to be renamed. This means certain checks
    /// (like uniqueness) should be skipped. However, we have _a_ name, so _other_ checks on the
    /// `@source` directive can continue.
    Invalid {
        value: Node<Value>,
        directive_name: DirectiveName,
    },
    /// The name was an empty string
    Empty {
        directive_name: DirectiveName,
        value: Node<Value>,
    },
    /// No `name` argument was defined
    Missing {
        directive_name: DirectiveName,
        ast_node: Node<Directive>,
    },
}

type DirectiveName = Name;

impl SourceName {
    fn from_directive(directive: &Component<Directive>) -> Self {
        let directive_name = directive.name.clone();
        let Some(arg) = directive
            .arguments
            .iter()
            .find(|arg| arg.name == SOURCE_NAME_ARGUMENT_NAME)
        else {
            return Self::Missing {
                directive_name,
                ast_node: directive.node.clone(),
            };
        };
        let Some(str_value) = arg.value.as_str() else {
            return Self::Invalid {
                value: arg.value.clone(),
                directive_name,
            };
        };
        if str_value.is_empty() {
            Self::Empty {
                directive_name,
                value: arg.value.clone(),
            }
        } else if str_value.chars().all(|c| c.is_alphanumeric() || c == '_') {
            Self::Valid {
                value: arg.value.clone(),
                directive_name,
            }
        } else {
            Self::Invalid {
                value: arg.value.clone(),
                directive_name,
            }
        }
    }

    pub fn into_value_or_error(self, sources: &SourceMap) -> Result<Node<Value>, Message> {
        match self {
            Self::Valid { value, ..} => Ok(value),
            Self::Invalid {
                value,
                directive_name,
            } => Err(Message {
                message: format!("There are invalid characters in {coordinate}. Only alphanumeric and underscores are allowed.", coordinate = source_name_value_coordinate(&directive_name, &value)),
                code: Code::InvalidSourceName,
                locations: Location::from_node(value.location(), sources).into_iter().collect(),
            }),
            Self::Empty { directive_name, value } => {
                Err(Message {
                    code: Code::EmptySourceName,
                    message: format!("The value for {coordinate} can't be empty.", coordinate = source_name_argument_coordinate(&directive_name))   ,
                    locations: Location::from_node(value.location(), sources).into_iter().collect(),
                })
            }
            Self::Missing { directive_name, ast_node } => Err(Message {
                code: Code::GraphQLError,
                message: format!("The {coordinate} argument is required.", coordinate = source_name_argument_coordinate(&directive_name)),
                locations: Location::from_node(ast_node.location(), sources).into_iter().collect()
            }),
        }
    }
}

impl Display for SourceName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Valid {
                value,
                directive_name,
            }
            | Self::Invalid {
                value,
                directive_name,
            } => write!(
                f,
                "`@{directive_name}({SOURCE_NAME_ARGUMENT_NAME}: {value})`"
            ),
            Self::Empty { directive_name, .. } | Self::Missing { directive_name, .. } => {
                write!(f, "unnamed `@{directive_name}`")
            }
        }
    }
}

impl PartialEq<Node<Value>> for SourceName {
    fn eq(&self, other: &Node<Value>) -> bool {
        match self {
            Self::Valid { value, .. } | Self::Invalid { value, .. } => value == other,
            Self::Empty { .. } | Self::Missing { .. } => {
                other.as_str().unwrap_or_default().is_empty()
            }
        }
    }
}

/// A problem was found when validating the connectors directives.
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
    use insta::assert_snapshot;

    use super::*;

    #[test]
    fn unparseable_url() {
        let schema = include_str!("test_data/invalid_source_url.graphql");
        let schema = Schema::parse(schema, "test.graphql").unwrap();
        let errors = validate(schema);
        assert_eq!(errors.len(), 1);
        assert_snapshot!(errors[0].message, @r###"The value "127.0.0.1" for `@source(baseURL:)` is not a valid URL: relative URL without a base."###);
    }

    #[test]
    fn incorrect_scheme() {
        let schema = include_str!("test_data/invalid_source_url_scheme.graphql");
        let schema = Schema::parse(schema, "test.graphql").unwrap();
        let errors = validate(schema);
        assert_eq!(errors.len(), 1);
        assert_snapshot!(errors[0].message, @r###"The value "file://data.json" for `@source(baseURL:)` must be http or https, got file."###);
    }

    #[test]
    fn invalid_chars_in_source_name() {
        let schema = include_str!("test_data/invalid_chars_in_source_name.graphql");
        let schema = Schema::parse(schema, "test.graphql").unwrap();
        let errors = validate(schema);
        assert_eq!(errors.len(), 1);
        assert_snapshot!(errors[0].message, @r###"There are invalid characters in `@source(name: "u$ers")`. Only alphanumeric and underscores are allowed."###);
    }

    #[test]
    fn empty_source_name() {
        let schema = include_str!("test_data/empty_source_name.graphql");
        let schema = Schema::parse(schema, "test.graphql").unwrap();
        let errors = validate(schema);
        assert_eq!(errors.len(), 1);
        assert_snapshot!(errors[0].message, @"The value for `@source(name:)` can't be empty.");
    }

    #[test]
    fn duplicate_source_name() {
        let schema = include_str!("test_data/duplicate_source_name.graphql");
        let schema = Schema::parse(schema, "test.graphql").unwrap();
        let errors = validate(schema);
        assert_eq!(errors.len(), 1);
        assert_snapshot!(errors[0].message, @r###"Every `@source(name:)` must be unique. Found duplicate name "v1"."###);
    }

    #[test]
    fn multiple_errors() {
        let schema = include_str!("test_data/multiple_errors.graphql");
        let schema = Schema::parse(schema, "test.graphql").unwrap();
        let errors = validate(schema);
        assert_eq!(errors.len(), 2);
        assert_snapshot!(errors[0].message, @r###"The value "ftp://127.0.0.1" for `@source(baseURL:)` must be http or https, got ftp."###);
        assert_snapshot!(errors[1].message, @r###"There are invalid characters in `@source(name: "u$ers")`. Only alphanumeric and underscores are allowed."###);
    }

    #[test]
    fn source_directive_rename() {
        let schema = include_str!("test_data/source_directive_rename.graphql");
        let schema = Schema::parse(schema, "test.graphql").unwrap();
        let errors = validate(schema);
        assert_eq!(errors.len(), 1);
        assert_snapshot!(errors[0].message, @r###"The value "blahblahblah" for `@api(baseURL:)` is not a valid URL: relative URL without a base."###);
    }

    #[test]
    fn connect_source_name_mismatch() {
        let schema = include_str!("test_data/connect_source_name_mismatch.graphql");
        let schema = Schema::parse(schema, "test.graphql").unwrap();
        let errors = validate(schema);
        assert_eq!(errors.len(), 1);
        assert_snapshot!(errors[0].message, @r###"`@connect(source: "v1")` on `Query.resources` does not match any defined sources. Did you mean `@source(name: "v2")`?"###);
    }

    #[test]
    fn connect_source_undefined() {
        let schema = include_str!("test_data/connect_source_undefined.graphql");
        let schema = Schema::parse(schema, "test.graphql").unwrap();
        let errors = validate(schema);
        assert_eq!(errors.len(), 1);
        assert_snapshot!(errors[0].message, @r###"`@connect(source: "v1")` on `Query.resources` specifies a source, but none are defined. Try adding `@source(name: "v1")` to the schema."###);
    }

    #[test]
    fn subscriptions_with_connectors() {
        let schema = include_str!("test_data/subscriptions_with_connectors.graphql");
        let schema = Schema::parse(schema, "test.graphql").unwrap();
        let errors = validate(schema);
        assert_eq!(errors.len(), 1);
        assert_snapshot!(errors[0].message, @"A subscription root type is not supported when using `@connect`.");
    }

    #[test]
    fn missing_connect_on_query_field() {
        let schema = include_str!("test_data/missing_connect_on_query_field.graphql");
        let schema = Schema::parse(schema, "test.graphql").unwrap();
        let errors = validate(schema);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].code, Code::QueryFieldMissingConnect);
        assert_snapshot!(errors[0].message, @r###"The field `Query.resources` has no `@connect` directive."###);
    }

    #[test]
    fn renamed_connect_directive() {
        let schema = include_str!("test_data/renamed_connect_directive.graphql");
        let schema = Schema::parse(schema, "test.graphql").unwrap();
        let errors = validate(schema);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].code, Code::QueryFieldMissingConnect);
        assert_snapshot!(errors[0].message, @r###"The field `Query.resources` has no `@data` directive."###);
    }

    #[test]
    fn absolute_connect_url_with_source() {
        let schema = include_str!("test_data/absolute_connect_url_with_source.graphql");
        let schema = Schema::parse(schema, "test.graphql").unwrap();
        let errors = validate(schema);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].code, Code::AbsoluteConnectUrlWithSource);
        assert_snapshot!(errors[0].message, @r###"`GET` in `@connect(http:)` on `Query.resources` contains the absolute URL "http://127.0.0.1/resources" while also specifying a `source`. Either remove the `source` argument or change the URL to a path."###);
    }

    #[test]
    fn invalid_connect_url() {
        let schema = include_str!("test_data/invalid_connect_url.graphql");
        let schema = Schema::parse(schema, "test.graphql").unwrap();
        let errors = validate(schema);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].code, Code::InvalidUrl);
        assert_snapshot!(errors[0].message, @r###"The value "127.0.0.1" for `GET` in `@connect(http:)` on `Query.resources` is not a valid URL: relative URL without a base."###);
    }

    #[test]
    fn invalid_connect_url_scheme() {
        let schema = include_str!("test_data/invalid_connect_url_scheme.graphql");
        let schema = Schema::parse(schema, "test.graphql").unwrap();
        let errors = validate(schema);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].code, Code::SourceScheme);
        assert_snapshot!(errors[0].message, @r###"The value "file://data.json" for `GET` in `@connect(http:)` on `Query.resources` must be http or https, got file."###);
    }

    #[test]
    fn relative_connect_url_without_source() {
        let schema = include_str!("test_data/relative_connect_url_without_source.graphql");
        let schema = Schema::parse(schema, "test.graphql").unwrap();
        let errors = validate(schema);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].code, Code::RelativeConnectUrlWithoutSource);
        assert_snapshot!(errors[0].message, @r###"`GET` in `@connect(http:)` on `Query.resources` specifies the relative URL "/resources", but no `source` is defined. Either use an absolute URL, or add a `@connect__source`."###);
    }

    #[test]
    fn valid_connect_absolute_url() {
        let schema = include_str!("test_data/valid_connect_absolute_url.graphql");
        let schema = Schema::parse(schema, "test.graphql").unwrap();
        let errors = validate(schema);
        assert_eq!(errors.len(), 0);
    }

    #[test]
    fn missing_source_import() {
        let schema = include_str!("test_data/missing_source_import.graphql");
        let schema = Schema::parse(schema, "test.graphql").unwrap();
        let errors = validate(schema);
        assert_eq!(errors.len(), 2);
        assert_eq!(errors[1].code, Code::NoSourceImport);
        assert_snapshot!(errors[1].message, @r###"The `@source` directive is not imported. Try adding `@source` to `import` for `@link(url: "https://specs.apollo.dev/connect")`"###);
    }
}
