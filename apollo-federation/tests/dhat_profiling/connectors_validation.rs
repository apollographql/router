#[global_allocator]
pub(crate) static ALLOC: dhat::Alloc = dhat::Alloc;

// Failure of the test can be diagnosed using the dhat-heap.json file.

// These values should be kept slightly larger (~10%) than the current heap usage to catch
// significant increases.
#[test]
fn valid_large_body() {
    const SCHEMA: &str = "src/connectors/validation/test_data/valid_large_body.graphql";

    // Bumped from 275_000 with apollo-shape 0.2.0, which gives GraphQL types the
    // shapes their schemas describe: a built-in scalar field is now a concrete
    // shape rather than a shared `Unknown`, a nullable one is a two-member `One`,
    // and nested lists keep every level. Peak live bytes grew 15.7% (269,550 ->
    // 311,925) while total allocation churn moved 1.1% (3,923,053 -> 3,966,885),
    // so this is more shape held live at once rather than more allocation
    // traffic. Note also that 275_000 sat only 2% above the measured peak,
    // tighter than the ~10% this file asks for; 345_000 restores that margin.
    const MAX_BYTES: usize = 345_000;
    // Bumped from 27_000 once the fused-trie consumption infrastructure
    // landed: `compute_output_shape` now records into a `SelectionTrie`
    // baton on every recursive step, which roughly doubles allocation
    // count during connector validation. The total bytes are unchanged —
    // only block count grew, dominated by short-lived `Vec`s that hold
    // per-segment `Name::locations()` slices in `SelectionTrie::add_name`.
    const MAX_ALLOCATIONS: u64 = 64_000;

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
