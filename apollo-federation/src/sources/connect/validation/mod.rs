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

mod coordinates;
mod entity;
mod extended_type;
mod http_headers;
mod http_method;
mod selection;
mod source_name;

use std::collections::HashMap;
use std::ops::Range;

use apollo_compiler::ast::OperationType;
use apollo_compiler::ast::Value;
use apollo_compiler::collections::IndexSet;
use apollo_compiler::name;
use apollo_compiler::parser::LineColumn;
use apollo_compiler::parser::SourceMap;
use apollo_compiler::schema::Component;
use apollo_compiler::schema::Directive;
use apollo_compiler::schema::ExtendedType;
use apollo_compiler::Name;
use apollo_compiler::Node;
use apollo_compiler::Schema;
use coordinates::source_base_url_argument_coordinate;
use coordinates::source_http_argument_coordinate;
use extended_type::validate_extended_type;
use http_headers::get_http_headers_arg;
use http_headers::validate_headers_arg;
use itertools::Itertools;
use source_name::SourceName;
use url::Url;

use crate::link::spec::Identity;
use crate::link::Import;
use crate::link::Link;
use crate::sources::connect::spec::schema::HTTP_ARGUMENT_NAME;
use crate::sources::connect::spec::schema::SOURCE_BASE_URL_ARGUMENT_NAME;
use crate::sources::connect::spec::schema::SOURCE_DIRECTIVE_NAME_IN_SPEC;
use crate::sources::connect::spec::schema::SOURCE_NAME_ARGUMENT_NAME;
use crate::sources::connect::ConnectSpecDefinition;
use crate::subgraph::spec::CONTEXT_DIRECTIVE_NAME;
use crate::subgraph::spec::EXTERNAL_DIRECTIVE_NAME;
use crate::subgraph::spec::FROM_CONTEXT_DIRECTIVE_NAME;

/// Validate the connectors-related directives `@source` and `@connect`.
///
/// This function attempts to collect as many validation errors as possible, so it does not bail
/// out as soon as it encounters one.
pub fn validate(schema: Schema) -> Vec<Message> {
    let connect_identity = ConnectSpecDefinition::identity();
    let Some((link, link_directive)) = Link::for_identity(&schema, &connect_identity) else {
        return Vec::new(); // There are no connectors-related directives to validate
    };

    let federation = Link::for_identity(&schema, &Identity::federation_identity());
    let external_directive_name = federation
        .map(|(link, _)| link.directive_name_in_schema(&EXTERNAL_DIRECTIVE_NAME))
        .unwrap_or(EXTERNAL_DIRECTIVE_NAME.clone());

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
                .extend(directive.directive.node.line_column_range(source_map)),
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

    let mut seen_fields = IndexSet::default();

    let connect_errors = schema.types.values().flat_map(|extended_type| {
        validate_extended_type(
            extended_type,
            &schema,
            &connect_directive_name,
            &source_directive_name,
            &all_source_names,
            source_map,
            &mut seen_fields,
        )
    });
    messages.extend(connect_errors);

    if should_check_seen_fields(&messages) {
        messages.extend(check_seen_fields(
            &schema,
            &seen_fields,
            source_map,
            &connect_directive_name,
            &external_directive_name,
        ));
    }

    if source_directive_name == DEFAULT_SOURCE_DIRECTIVE_NAME
        && messages
            .iter()
            .any(|error| error.code == Code::NoSourcesDefined)
    {
        messages.push(Message {
            code: Code::NoSourceImport,
            message: format!("The `@{SOURCE_DIRECTIVE_NAME_IN_SPEC}` directive is not imported. Try adding `@{SOURCE_DIRECTIVE_NAME_IN_SPEC}` to `import` for `@{link_name}(url: \"{connect_identity}\")`", link_name=link_directive.name),
            locations: link_directive.line_column_range(source_map)
                .into_iter()
                .collect(),
        });
    }
    messages
}

/// We'll avoid doing this work if there are bigger issues with the schema.
/// Otherwise we might emit a large number of diagnostics that will
/// distract from the main problems.
fn should_check_seen_fields(messages: &[Message]) -> bool {
    !messages.iter().any(|error| {
        // some invariant is violated, so let's just stop here
        error.code == Code::GraphQLError
            // the selection visitor emits these errors and stops visiting, so there will probably be fields we haven't visited
            || error.code == Code::SelectedFieldNotFound
            || error.code == Code::GroupSelectionIsNotObject
            || error.code == Code::GroupSelectionRequiredForObject
            // if we encounter unsupported definitions, there are probably related definitions that we won't be able to resolve
            || error.code == Code::SubscriptionInConnectors
            || error.code == Code::UnsupportedAbstractType
    })
}

