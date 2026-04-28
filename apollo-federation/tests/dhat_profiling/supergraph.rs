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
    // Actual number: 155_862.
    const MAX_BYTES_SUPERGRAPH: usize = 163_655; // ~160 KiB

    // Total number of allocations with a 5% buffer.
    // Actual number: 5_136.
    const MAX_ALLOCATIONS_SUPERGRAPH: u64 = 5_393;

    // Number of bytes when the heap size reached its global maximum with a 5% buffer.
    // Actual number: 215_189.
    //
    // API schema generation allocates additional 59_327 bytes (215_189-155_862=59_327).
    const MAX_BYTES_API_SCHEMA: usize = 225_948; // ~221 KiB

    // Total number of allocations with a 5% buffer.
    // Actual number: 5_702.
    //
    // API schema has an additional 566 allocations (= 5_702 - 5_136).
    const MAX_ALLOCATIONS_API_SCHEMA: u64 = 5_987;

    // Number of bytes when the heap size reached its global maximum with a 5% buffer.
    // Actual number: 641_583.
    //
    // Extract subgraphs allocates additional 426_394 bytes (641_583-215_189=426_394).
    const MAX_BYTES_SUBGRAPHS: usize = 673_662; // ~658 KiB

    // Total number of allocations with a 5% buffer.
    // Actual number: 11_989.
    //
    // Extract subgraphs from supergraph has an additional 6_287 allocations (= 11_989 - 5_702).
    const MAX_ALLOCATIONS_SUBGRAPHS: u64 = 12_588;

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
