//! ## Usage
//!
//! This crate is internal to [Apollo Router](https://www.apollographql.com/docs/router/)
//! and not intended to be used directly.
//!
//! ## Crate versioning
//!
//! The  `apollo-federation` crate does **not** adhere to [Semantic Versioning](https://semver.org/).
//! Any version may have breaking API changes, as this API is expected to only be used by `apollo-router`.
//! Instead, the version number matches exactly that of the `apollo-router` crate version using it.
//!
//! This version number is **not** that of the Apollo Federation specification being implemented.
//! See [Router documentation](https://www.apollographql.com/docs/router/federation-version-support/)
//! for which Federation versions are supported by which Router versions.

#![warn(
    rustdoc::broken_intra_doc_links,
    unreachable_pub,
    unreachable_patterns,
    unused,
    unused_qualifications,
    dead_code,
    while_true,
    unconditional_panic,
    clippy::all
)]

mod api_schema;
mod compat;
pub mod composition;
pub mod connectors;
#[cfg(feature = "correctness")]
pub mod correctness;
mod display_helpers;
pub mod error;
pub mod link;
pub mod merge;
mod merger;
pub(crate) mod operation;
pub mod query_graph;
pub mod query_plan;
pub mod schema;
pub mod subgraph;
pub mod supergraph;

pub mod utils;

use apollo_compiler::Schema;
use apollo_compiler::ast::NamedType;
use apollo_compiler::collections::HashSet;
use apollo_compiler::validation::Valid;
use itertools::Itertools;
use link::cache_tag_spec_definition::CACHE_TAG_VERSIONS;
use link::join_spec_definition::JOIN_VERSIONS;
use schema::FederationSchema;
use strum::IntoEnumIterator;

pub use crate::api_schema::ApiSchemaOptions;
use crate::connectors::ConnectSpec;
use crate::error::FederationError;
use crate::error::MultiTryAll;
use crate::error::MultipleFederationErrors;
use crate::error::SingleFederationError;
use crate::link::authenticated_spec_definition::AUTHENTICATED_VERSIONS;
use crate::link::cache_invalidation_spec_definition::CACHE_INVALIDATION_VERSIONS;
use crate::link::context_spec_definition::CONTEXT_VERSIONS;
use crate::link::context_spec_definition::ContextSpecDefinition;
use crate::link::cost_spec_definition::COST_VERSIONS;
use crate::link::inaccessible_spec_definition::INACCESSIBLE_VERSIONS;
use crate::link::join_spec_definition::JoinSpecDefinition;
use crate::link::link_spec_definition::CORE_VERSIONS;
use crate::link::link_spec_definition::LinkSpecDefinition;
use crate::link::policy_spec_definition::POLICY_VERSIONS;
use crate::link::requires_scopes_spec_definition::REQUIRES_SCOPES_VERSIONS;
use crate::link::spec::Identity;
use crate::link::spec::Url;
use crate::link::spec::Version;
use crate::link::spec_definition::SpecDefinition;
use crate::link::spec_definition::SpecDefinitions;
use crate::link::tag_spec_definition::TAG_VERSIONS;
use crate::merge::MergeFailure;
use crate::merge::merge_subgraphs;
use crate::schema::ValidFederationSchema;
use crate::subgraph::ValidSubgraph;
pub use crate::supergraph::ValidFederationSubgraph;
pub use crate::supergraph::ValidFederationSubgraphs;

pub mod internal_lsp_api {
    pub use crate::subgraph::schema_diff_expanded_from_initial;
}

/// Internal API for the apollo-composition crate.
pub mod internal_composition_api {
    use super::*;
    use crate::schema::validators::{cache_invalidation, cache_tag};
    use crate::subgraph::typestate;

    #[derive(Default)]
    pub struct ValidationResult {
        /// If `errors` is empty, validation was successful.
        pub errors: Vec<cache_tag::Message>,
    }

    /// Validates `@cacheTag` directives in the original (unexpanded) subgraph schema.
    /// * name: Subgraph name
    /// * url: Subgraph URL
    /// * sdl: Subgraph schema
    /// * Returns a `ValidationResult` if validation finished (either successfully or with
    ///   validation errors).
    /// * Or, a `FederationError` if validation stopped due to an internal error.
    pub fn validate_cache_tag_directives(
        name: &str,
        url: &str,
        sdl: &str,
    ) -> Result<ValidationResult, FederationError> {
        let subgraph =
            typestate::Subgraph::parse(name, url, sdl).map_err(|e| e.into_federation_error())?;
        let subgraph = subgraph
            .expand_links()
            .map_err(|e| e.into_federation_error())?;
        let mut result = ValidationResult::default();
        cache_tag::validate_cache_tag_directives(subgraph.schema(), &mut result.errors)?;
        Ok(result)
    }

