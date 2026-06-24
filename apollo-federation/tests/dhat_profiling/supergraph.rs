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
    // Actual number: 166_028.
    const MAX_BYTES_SUPERGRAPH: usize = 174_330; // ~171 KiB

    // Total number of allocations with a 5% buffer.
    // Actual number: 5_400.
    const MAX_ALLOCATIONS_SUPERGRAPH: u64 = 5_670;

    // Number of bytes when the heap size reached its global maximum with a 5% buffer.
    // Actual number: 225_691.
    //
    // API schema generation allocates additional 59_567 bytes (225_691-166_028=59_663).
    const MAX_BYTES_API_SCHEMA: usize = 236_976; // ~232 KiB

    // Total number of allocations with a 5% buffer.
    // Actual number: 6_019.
    //
    // API schema has an additional 619 allocations (= 6_019 - 5_400).
    const MAX_ALLOCATIONS_API_SCHEMA: u64 = 6_320;

    // Number of bytes when the heap size reached its global maximum with a 5% buffer.
    // Actual number: 661_387.
    //
    // Extract subgraphs allocates additional 416_238 bytes (661_387-225_691=435_696).
    const MAX_BYTES_SUBGRAPHS: usize = 694_457; // ~679 KiB

    // Total number of allocations with a 5% buffer.
    // Actual number: 12_371.
    //
    // Extract subgraphs from supergraph has an additional 6_352 allocations (= 12_371 - 6_019).
    const MAX_ALLOCATIONS_SUBGRAPHS: u64 = 12_990;

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
