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
mod expression;
mod extended_type;
mod graphql;
mod http;
mod selection;
mod source_name;
mod variable;

use std::collections::HashMap;
use std::fmt::Display;
use std::ops::Range;

use apollo_compiler::Name;
use apollo_compiler::Node;
use apollo_compiler::Schema;
use apollo_compiler::ast::OperationType;
use apollo_compiler::ast::Value;
use apollo_compiler::collections::IndexSet;
use apollo_compiler::executable::FieldSet;
use apollo_compiler::name;
use apollo_compiler::parser::LineColumn;
use apollo_compiler::parser::Parser;
use apollo_compiler::schema::Component;
use apollo_compiler::schema::Directive;
use apollo_compiler::schema::ExtendedType;
use apollo_compiler::schema::ObjectType;
use apollo_compiler::schema::SchemaBuilder;
use apollo_compiler::validation::Valid;
use coordinates::source_http_argument_coordinate;
use entity::EntityKeyChecker;
use entity::field_set_error;
use extended_type::validate_extended_type;
use itertools::Itertools;
use source_name::SourceName;
use strum::IntoEnumIterator;
use strum_macros::Display;
use strum_macros::IntoStaticStr;
use url::Url;

use super::Connector;
use crate::link::Import;
use crate::link::Link;
use crate::link::federation_spec_definition::FEDERATION_FIELDS_ARGUMENT_NAME;
use crate::link::federation_spec_definition::FEDERATION_KEY_DIRECTIVE_NAME_IN_SPEC;
use crate::link::federation_spec_definition::FEDERATION_RESOLVABLE_ARGUMENT_NAME;
use crate::link::spec::Identity;
use crate::sources::connect::ConnectSpec;
use crate::sources::connect::spec::schema::HTTP_ARGUMENT_NAME;
use crate::sources::connect::spec::schema::SOURCE_BASE_URL_ARGUMENT_NAME;
use crate::sources::connect::spec::schema::SOURCE_DIRECTIVE_NAME_IN_SPEC;
use crate::sources::connect::spec::schema::SOURCE_NAME_ARGUMENT_NAME;
use crate::sources::connect::validation::coordinates::BaseUrlCoordinate;
use crate::sources::connect::validation::coordinates::HttpHeadersCoordinate;
use crate::sources::connect::validation::graphql::GraphQLString;
use crate::sources::connect::validation::graphql::SchemaInfo;
use crate::sources::connect::validation::http::headers;
use crate::subgraph::spec::CONTEXT_DIRECTIVE_NAME;
use crate::subgraph::spec::EXTERNAL_DIRECTIVE_NAME;
use crate::subgraph::spec::FROM_CONTEXT_DIRECTIVE_NAME;

// The result of a validation pass on a subgraph
#[derive(Debug)]
pub struct ValidationResult {
    /// All validation errors encountered.
    pub errors: Vec<Message>,

    /// Whether or not the validated subgraph contained connector directives
    pub has_connectors: bool,

    /// The parsed (and potentially invalid) schema of the subgraph
    pub schema: Schema,
}

