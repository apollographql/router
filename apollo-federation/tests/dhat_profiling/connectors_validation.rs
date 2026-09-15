#[global_allocator]
pub(crate) static ALLOC: dhat::Alloc = dhat::Alloc;

// Failure of the test can be diagnosed using the dhat-heap.json file.

// These values should be kept slightly larger (~10%) than the current heap usage to catch
// significant increases.
#[test]
fn valid_large_body() {
    const SCHEMA: &str = "src/connectors/validation/test_data/valid_large_body.graphql";

    // Bumped from 275_000 when connectors validation moved to run mid-expansion
    // (`ConnectorsBlueprint::on_validation`), which put link expansion inside the profiled region,
    // and from 420_000 when the GraphQL front end became `connectors::graphql_shapes`.
    //
    // The profiled region covers parsing, expansion and validation. Of the pre-front-end 396k,
    // roughly 192k is parsing and expansion (see `subgraph_expansion_profiling`, which measures
    // exactly that half against the same fixture) and the rest is validation. If this test
    // regresses, check that one first: if it moved too, the cost is in expansion, not here.
    //
    // `validate()` covers connectors validation *and* GraphQL/federation validation, which cannot
    // be measured apart now that they share a transition. Connectors validation dominates: adding
    // the GraphQL half moved the peak by well under 1k.
    //
    // Measured against this fixture, before and after the front end moved in:
    //
    //              peak bytes   total blocks
    //   before        395,940         58,730
    //   after         450,627         59,307
    //
    // Peak live bytes grew 13.8% while allocation count moved 1.0%, which is the signature of more
    // shape held live at once rather than more allocation traffic. The heap profile attributes it
    // to node count, not to any one allocation getting bigger: GraphQL types now carry the shapes
    // their schemas describe, so `ID` is a three-node `One<String, Int>` where the old walker made
    // every scalar a single `Unknown`, and `[String]` is `One<List<One<String, null>>, null>`,
    // where that walker wrapped the inner type once and left list elements no nullability of their
    // own. Each node owns an `IndexSet<Location>` that allocates, since `Shape::cached_or_else`
    // hands back its cached singleton only for an empty location list and this path always has
    // real spans. Of the 55k, roughly 34k is those nodes and their locations, 14k is
    // `Namespace::insert` propagating derived names over the bigger tree, and 5k is the member
    // sets of the new unions.
    //
    // A sixth of that was avoidable and is gone: `graphql_shapes::Locator` memoizes each file's
    // `SourceId`, which took the `graphql:<path>` strings live at peak from 4,744 bytes in 103
    // blocks to 216 in 3.
    //
    // 500_000 is ~11% above the measured peak, the margin this file asks for. Note that 420_000
    // gave only 6% over 395,940, so the guard was already tighter than stated before this change.
    const MAX_BYTES: usize = 500_000;
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
        // Not `.ok()`: connectors validation runs first and returns early on any error, so a fixture
        // that drifted into being invalid would silently shrink the profiled region to
        // parse-and-bail and let the limits below pass no matter how much the real path regressed.
        .expect("fixture is expected to validate");

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
        // Same reason as above: a warm-up that bailed early would leave part of the one-time
        // initialization unpaid, and it would land inside the profiled region instead.
        .expect("warm-up subgraph is expected to validate");
}
