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

#![deny(
    rustdoc::broken_intra_doc_links,
    unreachable_pub,
    unreachable_patterns,
    unused,
    unused_qualifications,
    dead_code,
    while_true,
    trivial_casts,
    trivial_bounds,
    trivial_numeric_casts,
    unconditional_panic,
    clippy::all
)]

mod api_schema;
mod compat;
mod display_helpers;
pub mod error;
pub mod link;
pub mod merge;
pub(crate) mod operation;
pub mod query_graph;
pub mod query_plan;
pub mod schema;
pub mod subgraph;
pub(crate) mod supergraph;
pub(crate) mod utils;

use apollo_compiler::ast::NamedType;
use apollo_compiler::validation::Valid;
use apollo_compiler::Schema;
use link::join_spec_definition::JOIN_VERSIONS;
use schema::FederationSchema;

pub use crate::api_schema::ApiSchemaOptions;
use crate::error::FederationError;
use crate::error::SingleFederationError;
use crate::link::join_spec_definition::JoinSpecDefinition;
use crate::link::link_spec_definition::LinkSpecDefinition;
use crate::link::spec::Identity;
use crate::link::spec_definition::SpecDefinitions;
use crate::merge::merge_subgraphs;
use crate::merge::MergeFailure;
use crate::schema::ValidFederationSchema;
use crate::subgraph::ValidSubgraph;
pub use crate::supergraph::ValidFederationSubgraph;
pub use crate::supergraph::ValidFederationSubgraphs;

pub(crate) type SupergraphSpecs = (&'static LinkSpecDefinition, &'static JoinSpecDefinition);

pub(crate) fn validate_supergraph_for_query_planning(
    supergraph_schema: &FederationSchema,
) -> Result<SupergraphSpecs, FederationError> {
    validate_supergraph(supergraph_schema, &JOIN_VERSIONS)
}

/// Checks that required supergraph directives are in the schema, and returns which ones were used.
pub(crate) fn validate_supergraph(
    supergraph_schema: &FederationSchema,
    join_versions: &'static SpecDefinitions<JoinSpecDefinition>,
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
    Ok((link_spec_definition, join_spec_definition))
}

pub struct Supergraph {
    pub schema: ValidFederationSchema,
}

impl Supergraph {
    pub fn new(schema_str: &str) -> Result<Self, FederationError> {
        let schema = Schema::parse_and_validate(schema_str, "schema.graphql")?;
        Self::from_schema(schema)
    }

    pub fn from_schema(schema: Valid<Schema>) -> Result<Self, FederationError> {
        let schema = schema.into_inner();
        let schema = FederationSchema::new(schema)?;

        let _ = validate_supergraph_for_query_planning(&schema)?;

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
