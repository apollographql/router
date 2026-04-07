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
    // Actual number: 136_317.
    const MAX_BYTES_SUPERGRAPH: usize = 143_132; // ~143 KiB. actual number: 136317

    // Total number of allocations with a 5% buffer.
    // Actual number: 4952.
    const MAX_ALLOCATIONS_SUPERGRAPH: u64 = 5_200; // number of allocations.

    // Number of bytes when the heap size reached its global maximum with a 5% buffer.
    // Actual number: 195_884.
    //
    // API schema generation allocates additional 59_567 bytes (195_884-136_317=59_567).
    const MAX_BYTES_API_SCHEMA: usize = 205_678; // ~206 KiB

    // Total number of allocations with a 5% buffer.
    // Actual number: 5507.
    //
    // API schema has an additional 555 allocations (= 5507 - 4952).
    const MAX_ALLOCATIONS_API_SCHEMA: u64 = 5782;

    // Number of bytes when the heap size reached its global maximum with a 5% buffer.
    // Actual number: 570_253.
    //
    // Extract subgraphs allocates additional 384_369 bytes (570_253-195_884=384_369).
    const MAX_BYTES_SUBGRAPHS: usize = 598_766; // ~600 KiB

    // Total number of allocations with a 5% buffer.
    // Actual number: 13162.
    //
    // Extract subgraphs from supergraph has an additional 7655 allocations (= 13162 - 5507).
    const MAX_ALLOCATIONS_SUBGRAPHS: u64 = 13820;

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
