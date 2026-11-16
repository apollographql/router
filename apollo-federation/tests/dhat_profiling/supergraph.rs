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
    // Actual number: 5_978.
    const MAX_ALLOCATIONS_SUPERGRAPH: u64 = 6_277;

    // Number of bytes when the heap size reached its global maximum with a 5% buffer.
    // Actual number: 249_019.
    //
    // API schema generation allocates additional 60_504 bytes (249_019-188_515=60_504).
    const MAX_BYTES_API_SCHEMA: usize = 261_470; // ~255 KiB

    // Total number of allocations with a 5% buffer.
    // Actual number: 6_649.
    //
    // API schema has an additional 671 allocations (= 6_649 - 5_978).
    const MAX_ALLOCATIONS_API_SCHEMA: u64 = 6_981;

    // Number of bytes when the heap size reached its global maximum with a 5% buffer.
    // Actual number: 861_960.
    //
    // Extract subgraphs allocates additional 612_941 bytes (861_960-249_019=612_941).
    const MAX_BYTES_SUBGRAPHS: usize = 905_058; // ~884 KiB

    // Total number of allocations with a 5% buffer.
    // Actual number: 14_220.
    //
    // Extract subgraphs from supergraph has an additional 7_571 allocations (= 14_220 - 6_649).
    const MAX_ALLOCATIONS_SUBGRAPHS: u64 = 14_931;

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