/// Check that all fields defined in the schema are resolved by a connector.
fn check_seen_fields(
    schema: &Schema,
    seen_fields: &IndexSet<(Name, Name)>,
    source_map: &SourceMap,
    connect_directive_name: &Name,
    external_directive_name: &Name,
) -> Vec<Message> {
    let all_fields: IndexSet<_> = schema
        .types
        .values()
        .filter_map(|extended_type| {
            if extended_type.is_built_in() {
                return None;
            }
            // ignore root fields, we have different validations for them
            if schema.root_operation(OperationType::Query) == Some(extended_type.name())
                || schema.root_operation(OperationType::Mutation) == Some(extended_type.name())
                || schema.root_operation(OperationType::Subscription) == Some(extended_type.name())
            {
                return None;
            }
            let coord = |(name, _): (&Name, _)| (extended_type.name().clone(), name.clone());
            match extended_type {
                ExtendedType::Object(object) => Some(
                    // ignore @external fields
                    object
                        .fields
                        .iter()
                        .filter(|(_, def)| {
                            !def.directives
                                .iter()
                                .any(|dir| &dir.name == external_directive_name)
                        })
                        .map(coord),
                ),
                ExtendedType::Interface(_) => None, // TODO: when interfaces are supported (probably should include fields from implementing/member types as well)
                _ => None,
            }
        })
        .flatten()
        .collect();

    (&all_fields - seen_fields).iter().map(move |(parent_type, field_name)| {
        let Ok(field_def) = schema.type_field(parent_type, field_name) else {
            // This should never happen, but if it does, we don't want to panic
            return Message {
                code: Code::GraphQLError,
                message: format!(
                    "Field `{parent_type}.{field_name}` is missing from the schema.",
                ),
                locations: Vec::new(),
            };
        };
        Message {
            code: Code::UnresolvedField,
            message: format!(
                "No connector resolves field `{parent_type}.{field_name}`. It must have a `@{connect_directive_name}` directive or appear in `@{connect_directive_name}(selection:)`.",
            ),
            locations: field_def.line_column_range(source_map).into_iter().collect(),
        }
    }).collect()
}

fn check_conflicting_directives(schema: &Schema) -> Vec<Message> {
    let Some((fed_link, fed_link_directive)) =
        Link::for_identity(schema, &Identity::federation_identity())
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

    let disallowed_imports = [CONTEXT_DIRECTIVE_NAME, FROM_CONTEXT_DIRECTIVE_NAME];
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
                            (*reparsed == *import).then(|| value.line_column_range(&schema.sources))
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
        .argument_by_name(&HTTP_ARGUMENT_NAME)
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
        if let Some(headers) = get_http_headers_arg(http_arg) {
            let header_errors = validate_headers_arg(&directive.name, headers, sources, None, None);
            errors.extend(header_errors);
        }
    } else {
        errors.push(Message {
            code: Code::GraphQLError,
            message: format!(
                "{coordinate} must have a `{HTTP_ARGUMENT_NAME}` argument.",
                coordinate = source_http_argument_coordinate(&directive.name),
            ),
            locations: directive.line_column_range(sources).into_iter().collect(),
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

fn parse_url(value: &Node<Value>, coordinate: &str, sources: &SourceMap) -> Result<Url, Message> {
    let str_value = require_value_is_str(value, coordinate, sources)?;
    let url = Url::parse(str_value).map_err(|inner| Message {
        code: Code::InvalidUrl,
        message: format!("The value {value} for {coordinate} is not a valid URL: {inner}.",),
        locations: value.line_column_range(sources).into_iter().collect(),
    })?;
    let scheme = url.scheme();
    if scheme != "http" && scheme != "https" {
        return Err(Message {
            code: Code::SourceScheme,
            message: format!(
                "The value {value} for {coordinate} must be http or https, got {scheme}.",
            ),
            locations: value.line_column_range(sources).into_iter().collect(),
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
        locations: value.line_column_range(sources).into_iter().collect(),
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
    pub locations: Vec<Range<LineColumn>>,
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
    /// Query field is missing the `@connect` directive
    QueryFieldMissingConnect,
    /// Mutation field is missing the `@connect` directive
    MutationFieldMissingConnect,
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
    /// Abstract types are not allowed when using connectors
    UnsupportedAbstractType,
    /// Header does not define `from` or `value`
    MissingHeaderSource,
    /// Fields that return an object type must use a group JSONSelection `{}`
    GroupSelectionRequiredForObject,
    /// Fields in the schema that aren't resolved by a connector
    UnresolvedField,
    /// A field resolved by a connector has arguments defined
    FieldWithArguments,
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
                let schema = Schema::parse(schema, path).unwrap();
                let errors = validate(schema);
                assert_snapshot!(format!("{:?}", errors));
            });
        });
    }
}
