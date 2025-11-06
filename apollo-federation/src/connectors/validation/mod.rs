//! Validation of the `@source` and `@connect` directives.

mod connect;
mod coordinates;
mod errors;
mod expression;
mod graphql;
mod http;
mod schema;
mod source;

use std::ops::Range;

use apollo_compiler::Name;
use apollo_compiler::Schema;
use apollo_compiler::parser::LineColumn;
use apollo_compiler::schema::SchemaBuilder;
use itertools::Itertools;
pub(crate) use schema::field_set_is_subset;
use strum_macros::Display;
use strum_macros::IntoStaticStr;

use crate::connectors::ConnectSpec;
use crate::connectors::spec::ConnectLink;
use crate::connectors::spec::source::SOURCE_DIRECTIVE_NAME_IN_SPEC;
use crate::connectors::validation::connect::fields_seen_by_all_connects;
use crate::connectors::validation::graphql::SchemaInfo;
use crate::connectors::validation::source::SourceDirective;

/// The result of a validation pass on a subgraph
#[derive(Debug)]
pub struct ValidationResult {
    /// All validation errors encountered.
    pub errors: Vec<Message>,

    /// Whether the validated subgraph contained connector directives
    pub has_connectors: bool,

    /// The parsed (and potentially invalid) schema of the subgraph
    pub schema: Schema,

    /// The optionally transformed schema to be used in later steps.
    pub transformed: String,
}

/// Validate the connectors-related directives `@source` and `@connect`.
///
/// This function attempts to collect as many validation errors as possible, so it does not bail
/// out as soon as it encounters one.
pub fn validate(mut source_text: String, file_name: &str) -> ValidationResult {
    let schema = SchemaBuilder::new()
        .adopt_orphan_extensions()
        .parse(&source_text, file_name)
        .build()
        .unwrap_or_else(|schema_with_errors| schema_with_errors.partial);
    let link = match ConnectLink::new(&schema) {
        None => {
            return ValidationResult {
                errors: Vec::new(),
                has_connectors: false,
                schema,
                transformed: source_text,
            };
        }
        Some(Err(err)) => {
            return ValidationResult {
                errors: vec![err],
                has_connectors: true,
                schema,
                transformed: source_text,
            };
        }
        Some(Ok(link)) => link,
    };
    let schema_info = SchemaInfo::new(&schema, &source_text, link);

    let (source_directives, mut messages) = SourceDirective::find(&schema_info);
    let all_source_names = source_directives
        .iter()
        .map(|directive| directive.name.clone())
        .collect_vec();

    for source in source_directives {
        messages.extend(source.type_check());
    }

    match fields_seen_by_all_connects(&schema_info, &all_source_names) {
        Ok(fields_seen_by_connectors) => {
            // Don't run schema-wide checks if any connectors failed to validate
            messages.extend(schema::validate(
                &schema_info,
                file_name,
                fields_seen_by_connectors,
            ))
        }
        Err(errs) => {
            messages.extend(errs);
        }
    }

    if schema_info.source_directive_name() == DEFAULT_SOURCE_DIRECTIVE_NAME
        && messages
            .iter()
            .any(|error| error.code == Code::NoSourcesDefined)
    {
        messages.push(Message {
            code: Code::NoSourceImport,
            message: format!("The `@{SOURCE_DIRECTIVE_NAME_IN_SPEC}` directive is not imported. Try adding `@{SOURCE_DIRECTIVE_NAME_IN_SPEC}` to `import` for `{link}`", link=schema_info.connect_link),
            locations: schema_info.connect_link.directive.line_column_range(&schema.sources)
                .into_iter()
                .collect(),
        });
    }

    // Auto-upgrade the schema as the _last_ step, so that error messages from earlier don't have
    // incorrect line/col info if we mess this up
    if schema_info.connect_link.spec == ConnectSpec::V0_1 {
        if let Some(version_range) =
            schema_info
                .connect_link
                .directive
                .location()
                .and_then(|link_range| {
                    let version_offset = source_text
                        .get(link_range.offset()..link_range.end_offset())?
                        .find(ConnectSpec::V0_1.as_str())?;
                    let start = link_range.offset() + version_offset;
                    let end = start + ConnectSpec::V0_1.as_str().len();
                    Some(start..end)
                })
        {
            source_text.replace_range(version_range, ConnectSpec::V0_2.as_str());
        } else {
            messages.push(Message {
                code: Code::UnknownConnectorsVersion,
                message: "Failed to auto-upgrade 0.1 to 0.2, you must manually update the version in `@link`".to_string(),
                locations: schema_info.connect_link.directive.line_column_range(&schema.sources)
                    .into_iter()
                    .collect(),
            });
            return ValidationResult {
                errors: messages,
                has_connectors: true,
                schema,
                transformed: source_text,
            };
        };
    }

    ValidationResult {
        errors: messages,
        has_connectors: true,
        schema,
        transformed: source_text,
    }
}