    #[derive(Default)]
    pub struct CacheInvalidationValidationResult {
        /// If `errors` is empty, validation was successful.
        pub errors: Vec<cache_invalidation::Message>,
    }
    /// Validates `@cacheInvalidation` directives in the original (unexpanded) subgraph schema.
    /// * name: Subgraph name
    /// * url: Subgraph URL
    /// * sdl: Subgraph schema
    /// * Returns a `CacheInvalidationValidationResult` if validation finished (either successfully or with
    ///   validation errors).
    /// * Or, a `FederationError` if validation stopped due to an internal error.
    pub fn validate_cache_invalidation_directives(
        name: &str,
        url: &str,
        sdl: &str,
    ) -> Result<CacheInvalidationValidationResult, FederationError> {
        let subgraph =
            typestate::Subgraph::parse(name, url, sdl).map_err(|e| e.into_federation_error())?;
        let subgraph = subgraph
            .expand_links()
            .map_err(|e| e.into_federation_error())?;
        let mut result = CacheInvalidationValidationResult::default();
        cache_invalidation::validate_cache_invalidation_directives(
            subgraph.schema(),
            &mut result.errors,
        )?;
        Ok(result)
    }
}

pub(crate) type SupergraphSpecs = (
    &'static LinkSpecDefinition,
    &'static JoinSpecDefinition,
    Option<&'static ContextSpecDefinition>,
);

pub(crate) fn validate_supergraph_for_query_planning(
    supergraph_schema: &FederationSchema,
) -> Result<SupergraphSpecs, FederationError> {
    validate_supergraph(supergraph_schema, &JOIN_VERSIONS, &CONTEXT_VERSIONS)
}

/// Checks that required supergraph directives are in the schema, and returns which ones were used.
pub(crate) fn validate_supergraph(
    supergraph_schema: &FederationSchema,
    join_versions: &'static SpecDefinitions<JoinSpecDefinition>,
    context_versions: &'static SpecDefinitions<ContextSpecDefinition>,
) -> Result<SupergraphSpecs, FederationError> {
    let Some(metadata) = supergraph_schema.metadata() else {
        return Err(SingleFederationError::InvalidFederationSupergraph {
            message: "Invalid supergraph: must be a core schema".to_owned(),
        }
        .into());
    };
    let link_spec_definition = metadata.link_spec_definition()?;
    let Some(join_link) = metadata.for_identity(&Identity::join_identity()) else {
        return Err(SingleFederationError::InvalidFederationSupergraph {
            message: "Invalid supergraph: must use the join spec".to_owned(),
        }
        .into());
    };
    let Some(join_spec_definition) = join_versions.find(&join_link.url.version) else {
        return Err(SingleFederationError::InvalidFederationSupergraph {
            message: format!(
                "Invalid supergraph: uses unsupported join spec version {} (supported versions: {})",
                join_link.url.version,
                join_versions.versions().map(|v| v.to_string()).collect::<Vec<_>>().join(", "),
            ),
        }.into());
    };
    let context_spec_definition = metadata.for_identity(&Identity::context_identity()).map(|context_link| {
        context_versions.find(&context_link.url.version).ok_or_else(|| {
            SingleFederationError::InvalidFederationSupergraph {
                message: format!(
                    "Invalid supergraph: uses unsupported context spec version {} (supported versions: {})",
                    context_link.url.version,
                    context_versions.versions().join(", "),
                ),
            }
        })
    }).transpose()?;
    if let Some(connect_link) = metadata.for_identity(&ConnectSpec::identity()) {
        ConnectSpec::try_from(&connect_link.url.version)
            .map_err(|message| SingleFederationError::UnknownLinkVersion { message })?;
    }
    Ok((
        link_spec_definition,
        join_spec_definition,
        context_spec_definition,
    ))
}

#[derive(Debug)]
pub struct Supergraph {
    pub schema: ValidFederationSchema,
}

