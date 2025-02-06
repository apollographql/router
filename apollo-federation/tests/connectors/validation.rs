#[global_allocator]
pub(crate) static ALLOC: dhat::Alloc = dhat::Alloc;

#[test]
fn valid_large_body() {
    const SCHEMA: &str = "src/sources/connect/validation/test_data/valid_large_body.graphql";
    const MAX_BYTES: usize = 204_800; // 200 KiB

    let schema = std::fs::read_to_string(SCHEMA).unwrap();

    let _profiler = dhat::Profiler::builder().testing().build();

    apollo_federation::sources::connect::validation::validate(&schema, SCHEMA);

    let stats = dhat::HeapStats::get();
    dhat::assert!(stats.max_bytes < MAX_BYTES);
}
