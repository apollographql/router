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
    // Actual number: 181_885.
    const MAX_BYTES_SUPERGRAPH: usize = 190_980; // ~187 KiB

    // Total number of allocations with a 5% buffer.
    // Actual number: 5_910.
    const MAX_ALLOCATIONS_SUPERGRAPH: u64 = 6_206;

    // Number of bytes when the heap size reached its global maximum with a 5% buffer.
    // Actual number: 241_136.
    //
    // API schema generation allocates additional 59_251 bytes (241_136-181_885=59_251).
    const MAX_BYTES_API_SCHEMA: usize = 253_193; // ~247 KiB

    // Total number of allocations with a 5% buffer.
    // Actual number: 6_569.
    //
    // API schema has an additional 659 allocations (= 6_569 - 5_910).
    const MAX_ALLOCATIONS_API_SCHEMA: u64 = 6_898;

    // Number of bytes when the heap size reached its global maximum with a 5% buffer.
    // Actual number: 686_993.
    //
    // Extract subgraphs allocates additional 445_857 bytes (686_993-241_136=445_857).
    const MAX_BYTES_SUBGRAPHS: usize = 721_342; // ~679 KiB

    // Total number of allocations with a 5% buffer.
    // Actual number: 14_740.
    //
    // Extract subgraphs from supergraph has an additional 8_171 allocations (= 14_740 - 6_569).
    const MAX_ALLOCATIONS_SUBGRAPHS: u64 = 15_477;

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
