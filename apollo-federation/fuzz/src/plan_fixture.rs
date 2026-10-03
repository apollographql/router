//! The federated fixture both sides of the plan lane hold.
//!
//! One composed supergraph, checked in beside this package and never generated: the grammar varies
//! the operation and the plan, not the schema. It is the supergraph of
//! `it_handles_multiple_requires_within_the_same_entity_fetch`, chosen because it is small enough
//! to transcribe into the Lean oracle by hand and still exercises what the checker decides —
//! an interface root under a list, two entity types with `@key(fields: "id")`, a `@requires`
//! field on each, and a third implementation with no key at all.
//!
//! ```text
//! Query.is: [I!]!                          (Subgraph1)
//! interface I { id: ID!, f: Int, g: Int }
//! T1 implements I { id, f, g }             (Subgraph1 only, no key)
//! T2 implements I @key("id")               f: Int! in Subgraph1, g @requires("f") in Subgraph2
//! T3 implements I @key("id")               f: Int  in Subgraph1, g @requires("f") in Subgraph2
//! ```
//!
//! Holding a real [`QueryPlanner`] rather than only the schemas buys the planner-driven lane: an
//! operation can be planned for real and the result handed to both checkers, which is the only
//! source of plans that are correct by construction.

use std::sync::Arc;

use apollo_compiler::collections::IndexMap;
use apollo_federation::query_plan::query_planner::QueryPlanner;
use apollo_federation::schema::ValidFederationSchema;
use apollo_federation::ApiSchemaOptions;
use apollo_federation::Supergraph;

/// The composed supergraph, as the query-plan test suite caches it.
pub const SUPERGRAPH_SDL: &str = include_str!("../fixtures/supergraph.graphql");

pub struct Fixture {
    planner: QueryPlanner,
    api_schema: ValidFederationSchema,
}

impl Fixture {
    pub fn new() -> Self {
        let supergraph = Supergraph::new_with_router_specs(SUPERGRAPH_SDL).expect("valid fixture");
        let api_schema = supergraph
            .to_api_schema(ApiSchemaOptions::default())
            .expect("api schema");
        let planner =
            QueryPlanner::new(&supergraph, Default::default()).expect("planner for fixture");
        Fixture {
            planner,
            api_schema,
        }
    }

    pub fn api_schema(&self) -> &ValidFederationSchema {
        &self.api_schema
    }

    pub fn supergraph_schema(&self) -> &ValidFederationSchema {
        self.planner.supergraph_schema()
    }

    pub fn subgraph_schemas(&self) -> &IndexMap<Arc<str>, ValidFederationSchema> {
        self.planner.subgraph_schemas()
    }

    pub fn planner(&self) -> &QueryPlanner {
        &self.planner
    }
}

impl Default for Fixture {
    fn default() -> Self {
        Self::new()
    }
}
