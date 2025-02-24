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
    // Actual number: 128_605.
    const MAX_BYTES_SUPERGRAPH: usize = 135_050; // ~135 KiB. actual number: 128605

    // Total number of allocations with a 5% buffer.
    // Actual number: 4889.
    const MAX_ALLOCATIONS_SUPERGRAPH: u64 = 5_150; // number of allocations. actual number: 4889

    // Number of bytes when the heap size reached its global maximum with a 5% buffer.
    // Actual number: 188_420.
    //
    // API schema generation allocates additional 59_635 bytes (188_420-128_605=59_635).
    const MAX_BYTES_API_SCHEMA: usize = 197_900; // ~200 KiB

    // Total number of allocations with a 5% buffer.
    // Actual number: 5_535.
    //
    // API schema has an additional 646 allocations (5_535-4_889=646).
    const MAX_ALLOCATIONS_API_SCHEMA: u64 = 5_800;

    // Number of bytes when the heap size reached its global maximum with a 5% buffer.
    // Actual number: 552_781.
    //
    // Extract subgraphs allocates additional 364_361 bytes (552_781-188_420=364_361).
    const MAX_BYTES_SUBGRAPHS: usize = 580_420; // ~600 KiB

    // Total number of allocations with a 5% buffer.
    // Actual number: 12_185.
    //
    // Extract subgraphs from supergraph has an additional 6_650 allocations (12_185-5_535=6_650).
    const MAX_ALLOCATIONS_SUBGRAPHS: u64 = 12_800;

    let schema = std::fs::read_to_string(SCHEMA).unwrap();

    let _profiler = dhat::Profiler::builder().testing().build();

    let supergraph =
        apollo_federation::Supergraph::new(&schema).expect("supergraph should be valid");
    let stats = dhat::HeapStats::get();
    dhat::assert!(stats.max_bytes < MAX_BYTES_SUPERGRAPH);
    dhat::assert!(stats.total_blocks < MAX_ALLOCATIONS_SUPERGRAPH);

    let api_options = apollo_federation::ApiSchemaOptions::default();
    let _api_schema = supergraph.to_api_schema(api_options);
    let stats = dhat::HeapStats::get();
    dhat::assert!(stats.max_bytes < MAX_BYTES_API_SCHEMA);
    dhat::assert!(stats.total_blocks < MAX_ALLOCATIONS_API_SCHEMA);

    let _subgraphs = supergraph
        .extract_subgraphs()
        .expect("subgraphs should be extracted");
    let stats = dhat::HeapStats::get();
    dhat::assert!(stats.max_bytes < MAX_BYTES_SUBGRAPHS);
    dhat::assert!(stats.total_blocks < MAX_ALLOCATIONS_SUBGRAPHS);
}