/// Validate the connectors-related directives `@source` and `@connect`.
///
/// This function attempts to collect as many validation errors as possible, so it does not bail
/// out as soon as it encounters one.
pub fn validate(source_text: &str, file_name: &str) -> ValidationResult {
    // TODO: Use parse_and_validate (adding in directives as needed)
    // TODO: Handle schema errors rather than relying on JavaScript to catch it later
    let schema = SchemaBuilder::new()
        .adopt_orphan_extensions()
        .parse(source_text, file_name)
        .build()
        .unwrap_or_else(|schema_with_errors| schema_with_errors.partial);
    let connect_identity = ConnectSpec::identity();
    let Some((link, link_directive)) = Link::for_identity(&schema, &connect_identity) else {
        // There are no connectors-related directives to validate
        return ValidationResult {
            errors: Vec::new(),
            has_connectors: false,
            schema,
        };
    };

    if let Err(err) = ConnectSpec::try_from(&link.url.version) {
        let available_versions = ConnectSpec::iter().map(ConnectSpec::as_str).collect_vec();
        let message = if available_versions.len() == 1 {
            // TODO: No need to branch here once multiple spec versions are available
            format!("{err}; should be {version}.", version = ConnectSpec::V0_1)
        } else {
            // This won't happen today, but it's prepping for 0.2 so we don't forget
            format!(
                "{err}; should be one of {available_versions}.",
                available_versions = available_versions.join(", "),
            )
        };
        return ValidationResult {
            errors: vec![Message {
                code: Code::UnknownConnectorsVersion,
                message,
                locations: link_directive
                    .line_column_range(&schema.sources)
                    .into_iter()
                    .collect(),
            }],
            has_connectors: true,
            schema,
        };
    }

    let federation = Link::for_identity(&schema, &Identity::federation_identity());
    let external_directive_name = federation
        .map(|(link, _)| link.directive_name_in_schema(&EXTERNAL_DIRECTIVE_NAME))
        .unwrap_or(EXTERNAL_DIRECTIVE_NAME.clone());

    let mut messages = check_conflicting_directives(&schema);

    let source_directive_name = ConnectSpec::source_directive_name(&link);
    let connect_directive_name = ConnectSpec::connect_directive_name(&link);
    let schema_info = SchemaInfo::new(
        &schema,
        source_text,
        &connect_directive_name,
        &source_directive_name,
    );

    let source_directives: Vec<SourceDirective> = schema
        .schema_definition
        .directives
        .iter()
        .filter(|directive| directive.name == source_directive_name)
        .map(|directive| validate_source(directive, &schema_info))
        .collect();

    let mut valid_source_names = HashMap::new();
    let all_source_names = source_directives
        .iter()
        .map(|directive| directive.name.clone())
        .collect_vec();
    for directive in source_directives {
        messages.extend(directive.errors);
        match directive.name.into_value_or_error(&schema_info.sources) {
            Err(error) => messages.push(error),
            Ok(name) => valid_source_names
                .entry(name)
                .or_insert_with(Vec::new)
                .extend(
                    directive
                        .directive
                        .node
                        .line_column_range(&schema_info.sources),
                ),
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
            &schema_info,
            &all_source_names,
            &mut seen_fields,
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
            locations: link_directive.line_column_range(&schema.sources)
                .into_iter()
                .collect(),
        });
    }

    if should_do_advanced_validations(&messages) {
        messages.extend(check_seen_fields(
            &schema_info,
            &seen_fields,
            &external_directive_name,
        ));

        match advanced_validations(&schema, file_name, ConnectSpec::V0_1) {
            Ok(multiple) => messages.extend(multiple),
            Err(Some(single)) => messages.push(single),
            _ => {} // let the rest of composition handle this
        };
    }

    ValidationResult {
        errors: messages,
        has_connectors: true,
        schema,
    }
}

fn advanced_validations(
    schema: &Schema,
    subgraph_name: &str,
    spec: ConnectSpec,
) -> Result<Vec<Message>, Option<Message>> {
    let mut messages = vec![];

    let connectors = Connector::from_schema(schema, subgraph_name, spec).map_err(|_| None)?;

    let mut entity_checker = EntityKeyChecker::default();

    for (field_set, directive) in find_all_resolvable_keys(schema) {
        entity_checker.add_key(&field_set, directive);
    }

    for (_, connector) in connectors {
        if let Some(field_set) = connector.resolvable_key(schema).map_err(|_| {
            let variables = connector.variable_references().collect_vec();
            field_set_error(&variables, connector.id.directive.field.type_name())
        })? {
            entity_checker.add_connector(field_set);
        }
    }

    messages.extend(entity_checker.check_for_missing_entity_connectors(schema));

    Ok(messages)
}

/// We'll avoid doing this work if there are bigger issues with the schema.
/// Otherwise we might emit a large number of diagnostics that will
/// distract from the main problems.
fn should_do_advanced_validations(messages: &[Message]) -> bool {
    messages.is_empty()
}

