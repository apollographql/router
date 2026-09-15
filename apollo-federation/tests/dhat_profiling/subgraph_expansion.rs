#[global_allocator]
pub(crate) static ALLOC: dhat::Alloc = dhat::Alloc;

// Failure of the test can be diagnosed using the dhat-heap.json file.

// These values should be kept slightly larger (~10%) than the current heap usage to catch
// significant increases.

/// Link expansion on its own, with validation deliberately *not* run.
///
/// This is the companion to `connectors_validation_profiling`, which measures the same subgraph
/// through the same path *plus* `validate()`. Keeping them separate makes a regression
/// attributable: if only that test moves, the cost is in validation; if both move by the same
/// amount, it is in expansion. The difference between the two is the validation cost, currently
/// ~188k (~380k there against ~192k here), essentially all of it connectors validation.
///
/// The same fixture is used in both so the numbers are directly comparable — do not change it in
/// one without the other.
#[test]
fn expands_large_connectors_subgraph() {
    const SCHEMA: &str = "src/connectors/validation/test_data/valid_large_body.graphql";

    // Measured ~192k: roughly ~53k parsing and ~139k expanding.
    const MAX_BYTES: usize = 215_000;
    const MAX_ALLOCATIONS: u64 = 4_400;

    let sdl = std::fs::read_to_string(SCHEMA).unwrap();

    // See the note in `connectors_validation.rs`: expansion forces one-time `LazyLock` init that
    // would otherwise be charged to whichever measurement runs first.
    warm_up_lazy_statics();

    let _profiler = dhat::Profiler::builder().testing().build();

    apollo_federation::subgraph::typestate::Subgraph::parse(SCHEMA, "http://test", &sdl)
        .unwrap()
        .expand_links()
        .unwrap();

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

/// Runs a trivial subgraph through the same path, so process-wide one-time allocations are already
/// paid before the measurement starts.
fn warm_up_lazy_statics() {
    const TRIVIAL: &str = r#"
        extend schema
          @link(url: "https://specs.apollo.dev/federation/v2.10", import: ["@key"])

        type Query {
          hello: String
        }
    "#;

    apollo_federation::subgraph::typestate::Subgraph::parse("warmup", "http://warmup", TRIVIAL)
        .unwrap()
        .expand_links()
        .unwrap();
}
