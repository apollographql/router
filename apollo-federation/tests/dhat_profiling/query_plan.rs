#[global_allocator]
pub(crate) static ALLOC: dhat::Alloc = dhat::Alloc;

// Failure of the test can be diagnosed using the dhat-heap.json file.

// The figures have a 5% buffer from the actual profiling stats. This
// should help us keep an eye on allocation increases, (hopefully) without
// too much flakiness.
#[test]
fn valid_query_plan() {
    const SCHEMA: &str = "../examples/graphql/supergraph.graphql";
    const OPERATION: &str = "query fetchUser {
      me {
        id
        name
        username
        reviews {
          ...reviews
        }
      }
      recommendedProducts {
        ...products
      } 
      topProducts {
        ...products
      }
    }
    fragment products on Product {
        upc
        weight
        price
        shippingEstimate
        reviews {
          ...reviews
        }
    }
    fragment reviews on Review {
      id
      author {
        id
        name
      }
    }
    ";

    // Number of bytes when the heap size reached its global maximum with a 5% buffer.
    // Actual number: 843_967.
    const MAX_BYTES_QUERY_PLANNER: usize = 886_165; // ~865 KiB

    // Total number of allocations with a 5% buffer.
    // Actual number: 15_939.
    const MAX_ALLOCATIONS_QUERY_PLANNER: u64 = 16_736;

    // Number of bytes when the heap size reached its global maximum with a 5% buffer.
    // Actual number: 975_837.
    //
    // Planning adds 131_870 bytes to heap max (975_837-843_967=131_870).
    const MAX_BYTES_QUERY_PLAN: usize = 1_024_629; // ~1000 KiB

    // Total number of allocations with a 5% buffer.
    // Actual number: 23_350.
    //
    // Planning adds 7_411 allocations (23_350-15_939=7_411).
    const MAX_ALLOCATIONS_QUERY_PLAN: u64 = 24_518;

    let schema = std::fs::read_to_string(SCHEMA).unwrap();

    let _profiler = dhat::Profiler::builder().testing().build();

    let supergraph =
        apollo_federation::Supergraph::new(&schema).expect("supergraph should be valid");
    let api_options = apollo_federation::ApiSchemaOptions::default();
    let api_schema = supergraph
        .to_api_schema(api_options)
        .expect("api schema should be valid");
    let qp_config = apollo_federation::query_plan::query_planner::QueryPlannerConfig::default();
    let planner =
        apollo_federation::query_plan::query_planner::QueryPlanner::new(&supergraph, qp_config)
            .expect("query planner should be created");
    let stats = dhat::HeapStats::get();
    println!("QueryPlanner::new: {stats:?}");
    dhat::assert!(stats.max_bytes < MAX_BYTES_QUERY_PLANNER);
    dhat::assert!(stats.total_blocks < MAX_ALLOCATIONS_QUERY_PLANNER);
    let document = apollo_compiler::ExecutableDocument::parse_and_validate(
        api_schema.schema(),
        OPERATION,
        "operation.graphql",
    )
    .expect("operation should be valid");
    let qp_options = apollo_federation::query_plan::query_planner::QueryPlanOptions::default();
    planner
        .build_query_plan(&document, None, qp_options)
        .expect("valid query plan");
    let stats = dhat::HeapStats::get();
    println!("QueryPlanner::build_query_plan: {stats:?}");
    dhat::assert!(stats.max_bytes < MAX_BYTES_QUERY_PLAN);
    dhat::assert!(stats.total_blocks < MAX_ALLOCATIONS_QUERY_PLAN);
}
