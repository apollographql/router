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
use apollo_compiler::parser::LineColumn;
use itertools::Itertools;
pub(crate) use schema::field_set_is_subset;
use strum_macros::Display;
use strum_macros::EnumIter;
use strum_macros::IntoStaticStr;

use crate::connectors::spec::ConnectLink;
use crate::connectors::spec::source::SOURCE_DIRECTIVE_NAME_IN_SPEC;
use crate::connectors::validation::connect::fields_seen_by_all_connects;
use crate::connectors::validation::graphql::SchemaInfo;
use crate::connectors::validation::source::SourceDirective;
use crate::subgraph::typestate::HasMetadata;
use crate::subgraph::typestate::Subgraph;

/// Validate the connectors-related directives `@source` and `@connect`.
///
/// Returns every [`Message`] produced, errors and warnings alike — see [`Code::severity`]. This
/// function attempts to collect as many of them as possible, so it does not bail out as soon as it
/// encounters one. An empty result means the subgraph either has no connectors or has no problems.
///
/// Takes an expanded subgraph because that is the state these validations are defined against: the
/// connect and federation definitions are present and link metadata has been collected, but the
/// schema has not been GraphQL-validated yet. The subgraph is only read, never rewritten —
/// normalizing the spec version (`connect/v0.1` is auto-upgraded to `v0.2`) is link expansion's
/// job, see `connectors::spec::upgrade_connect_link_if_needed`.
#[allow(private_bounds)]
pub fn validate<S: HasMetadata>(subgraph: &Subgraph<S>) -> Vec<Message> {
    let schema = subgraph.schema();
    let subgraph_name = subgraph.name.as_str();

    let link = match ConnectLink::new(schema.schema()) {
        // Not a connectors subgraph.
        None => return Vec::new(),
        Some(Err(err)) => return vec![err],
        Some(Ok(link)) => link,
    };

    let schema_info = SchemaInfo::new(schema, link);

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
                subgraph_name,
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
            locations: schema_info.connect_link.directive.line_column_range(&schema.schema().sources)
                .into_iter()
                .collect(),
        });
    }

    messages
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
#[derive(Clone, Copy, Debug, Display, EnumIter, Eq, Hash, IntoStaticStr, PartialEq)]
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
    /// Any named type not found in a GraphQL schema where expected
    MissingSchemaType,
    /// Omitting `http:` from `@connect` requires connect spec v0.4 or later
    HttpOmittedRequiresV0_4,
    /// A `@connect` selection is requestless — the directive specifies no
    /// transport (`http:` is absent, and no other transport argument has been
    /// added yet) — but the selection reads request-phase data: `$root` (the
    /// response body), `$status` (the response status), or `$response` (the
    /// response headers). None of those are bound without a transport, so the
    /// offending paths would silently produce `null` at runtime. The wording
    /// is transport-agnostic so that if/when a `sql:` (or other) transport
    /// joins `http:`, this same code keeps describing the same condition.
    RequestlessSelectionUsesRequestData,
    /// A field uses `@override(from:)` to override a connector-enabled subgraph.
    ///
    /// Unlike every other code here, this one is not raised by the subgraph validations in this
    /// module — it comes from a post-merge composition check, which needs all subgraphs at once.
    /// See `schema::validators::connectors::validate_override_on_connector`.
    OverrideOnConnector,
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

    use apollo_compiler::schema::SchemaBuilder;
    use insta::assert_debug_snapshot;
    use insta::glob;
    use pretty_assertions::assert_str_eq;

    use super::*;

    #[test]
    fn validation_tests() {
        insta::with_settings!({prepend_module_to_snapshot => false}, {
            glob!("test_data", "**/*.graphql", |path| {
                let sdl = read_to_string(path).unwrap();
                let name = path.to_str().unwrap();
                // Mirror the production path: parse, expand links, then validate. Expansion is what
                // puts the `@connect`/`@source`/federation definitions, link metadata and
                // referencers in place, and `ConnectorsBlueprint::on_validation` runs on the result.
                //
                // The schema is built here rather than via `Subgraph::parse` so that a failed build
                // still yields the partial schema: connector errors are reported for documents that
                // aren't valid GraphQL, matching the fact that validation runs before
                // `validate_or_return_self`.
                let builder = SchemaBuilder::new()
                    .adopt_orphan_extensions()
                    .parse(&sdl, name);
                let orphan_extension_types = builder.iter_orphan_extension_types().cloned().collect();
                let parsed = builder
                    .build()
                    .unwrap_or_else(|schema_with_errors| schema_with_errors.partial);

                let subgraph = Subgraph::new(name, "http://test", parsed, orphan_extension_types)
                    .expect("valid subgraph name");
                let subgraph = match subgraph.expand_links() {
                    Ok(subgraph) => subgraph,
                    Err(err) => {
                        assert_debug_snapshot!(format!("failed to expand: {err}"));
                        return;
                    }
                };

                let before = subgraph.schema_string();
                let errors = validate(&subgraph);
                let after = subgraph.schema_string();

                assert_debug_snapshot!(errors);
                assert_str_eq!(
                    before, after,
                    "Validations must not modify the schema; rewrites belong in link expansion"
                );
            });
        });
    }
}