impl Supergraph {
    pub fn new_with_spec_check(
        schema_str: &str,
        supported_specs: &[Url],
    ) -> Result<Self, FederationError> {
        let schema = Schema::parse_and_validate(schema_str, "schema.graphql")?;
        Self::from_schema(schema, Some(supported_specs))
    }

    /// Same as `new_with_spec_check(...)` with the default set of supported specs.
    pub fn new(schema_str: &str) -> Result<Self, FederationError> {
        Self::new_with_spec_check(schema_str, &default_supported_supergraph_specs())
    }

    /// Same as `new_with_spec_check(...)` with the specs supported by Router.
    pub fn new_with_router_specs(schema_str: &str) -> Result<Self, FederationError> {
        Self::new_with_spec_check(schema_str, &router_supported_supergraph_specs())
    }

    /// Construct from a pre-validation supergraph schema, which will be validated.
    /// * `supported_specs`: (optional) If provided, checks if all EXECUTION/SECURITY specs are
    ///   supported.
    pub fn from_schema(
        schema: Valid<Schema>,
        supported_specs: Option<&[Url]>,
    ) -> Result<Self, FederationError> {
        let schema: Schema = schema.into_inner();
        let schema = FederationSchema::new(schema)?;

        let _ = validate_supergraph_for_query_planning(&schema)?;

        if let Some(supported_specs) = supported_specs {
            check_spec_support(&schema, supported_specs)?;
        }

        Ok(Self {
            // We know it's valid because the input was.
            schema: schema.assume_valid()?,
        })
    }

    pub fn compose(subgraphs: Vec<&ValidSubgraph>) -> Result<Self, MergeFailure> {
        let schema = merge_subgraphs(subgraphs)?.schema;
        Ok(Self {
            schema: ValidFederationSchema::new(schema).map_err(Into::<MergeFailure>::into)?,
        })
    }

    /// Generates an API Schema from this supergraph schema. The API Schema represents the combined
    /// API of the supergraph that's visible to end users.
    pub fn to_api_schema(
        &self,
        options: ApiSchemaOptions,
    ) -> Result<ValidFederationSchema, FederationError> {
        api_schema::to_api_schema(self.schema.clone(), options)
    }

    pub fn extract_subgraphs(&self) -> Result<ValidFederationSubgraphs, FederationError> {
        supergraph::extract_subgraphs_from_supergraph(&self.schema, None)
    }
}

const _: () = {
    const fn assert_thread_safe<T: Sync + Send>() {}

    assert_thread_safe::<Supergraph>();
    assert_thread_safe::<query_plan::query_planner::QueryPlanner>();
};

/// Returns if the type of the node is a scalar or enum.
pub(crate) fn is_leaf_type(schema: &Schema, ty: &NamedType) -> bool {
    schema.get_scalar(ty).is_some() || schema.get_enum(ty).is_some()
}

pub fn default_supported_supergraph_specs() -> Vec<Url> {
    fn urls(defs: &SpecDefinitions<impl SpecDefinition>) -> impl Iterator<Item = Url> {
        defs.iter().map(|(_, def)| def.url()).cloned()
    }

    urls(&CORE_VERSIONS)
        .chain(urls(&JOIN_VERSIONS))
        .chain(urls(&TAG_VERSIONS))
        .chain(urls(&INACCESSIBLE_VERSIONS))
        .collect()
}

/// default_supported_supergraph_specs() + additional specs supported by Router
pub fn router_supported_supergraph_specs() -> Vec<Url> {
    fn urls(defs: &SpecDefinitions<impl SpecDefinition>) -> impl Iterator<Item = Url> {
        defs.iter().map(|(_, def)| def.url()).cloned()
    }

    // PORT_NOTE: "https://specs.apollo.dev/source/v0.1" is listed in the JS version. But, it is
    //            not ported here, since it has been fully deprecated.
    default_supported_supergraph_specs()
        .into_iter()
        .chain(urls(&AUTHENTICATED_VERSIONS))
        .chain(urls(&REQUIRES_SCOPES_VERSIONS))
        .chain(urls(&POLICY_VERSIONS))
        .chain(urls(&CONTEXT_VERSIONS))
        .chain(urls(&COST_VERSIONS))
        .chain(urls(&CACHE_TAG_VERSIONS))
        .chain(urls(&CACHE_INVALIDATION_VERSIONS))
        .chain(ConnectSpec::iter().map(|s| s.url()))
        .collect()
}