/// Check that all fields defined in the schema are resolved by a connector.
fn check_seen_fields(
    schema: &SchemaInfo,
    seen_fields: &IndexSet<(Name, Name)>,
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

            // ignore all fields on objects marked @external
            if extended_type
                .directives()
                .iter()
                .any(|dir| &dir.name == external_directive_name)
            {
                return None;
            }

            match extended_type {
                ExtendedType::Object(object) => {
                    // ignore fields marked @external
                    Some(
                        object
                            .fields
                            .iter()
                            .filter(|(_, def)| {
                                !def.directives
                                    .iter()
                                    .any(|dir| &dir.name == external_directive_name)
                            })
                            .map(coord),
                    )
                }
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
            code: Code::ConnectorsUnresolvedField,
            message: format!(
                "No connector resolves field `{parent_type}.{field_name}`. It must have a `@{connect_directive_name}` directive or appear in `@{connect_directive_name}(selection:)`.",
                connect_directive_name = schema.connect_directive_name
            ),
            locations: field_def.line_column_range(&schema.sources).into_iter().collect(),
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
        .specified_argument_by_name(&name!("import"))
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
                    code: Code::ConnectorsUnsupportedFederationDirective,
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
#[allow(unused)]
const DEFAULT_CONNECT_DIRECTIVE_NAME: &str = "connect__connect";

fn validate_source(directive: &Component<Directive>, schema: &SchemaInfo) -> SourceDirective {
    let name = SourceName::from_directive(directive);
    let mut errors = Vec::new();

    if let Some(http_arg) = directive
        .specified_argument_by_name(&HTTP_ARGUMENT_NAME)
        .and_then(|arg| arg.as_object())
    {
        // Validate URL argument
        if let Some(url_value) = http_arg
            .iter()
            .find_map(|(key, value)| (key == &SOURCE_BASE_URL_ARGUMENT_NAME).then_some(value))
        {
            if let Some(url_error) = parse_url(
                url_value,
                BaseUrlCoordinate {
                    source_directive_name: &directive.name,
                },
                schema,
            )
            .err()
            {
                errors.push(url_error);
            }
        }

        errors.extend(headers::validate_arg(
            http_arg,
            HttpHeadersCoordinate::Source {
                directive_name: &directive.name,
            },
            schema,
        ));
    } else {
        errors.push(Message {
            code: Code::GraphQLError,
            message: format!(
                "{coordinate} must have a `{HTTP_ARGUMENT_NAME}` argument.",
                coordinate = source_http_argument_coordinate(&directive.name),
            ),
            locations: directive
                .line_column_range(&schema.sources)
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

fn parse_url<Coordinate: Display + Copy>(
    value: &Node<Value>,
    coordinate: Coordinate,
    schema: &SchemaInfo,
) -> Result<(), Message> {
    let str_value = GraphQLString::new(value, &schema.sources).map_err(|_| Message {
        code: Code::GraphQLError,
        message: format!("The value for {coordinate} must be a string."),
        locations: value
            .line_column_range(&schema.sources)
            .into_iter()
            .collect(),
    })?;
    let url = Url::parse(str_value.as_str()).map_err(|inner| Message {
        code: Code::InvalidUrl,
        message: format!("The value {value} for {coordinate} is not a valid URL: {inner}."),
        locations: value
            .line_column_range(&schema.sources)
            .into_iter()
            .collect(),
    })?;
    http::url::validate_base_url(&url, coordinate, value, str_value, schema)
}

/// For an object type, get all the keys (and directive nodes) that are resolvable.
///
/// The FieldSet returned here is what goes in the `fields` argument, so `id` in `@key(fields: "id")`
fn resolvable_key_fields<'a>(
    object: &'a Node<ObjectType>,
    schema: &'a Schema,
) -> impl Iterator<Item = (FieldSet, &'a Component<Directive>)> {
    object
        .directives
        .iter()
        .filter(|directive| directive.name == FEDERATION_KEY_DIRECTIVE_NAME_IN_SPEC)
        .filter(|directive| {
            directive
                .arguments
                .iter()
                .find(|arg| arg.name == FEDERATION_RESOLVABLE_ARGUMENT_NAME)
                .and_then(|arg| arg.value.to_bool())
                .unwrap_or(true)
        })
        .filter_map(|directive| {
            if let Some(fields_str) = directive
                .arguments
                .iter()
                .find(|arg| arg.name == FEDERATION_FIELDS_ARGUMENT_NAME)
                .map(|arg| &arg.value)
                .and_then(|value| value.as_str())
            {
                Parser::new()
                    .parse_field_set(
                        Valid::assume_valid_ref(schema),
                        object.name.clone(),
                        fields_str.to_string(),
                        "",
                    )
                    .ok()
                    .map(|field_set| (field_set, directive))
            } else {
                None
            }
        })
}

fn find_all_resolvable_keys(schema: &Schema) -> Vec<(FieldSet, &Component<Directive>)> {
    schema
        .types
        .values()
        .flat_map(|extended_type| match extended_type {
            ExtendedType::Object(object) => Some(resolvable_key_fields(object, schema)),
            _ => None,
        })
        .flatten()
        .collect()
}

type DirectiveName = Name;

#[derive(Debug, Clone)]
pub struct Message {
    /// A unique, per-error code to allow consuming tools to take specific actions. These codes
    /// should not change once stabilized.
    pub code: Code,
    /// A human-readable message describing the error. These messages are not stable, tools should
    /// not rely on them remaining the same.
    ///
    /// # Formatting messages
    /// 1. Messages should be complete sentences, starting with capitalization as appropriate and
    ///    ending with punctuation.
    /// 2. When referring to elements of the schema, use
    ///    [schema coordinates](https://github.com/graphql/graphql-wg/blob/main/rfcs/SchemaCoordinates.md)
    ///    with any additional information added as required for clarity (e.g., the value of an arg).
    /// 3. When referring to code elements (including schema coordinates), surround them with
    ///    backticks. This clarifies that `Type.field` is not ending a sentence with its period.
    pub message: String,
    pub locations: Vec<Range<LineColumn>>,
}

/// The error code that will be shown to users when a validation fails during composition.
///
/// Note that these codes are global, not scoped to connectors, so they should attempt to be
/// unique across all pieces of composition, including JavaScript components.
#[derive(Clone, Copy, Debug, Display, Eq, IntoStaticStr, PartialEq)]
#[strum(serialize_all = "SCREAMING_SNAKE_CASE")]
pub enum Code {
    /// A problem with GraphQL syntax or semantics was found. These will usually be caught before
    /// this validation process.
    GraphQLError,
    /// Indicates two connector sources with the same name were created.
    DuplicateSourceName,
    /// The `name` provided for a `@source` was invalid.
    InvalidSourceName,
    /// No `name` was provided when creating a connector source with `@source`.
    EmptySourceName,
    /// A URL provided to `@source` or `@connect` was not valid.
    InvalidUrl,
    /// A URL scheme provided to `@source` or `@connect` was not `http` or `https`.
    InvalidUrlScheme,
    /// The `source` argument used in a `@connect` directive doesn't match any named connecter
    /// sources created with `@source`.
    SourceNameMismatch,
    /// Connectors currently don't support subscription operations.
    SubscriptionInConnectors,
    /// A query field is missing the `@connect` directive.
    QueryFieldMissingConnect,
    /// A mutation field is missing the `@connect` directive.
    MutationFieldMissingConnect,
    /// The `@connect` is using a `source`, but the URL is absolute. This is not allowed because
    /// the `@source` URL will be joined with the `@connect` URL, so the `@connect` URL should
    /// only be a path.
    AbsoluteConnectUrlWithSource,
    /// The `@connect` directive is using a relative URL (path only) but does not define a `source`.
    /// This is a specialization of [`Self::InvalidUrl`].
    RelativeConnectUrlWithoutSource,
    /// This is a specialization of [`Self::SourceNameMismatch`] that indicates no sources were defined.
    NoSourcesDefined,
    /// The subgraph doesn't import the `@source` directive. This isn't necessarily a problem, but
    /// is likely a mistake.
    NoSourceImport,
    /// The `@connect` directive has multiple HTTP methods when only one is allowed.
    MultipleHttpMethods,
    /// The `@connect` directive is missing an HTTP method.
    MissingHttpMethod,
    /// The `@connect` directive's `entity` argument should only be used on the root `Query` field.
    EntityNotOnRootQuery,
    /// The arguments to the entity reference resolver do not match the entity type.
    EntityResolverArgumentMismatch,
    /// The `@connect` directive's `entity` argument should only be used with non-list, nullable, object types.
    EntityTypeInvalid,
    /// A `@key` was defined without a corresponding entity connector.
    MissingEntityConnector,
    /// The provided selection mapping in a `@connect`s `selection` was not valid.
    InvalidSelection,
    /// The `http.body` provided in `@connect` was not valid.
    InvalidBody,
    /// A circular reference was detected in a `@connect` directive's `selection` argument.
    CircularReference,
    /// A field included in a `@connect` directive's `selection` argument is not defined on the corresponding type.
    SelectedFieldNotFound,
    /// A group selection mapping (`a { b }`) was used, but the field is not an object.
    GroupSelectionIsNotObject,
    /// The `name` mapping must be unique for all headers.
    HttpHeaderNameCollision,
    /// A provided header in `@source` or `@connect` was not valid.
    InvalidHeader,
    /// Certain directives are not allowed when using connectors.
    ConnectorsUnsupportedFederationDirective,
    /// Abstract types are not allowed when using connectors.
    ConnectorsUnsupportedAbstractType,
    /// Fields that return an object type must use a group selection mapping `{}`.
    GroupSelectionRequiredForObject,
    /// The schema includes fields that aren't resolved by a connector.
    ConnectorsUnresolvedField,
    /// A field resolved by a connector has arguments defined.
    ConnectorsFieldWithArguments,
    /// Part of the `@connect` refers to an `$args` which is not defined.
    UndefinedArgument,
    /// Part of the `@connect` refers to an `$this` which is not defined.
    UndefinedField,
    /// A type used in a variable is not yet supported (i.e., unions).
    UnsupportedVariableType,
    /// A variable is nullable in a location which requires non-null at runtime.
    NullabilityMismatch,
    /// The version set in the connectors `@link` URL is not recognized.
    UnknownConnectorsVersion,
}

impl Code {
    pub const fn severity(&self) -> Severity {
        match self {
            Self::NoSourceImport | Self::NullabilityMismatch => Severity::Warning,
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
            glob!("test_data", "**/*.graphql", |path| {
                let schema = read_to_string(path).unwrap();
                let start_time = std::time::Instant::now();
                let result = validate(&schema, path.to_str().unwrap());
                let end_time = std::time::Instant::now();
                assert_snapshot!(format!("{:#?}", result.errors));
                assert!(end_time - start_time < std::time::Duration::from_millis(100));
            });
        });
    }
}
