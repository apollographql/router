#[global_allocator]
pub(crate) static ALLOC: dhat::Alloc = dhat::Alloc;

// Failure of the test can be diagnosed using the dhat-heap.json file.

// The figures have a 5% buffer from the actual profiling stats. This
// should help us keep an eye on allocation increases, (hopefully) without
// too much flakiness.
#[test]
fn valid_supergraph_schema() {
    const SCHEMA: &str = "../examples/graphql/supergraph.graphql";

    // Number of bytes when the heap size reached its global maximum with a 5% buffer.
    // Actual number: 188_515.
    const MAX_BYTES_SUPERGRAPH: usize = 197_941; // ~193 KiB

    // Total number of allocations with a 5% buffer.
    // Actual number: 5_984.
    const MAX_ALLOCATIONS_SUPERGRAPH: u64 = 6_283;

    // Number of bytes when the heap size reached its global maximum with a 5% buffer.
    // Actual number: 249_019.
    //
    // API schema generation allocates additional 60_504 bytes (249_019-188_515=60_504).
    const MAX_BYTES_API_SCHEMA: usize = 261_470; // ~255 KiB

    // Total number of allocations with a 5% buffer.
    // Actual number: 6_655.
    //
    // API schema has an additional 671 allocations (= 6_655 - 5_984).
    const MAX_ALLOCATIONS_API_SCHEMA: u64 = 6_988;

    // Number of bytes when the heap size reached its global maximum with a 5% buffer.
    // Actual number: 865_032.
    //
    // Extract subgraphs allocates additional 616_013 bytes (865_032-249_019=616_013).
    const MAX_BYTES_SUBGRAPHS: usize = 908_284; // ~887 KiB

    // Total number of allocations with a 5% buffer.
    // Actual number: 14_282.
    //
    // Extract subgraphs from supergraph has an additional 7_627 allocations (= 14_282 - 6_655).
    const MAX_ALLOCATIONS_SUBGRAPHS: u64 = 14_996;

    let schema = std::fs::read_to_string(SCHEMA).unwrap();

    let _profiler = dhat::Profiler::builder().testing().build();

    let supergraph =
        apollo_federation::Supergraph::new(&schema).expect("supergraph should be valid");
    let stats = dhat::HeapStats::get();
    println!("Supergraph::new: {stats:?}");
    dhat::assert!(stats.max_bytes < MAX_BYTES_SUPERGRAPH);
    dhat::assert!(stats.total_blocks < MAX_ALLOCATIONS_SUPERGRAPH);

    let api_options = apollo_federation::ApiSchemaOptions::default();
    let _api_schema = supergraph.to_api_schema(api_options);
    let stats = dhat::HeapStats::get();
    println!("supergraph.to_api_schema: {stats:?}");
    dhat::assert!(stats.max_bytes < MAX_BYTES_API_SCHEMA);
    dhat::assert!(stats.total_blocks < MAX_ALLOCATIONS_API_SCHEMA);

    let _subgraphs = supergraph
        .extract_subgraphs()
        .expect("subgraphs should be extracted");
    let stats = dhat::HeapStats::get();
    println!("supergraph.extract_subgraphs: {stats:?}");
    dhat::assert!(stats.max_bytes < MAX_BYTES_SUBGRAPHS);
    dhat::assert!(stats.total_blocks < MAX_ALLOCATIONS_SUBGRAPHS);
}
