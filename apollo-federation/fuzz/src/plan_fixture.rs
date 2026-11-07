//! The federated fixture both sides of the plan lane hold.
//!
//! One composed supergraph, checked in beside this package and never generated during a run: the
//! grammar varies the operation and the plan, not the schema. Its subgraphs are in
//! `runners/compose_plan_fixture.rs`, extending those of
//! `it_handles_multiple_requires_within_the_same_entity_fetch`; it stays small enough to transcribe
//! into the Lean oracle by hand.
//!
//! ```text
//! Query.is: [I!]!                          (Subgraph1)
//! interface I @key("id") { id f cs g u h }
//! T1 implements I                          (Subgraph1 only)
//! T2 implements I @key("id")               g @requires("f cs { ... on C { a } }") in Subgraph2,
//!                                          peer: T3 @provides("f") in Subgraph2
//! T3 implements I @key("id")               g @requires("f cs { ... on C { a } }") in Subgraph2
//! union U = T1 | T2 | T3                   Subgraph1 declares only T1 | T2
//! I @interfaceObject { id h }              (Subgraph3)
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
