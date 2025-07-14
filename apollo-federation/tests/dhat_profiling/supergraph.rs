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
    // Actual number: 159_855.
    const MAX_BYTES_SUPERGRAPH: usize = 167_848; // ~168 KiB

    // Total number of allocations with a 5% buffer.
    // Actual number: 5168.
    const MAX_ALLOCATIONS_SUPERGRAPH: u64 = 5_246; // number of allocations.

    // Number of bytes when the heap size reached its global maximum with a 5% buffer.
    // Actual number: 218_998.
    //
    // API schema generation allocates additional 59_143 bytes (218_998-159_855=59_143).
    const MAX_BYTES_API_SCHEMA: usize = 229_948; // ~230 KiB

    // Total number of allocations with a 5% buffer.
    // Actual number: 5817.
    //
    // API schema has an additional 649 allocations (= 5817 - 5168).
    const MAX_ALLOCATIONS_API_SCHEMA: u64 = 6108;

    // Number of bytes when the heap size reached its global maximum with a 5% buffer.
    // Actual number: 641_696.
    //
    // Extract subgraphs allocates additional 422_698 bytes (= 641_696 - 218_998).
    const MAX_BYTES_SUBGRAPHS: usize = 673_781; // ~674 KiB

    // Total number of allocations with a 5% buffer.
    // Actual number: 13488.
    //
    // Extract subgraphs from supergraph has an additional 7671 allocations (= 13488 - 5817).
    const MAX_ALLOCATIONS_SUBGRAPHS: u64 = 14162;

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