const DEFAULT_SOURCE_DIRECTIVE_NAME: &str = "connect__source";

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
    /// Indicates two connector IDs with the same name were created.
    DuplicateIdName,
    /// The `name` provided for a `@source` was invalid.
    InvalidSourceName,
    /// No `name` was provided when creating a connector source with `@source`.
    EmptySourceName,
    /// Connector ID name must be `alphanumeric_`.
    InvalidConnectorIdName,
    /// A URL provided to `@source` or `@connect` was not valid.
    InvalidUrl,
    /// A URL scheme provided to `@source` or `@connect` was not `http` or `https`.
    InvalidUrlScheme,
    /// The `source` argument used in a `@connect` directive doesn't match any named connector
    /// sources created with `@source`.
    SourceNameMismatch,
    /// Connectors currently don't support subscription operations.
    SubscriptionInConnectors,
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
    /// The `errors.message` provided in `@connect` or `@source` was not valid.
    InvalidErrorsMessage,
    /// The `isSuccess` mapping provided in `@connect` or `@source` was not valid.
    InvalidIsSuccess,
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
    /// Connector batch key is not reflected in the output selection
    ConnectorsBatchKeyNotInSelection,
    /// Connector batch key is derived from a non-root variable such as `$this` or `$context`.
    ConnectorsNonRootBatchKey,
    /// A `@key` could not be resolved for the given combination of variables.
    ConnectorsCannotResolveKey,
    /// Part of the `@connect` refers to an `$args` which is not defined.
    UndefinedArgument,
    /// Part of the `@connect` refers to an `$this` which is not defined.
    UndefinedField,
    /// A type used in a variable is not yet supported (i.e., unions).
    UnsupportedVariableType,
    /// The version set in the connectors `@link` URL is not recognized.
    UnknownConnectorsVersion,
    /// When `@connect` is applied to a type, `entity` can't be set to `false`
    ConnectOnTypeMustBeEntity,
    /// `@connect` cannot be applied to a query, mutation, or subscription root type
    ConnectOnRoot,
    /// Using both `$batch` and `$this` is not allowed
    ConnectBatchAndThis,
    /// Invalid URL property
    InvalidUrlProperty,
    /// The `fragments` mapping provided in `@source` was not valid.
    InvalidFragments,
}

impl Code {
    pub fn severity(&self) -> Severity {
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
    use pretty_assertions::assert_str_eq;

    use super::*;

    #[test]
    fn validation_tests() {
        insta::with_settings!({prepend_module_to_snapshot => false}, {
            glob!("test_data", "**/*.graphql", |path| {
                let schema = read_to_string(path).unwrap();
                let start_time = std::time::Instant::now();
                let result = validate(schema.clone(), path.to_str().unwrap());
                let end_time = std::time::Instant::now();
                assert_snapshot!(format!("{:#?}", result.errors));
                if path.parent().is_some_and(|parent| parent.ends_with("transformed")) {
                    assert_snapshot!(&diff::lines(&schema, &result.transformed).into_iter().filter_map(|res| match res {
                        diff::Result::Left(line) => Some(format!("- {line}")),
                        diff::Result::Right(line) => Some(format!("+ {line}")),
                        diff::Result::Both(_, _) => None,
                    }).join("\n"));
                } else {
                    assert_str_eq!(schema, result.transformed, "Schema should not have been transformed by validations")
                }

                assert!(end_time - start_time < std::time::Duration::from_millis(100));
            });
        });
    }
}
