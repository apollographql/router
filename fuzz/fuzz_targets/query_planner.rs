//! Fuzz target for the apollo-federation query planner.
//!
//! Generates arbitrary valid GraphQL operations against the planner's own API
//! schema (derived from `fuzz/subgraph/supergraph.graphql`), then asks the
//! federation query planner to plan them. Any panic inside the planner is a
//! finding; `Err` results are expected for adversarial inputs and are ignored.
//!
//! The schema apollo-smith generates from is taken from the planner itself so
//! that whatever the planner accepts — including `@defer` when enabled in the
//! config — is also in scope for the generator.
#![no_main]

use std::sync::OnceLock;

use apollo_compiler::ExecutableDocument;
use apollo_federation::query_plan::query_planner::QueryPlanIncrementalDeliveryConfig;
use apollo_federation::query_plan::query_planner::QueryPlanOptions;
use apollo_federation::query_plan::query_planner::QueryPlanner;
use apollo_federation::query_plan::query_planner::QueryPlannerConfig;
use apollo_federation::Supergraph;
use libfuzzer_sys::fuzz_target;
use router_fuzz::generate_valid_operation_from_schema;

const SUPERGRAPH_SCHEMA: &str = include_str!("../subgraph/supergraph.graphql");

fn planner_config() -> QueryPlannerConfig {
    QueryPlannerConfig {
        generate_query_fragments: true, // Router defaults to true
        incremental_delivery: QueryPlanIncrementalDeliveryConfig {
            enable_defer: true, // Router defaults to true
        },
        type_conditioned_fetching: false, // Router defaults to false
        ..Default::default()
    }
}

fn planner() -> &'static QueryPlanner {
    static PLANNER: OnceLock<QueryPlanner> = OnceLock::new();
    PLANNER.get_or_init(|| {
        let supergraph =
            Supergraph::new(SUPERGRAPH_SCHEMA).expect("fuzz supergraph schema should be valid");
        QueryPlanner::new(&supergraph, planner_config())
            .expect("query planner should build from fuzz supergraph")
    })
}

/// SDL of the planner's API schema, conditioned for apollo-smith.
///
/// Two adjustments are needed on top of `Schema::serialize()`:
///
/// 1. **Schema definition block.** apollo-compiler's `serialize()` omits the
///    `schema { ... }` block when root operation type names match the defaults,
///    but apollo-parser (used by apollo-smith) needs an explicit block to
///    discover the root types — without it, `operation_definition()` returns
///    `Ok(None)` for every input. We synthesize one from `schema_definition`.
///
/// 2. **Standard `@skip` / `@include` declarations.** apollo-smith 0.15.2 picks
///    `num_directives` for each location from `0..=(directive_defs.len() - 1)`
///    (see `directive.rs:146`). If only `@defer` is declared, that range is
///    `0..=0` and smith *always* picks zero — so `@defer` would never appear.
///    apollo-compiler omits `@skip` / `@include` from `serialize()` since they
///    are built-in, so we add them back here. With three directives declared,
///    smith samples `@defer` on roughly one in three eligible locations.
fn api_schema_sdl() -> &'static str {
    static SDL: OnceLock<String> = OnceLock::new();
    SDL.get_or_init(|| {
        let schema = planner().api_schema().schema();
        let def = &schema.schema_definition;
        let mut sdl = String::from("schema {\n");
        if let Some(q) = &def.query {
            sdl.push_str(&format!("  query: {}\n", q.name));
        }
        if let Some(m) = &def.mutation {
            sdl.push_str(&format!("  mutation: {}\n", m.name));
        }
        if let Some(s) = &def.subscription {
            sdl.push_str(&format!("  subscription: {}\n", s.name));
        }
        sdl.push_str("}\n\n");
        // Built-in selection-set directives, restored so smith picks more than zero
        // directives per location (see doc comment for the off-by-one explanation).
        sdl.push_str(
            "directive @skip(if: Boolean!) on FIELD | FRAGMENT_SPREAD | INLINE_FRAGMENT\n\
             directive @include(if: Boolean!) on FIELD | FRAGMENT_SPREAD | INLINE_FRAGMENT\n\n",
        );
        sdl.push_str(&schema.serialize().to_string());
        sdl
    })
}

fuzz_target!(|data: &[u8]| {
    let operation = match generate_valid_operation_from_schema(data, api_schema_sdl()) {
        Ok((op, _)) => op,
        Err(_) => return,
    };

    let planner = planner();
    let document = match ExecutableDocument::parse_and_validate(
        planner.api_schema().schema(),
        &operation,
        "fuzz.graphql",
    ) {
        Ok(doc) => doc,
        Err(_) => return,
    };

    let _ = planner.build_query_plan(&document, None, QueryPlanOptions::default());
});