fn is_core_version_zero_dot_one(url: &Url) -> bool {
    CORE_VERSIONS
        .find(&Version { major: 0, minor: 1 })
        .is_some_and(|v| *v.url() == *url)
}

fn check_spec_support(
    schema: &FederationSchema,
    supported_specs: &[Url],
) -> Result<(), FederationError> {
    let Some(metadata) = schema.metadata() else {
        // This can't happen since `validate_supergraph_for_query_planning` already checked.
        bail!("Schema must have metadata");
    };
    let mut errors = MultipleFederationErrors::new();
    let link_spec = metadata.link_spec_definition()?;
    if is_core_version_zero_dot_one(link_spec.url()) {
        let has_link_with_purpose = metadata
            .all_links()
            .iter()
            .any(|link| link.purpose.is_some());
        if has_link_with_purpose {
            // PORT_NOTE: This is unreachable since the schema is validated before this check in
            //            Rust and a apollo-compiler error will have been raised already. This is
            //            still kept for historic reasons and potential fix in the future. However,
            //            it didn't seem worth changing the router's workflow so this specialized
            //            error message can be displayed.
            errors.push(SingleFederationError::UnsupportedLinkedFeature {
                message: format!(
                    "the `for:` argument is unsupported by version {version} of the core spec.\n\
                    Please upgrade to at least @core v0.2 (https://specs.apollo.dev/core/v0.2).",
                    version = link_spec.url().version),
            }.into());
        }
    }

    let supported_specs: HashSet<_> = supported_specs.iter().collect();
    errors
        .and_try(metadata.all_links().iter().try_for_all(|link| {
            let Some(purpose) = link.purpose else {
                return Ok(());
            };
            if !is_core_version_zero_dot_one(&link.url)
                && purpose != link::Purpose::EXECUTION
                && purpose != link::Purpose::SECURITY
            {
                return Ok(());
            }

            let link_url = &link.url;
            if supported_specs.contains(link_url) {
                Ok(())
            } else {
                Err(SingleFederationError::UnsupportedLinkedFeature {
                    message: format!("feature {link_url} is for: {purpose} but is unsupported"),
                }
                .into())
            }
        }))
        .into_result()
}

#[cfg(test)]
mod test_supergraph {
    use pretty_assertions::assert_str_eq;

    use super::*;
    use crate::internal_composition_api::CacheInvalidationValidationResult;
    use crate::internal_composition_api::ValidationResult;
    use crate::internal_composition_api::validate_cache_invalidation_directives;
    use crate::internal_composition_api::validate_cache_tag_directives;

    #[test]
    fn validates_connect_spec_is_known() {
        let res = Supergraph::new(
            r#"
        extend schema @link(url: "https://specs.apollo.dev/connect/v99.99")

        # Required stuff for the supergraph to parse at all, not what we're testing
        extend schema
            @link(url: "https://specs.apollo.dev/link/v1.0")
            @link(url: "https://specs.apollo.dev/join/v0.3", for: EXECUTION)
        directive @link(url: String, as: String, for: link__Purpose, import: [link__Import]) repeatable on SCHEMA
        scalar link__Import
        enum link__Purpose {
          """
          `SECURITY` features provide metadata necessary to securely resolve fields.
          """
          SECURITY

          """
          `EXECUTION` features provide metadata necessary for operation execution.
          """
          EXECUTION
        }
        type Query {required: ID!}
    "#,
        )
        .expect_err("Unknown spec version did not cause error");
        assert_str_eq!(res.to_string(), "Unknown connect version: 99.99");
    }

    #[track_caller]
    fn build_and_validate_cache_tag(name: &str, url: &str, sdl: &str) -> ValidationResult {
        validate_cache_tag_directives(name, url, sdl).unwrap()
    }

