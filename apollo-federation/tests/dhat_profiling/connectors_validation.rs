#[global_allocator]
pub(crate) static ALLOC: dhat::Alloc = dhat::Alloc;

// Failure of the test can be diagnosed using the dhat-heap.json file.

// These values should be kept slightly larger (~10%) than the current heap usage to catch
// significant increases.
#[test]
fn valid_large_body() {
    const SCHEMA: &str = "src/connectors/validation/test_data/valid_large_body.graphql";

    const MAX_BYTES: usize = 236_000;
    // `compute_output_shape` records into a `SelectionTrie` on every
    // recursive step, so allocation count is dominated by that bookkeeping.
    // Lowered from 64_000 once `SelectionTrie::add_name` stopped collecting
    // `Name::locations()` for every name segment.
    const MAX_ALLOCATIONS: u64 = 51_000;

    let schema = std::fs::read_to_string(SCHEMA).unwrap();

    let _profiler = dhat::Profiler::builder().testing().build();

    apollo_federation::connectors::validation::validate(schema, SCHEMA);

    let stats = dhat::HeapStats::get();
    dhat::assert!(
        stats.max_bytes < MAX_BYTES,
        "{} > {}",
        stats.max_bytes,
        MAX_BYTES
    );
    dhat::assert!(
        stats.total_blocks < MAX_ALLOCATIONS,
        "{} > {}",
        stats.total_blocks,
        MAX_ALLOCATIONS
    );
}
