// Parametric federation-only startup heap profiling for the connectors-startup-memory
// investigation (branch smyrick/6817694b). Mirrors the router startup path
// (apollo-router/src/spec/schema.rs: expand_connectors -> build planner) but WITHOUT the
// full router (no tokio/http/telemetry), so it isolates the federation-side allocations
// (connector expansion + federated query-graph build) from total router RSS.
//
// Env-gated + #[ignore] so it never runs in normal `cargo test`. Run explicitly:
//   CONNECTORS_SUPERGRAPH=.../supergraph.graphql \
//     cargo test -p apollo-federation --test connectors_startup_profiling -- --ignored --nocapture
//
// Prints one machine-parseable line:
//   CONNECTORS_DHAT max_bytes=<peak heap B> total_bytes=<B> total_blocks=<allocs> curr_bytes=<B>

#[global_allocator]
pub(crate) static ALLOC: dhat::Alloc = dhat::Alloc;

#[test]
#[ignore]
fn connectors_startup_profile() {
    let path = match std::env::var("CONNECTORS_SUPERGRAPH") {
        Ok(p) => p,
        Err(_) => {
            eprintln!("CONNECTORS_SUPERGRAPH unset; skipping");
            return;
        }
    };
    let sdl = std::fs::read_to_string(&path).expect("read supergraph sdl");

    let _profiler = dhat::Profiler::builder().testing().build();

    // 1. Expand connectors exactly as the router does at startup.
    let api_options = apollo_federation::ApiSchemaOptions::default();
    let expanded = apollo_federation::connectors::expand::expand_connectors(&sdl, &api_options)
        .expect("expand_connectors");
    let raw_sdl = match expanded {
        apollo_federation::connectors::expand::ExpansionResult::Expanded { raw_sdl, .. } => raw_sdl,
        apollo_federation::connectors::expand::ExpansionResult::Unchanged => sdl,
    };
    let after_expand = dhat::HeapStats::get();
    eprintln!(
        "after expand_connectors: max_bytes={} total_bytes={} total_blocks={}",
        after_expand.max_bytes, after_expand.total_bytes, after_expand.total_blocks
    );

    // 2. Build the federated query planner (builds the federated query graph -> the
    //    handle_key / copy_subgraphs / api_schema.clone hot spots).
    // Use new_with_router_specs (as the router does): the expanded SDL retains the
    // connect/router EXECUTION link that plain Supergraph::new rejects.
    let supergraph = apollo_federation::Supergraph::new_with_router_specs(&raw_sdl)
        .expect("expanded supergraph should be valid");
    let qp_config = apollo_federation::query_plan::query_planner::QueryPlannerConfig::default();
    let _planner =
        apollo_federation::query_plan::query_planner::QueryPlanner::new(&supergraph, qp_config)
            .expect("query planner should be created");

    let stats = dhat::HeapStats::get();
    println!(
        "CONNECTORS_DHAT max_bytes={} total_bytes={} total_blocks={} curr_bytes={}",
        stats.max_bytes, stats.total_bytes, stats.total_blocks, stats.curr_bytes
    );
}
