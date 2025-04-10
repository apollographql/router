#[global_allocator]
pub(crate) static ALLOC: dhat::Alloc = dhat::Alloc;

// Failure of the test can be diagnosed using the dhat-heap.json file.

// These values should be kept slightly larger (~10%) than the current heap usage to catch
// significant increases.
#[test]
fn valid_large_body() {
    const SCHEMA: &str = "src/sources/connect/validation/test_data/valid_large_body.graphql";

    // Both of these values are rounded up to the next hundred to provide a
    // small amount of wiggle room.
    const MAX_BYTES: usize = 283_500; // 276.8 KiB
    const MAX_ALLOCATIONS: u64 = 23_700;

    let schema = std::fs::read_to_string(SCHEMA).unwrap();

    let _profiler = dhat::Profiler::builder().testing().build();

    apollo_federation::sources::connect::validation::validate(&schema, SCHEMA);

    let stats = dhat::HeapStats::get();
    dhat::assert!(
        stats.max_bytes < MAX_BYTES,
        "{} < {}",
        stats.max_bytes,
        MAX_BYTES
    );
    dhat::assert!(
        stats.total_blocks < MAX_ALLOCATIONS,
        "{} < {}",
        stats.total_blocks,
        MAX_ALLOCATIONS
    );
}