    #[test]
    fn it_validates_cache_tag_directives() {
        // Ok with older federation versions without @cacheTag directive.
        let res = build_and_validate_cache_tag(
            "accounts",
            "accounts.graphql",
            r#"
                extend schema
                    @link(
                        url: "https://specs.apollo.dev/federation/v2.11"
                        import: ["@key"]
                    )

                type Query {
                    topProducts(first: Int = 5): [Product]
                }

                type Product
                    @key(fields: "upc")
                    @key(fields: "name") {
                    upc: String!
                    name: String!
                    price: Int
                    weight: Int
                }
            "#,
        );

        assert!(res.errors.is_empty());

        // validation error test
        let res = build_and_validate_cache_tag(
            "accounts",
            "https://accounts",
            r#"
            extend schema
                @link(
                    url: "https://specs.apollo.dev/federation/v2.12"
                    import: ["@key", "@cacheTag"]
                )

            type Query {
                topProducts(first: Int = 5): [Product]
                    @cacheTag(format: "topProducts")
                    @cacheTag(format: "topProducts-{$args.first}")
            }

            type Product
                @key(fields: "upc")
                @key(fields: "name")
                @cacheTag(format: "product-{$key.upc}") {
                upc: String!
                name: String!
                price: Int
                weight: Int
            }
        "#,
        );

        assert_eq!(
            res.errors
                .into_iter()
                .map(|err| err.to_string())
                .collect::<Vec<String>>(),
            vec!["Each entity field referenced in a @cacheTag format (applied on entity type) must be a member of every @key field set. In other words, when there are multiple @key fields on the type, the referenced field(s) must be limited to their intersection. Bad cacheTag format \"product-{$key.upc}\" on type \"Product\"".to_string()]
        );

        // valid usage test
        let res = build_and_validate_cache_tag(
            "accounts",
            "accounts.graphql",
            r#"
                    extend schema
                    @link(
                        url: "https://specs.apollo.dev/federation/v2.12"
                        import: ["@key", "@cacheTag"]
                    )

                type Query {
                    topProducts(first: Int = 5): [Product]
                        @cacheTag(format: "topProducts")
                        @cacheTag(format: "topProducts-{$args.first}")
                }

                type Product
                    @key(fields: "upc")
                    @cacheTag(format: "product-{$key.upc}") {
                    upc: String!
                    name: String!
                    price: Int
                    weight: Int
                }
            "#,
        );

        assert!(res.errors.is_empty());
    }

    #[track_caller]
    fn build_and_validate_cache_invalidation(
        name: &str,
        url: &str,
        sdl: &str,
    ) -> CacheInvalidationValidationResult {
        validate_cache_invalidation_directives(name, url, sdl).unwrap()
    }

    #[test]
    fn it_validates_cache_invalidation_directives() {
        // Ok with older federation versions without @cacheTag directive.
        let res = build_and_validate_cache_invalidation(
            "accounts",
            "accounts.graphql",
            r#"
                extend schema
                    @link(
                        url: "https://specs.apollo.dev/federation/v2.11"
                        import: ["@key"]
                    )

                type Query {
                    topProducts(first: Int = 5): [Product]
                }

                type Product
                    @key(fields: "upc")
                    @key(fields: "name") {
                    upc: String!
                    name: String!
                    price: Int
                    weight: Int
                }
            "#,
        );

        assert!(res.errors.is_empty());

        // validation error test
        let res = build_and_validate_cache_invalidation(
            "accounts",
            "https://accounts",
            r#"
            extend schema
                @link(
                    url: "https://specs.apollo.dev/federation/v2.12"
                    import: ["@key", "@cacheInvalidation"]
                )

            type Mutation {
                updateProduct(productUpc: String): User @cacheInvalidation(cacheTag: "product-{$args.productUpc}")
            }

            type Product
                @key(fields: "upc")
                @key(fields: "name")
                @cacheTag(format: "product-{$key.upc}") {
                upc: String!
                name: String!
                price: Int
                weight: Int
            }
        "#,
        );

        assert_eq!(
            res.errors
                .into_iter()
                .map(|err| err.to_string())
                .collect::<Vec<String>>(),
            vec!["@cacheInvalidation can only use non nullable argument but \"productUpc\" in cacheTag is nullable".to_string()]
        );

        // valid usage test
        let res = build_and_validate_cache_invalidation(
            "accounts",
            "accounts.graphql",
            r#"
                extend schema
                @link(
                    url: "https://specs.apollo.dev/federation/v2.12"
                    import: ["@key", "@cacheInvalidation"]
                )

                type Mutation {
                    updateProduct(productUpc: String!): User @cacheInvalidation(cacheTag: "product-{$args.productUpc}") @cacheInvalidation(cacheTag: "product")
                }

                type Product
                    @key(fields: "upc")
                    @cacheTag(format: "product-{$key.upc}") {
                    upc: String!
                    name: String!
                    price: Int
                    weight: Int
                }
            "#,
        );

        assert!(res.errors.is_empty());
    }
}
