#[global_allocator]
pub(crate) static ALLOC: dhat::Alloc = dhat::Alloc;

// Failure of the test can be diagnosed using the dhat-heap.json file.

// These values should be kept slightly larger (~10%) than the current heap usage to catch
// significant increases.
#[test]
fn valid_large_body() {
    const SCHEMA: &str = "src/connectors/validation/test_data/valid_large_body.graphql";

    // Bumped from 275_000 when connectors validation moved to run mid-expansion
    // (`ConnectorsBlueprint::on_validation`), which put link expansion inside the profiled region.
    //
    // Measured ~380k, made up of ~192k for parsing and expansion (see
    // `subgraph_expansion_profiling`, which measures exactly that half against the same fixture)
    // and ~188k for validation. If this test regresses, check that one first: if it moved too, the
    // cost is in expansion, not here.
    //
    // `validate()` covers connectors validation *and* GraphQL/federation validation, which cannot
    // be measured apart now that they share a transition. Connectors validation dominates: adding
    // the GraphQL half moved the peak by well under 1k.
    const MAX_BYTES: usize = 420_000;
    // Bumped from 27_000 once the fused-trie consumption infrastructure
    // landed: `compute_output_shape` now records into a `SelectionTrie`
    // baton on every recursive step, which roughly doubles allocation
    // count during connector validation. The total bytes are unchanged —
    // only block count grew, dominated by short-lived `Vec`s that hold
    // per-segment `Name::locations()` slices in `SelectionTrie::add_name`.
    const MAX_ALLOCATIONS: u64 = 66_000;

    let sdl = std::fs::read_to_string(SCHEMA).unwrap();

    // Expansion forces one-time `LazyLock` initialization (`SPEC_REGISTRY` and every `*_VERSIONS`),
    // which costs ~70k on whichever code path touches it first. Pay that outside the profiled
    // region so this test measures steady-state per-subgraph cost rather than process startup.
    warm_up_lazy_statics();

    let _profiler = dhat::Profiler::builder().testing().build();

    // Profiles the production path: parsing, link expansion, then validation (connectors first,
    // then GraphQL and federation rules).
    apollo_federation::subgraph::typestate::Subgraph::parse(SCHEMA, "http://test", &sdl)
        .unwrap()
        .expand_links()
        .unwrap()
        .validate()
        .ok();

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
          @link(url: "https://specs.apollo.dev/connect/v0.2", import: ["@connect"])

        type Query {
          hello: String @connect(http: { GET: "http://example/hello" }, selection: "$")
        }
    "#;

    apollo_federation::subgraph::typestate::Subgraph::parse("warmup", "http://warmup", TRIVIAL)
        .unwrap()
        .expand_links()
        .unwrap()
        .validate()
        .ok();
}
