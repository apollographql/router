//! Determinism contract tests for the generators. Same seed → same output.
//!
//! The harness deliberately uses ordered containers (`BTreeSet`, sorted
//! `Vec`s, fixed iteration orders) to keep generation reproducible across
//! runs. These tests pin that contract: if a future refactor introduces a
//! `HashMap` for "performance", produces nondeterministic output via
//! threadlocal RNGs, or otherwise lets seed → output drift, these tests
//! break.
//!
//! Reproducibility matters because:
//!  - Saved regression artefacts (`tests/regressions/*.txt`) reference
//!    specific seeds; if those seeds suddenly produce different schemas,
//!    the artefacts become misleading.
//!  - Bisecting upstream planner bugs requires that "seed 17 op 273" means
//!    the same thing on Monday and Friday.

use arbitrary::Unstructured;

use apollo_federation_fuzz::compose::{ComposeOutcome, try_compose};
use apollo_federation_fuzz::op_gen::{OpGenConfig, generate_operation_with_config};
use apollo_federation_fuzz::subgraph_gen::{GenConfig, generate_federated_subgraphs};

/// SplitMix64-based deterministic byte stream. Matches the helper used by
/// `tests/gen_compose.rs` and `bin/fuzz.rs`, so seed values are
/// directly comparable across the test suite.
fn seeded_bytes(seed: u64, n: usize) -> Vec<u8> {
    let mut state = seed.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut out = Vec::with_capacity(n);
    while out.len() < n {
        state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^= z >> 31;
        out.extend_from_slice(&z.to_le_bytes());
    }
    out.truncate(n);
    out
}

#[test]
fn schema_generation_is_deterministic() {
    let cfg = GenConfig::default();
    for seed in [0, 17, 42, 99, 1234u64] {
        let bytes = seeded_bytes(seed, 4096);
        let mut u1 = Unstructured::new(&bytes);
        let mut u2 = Unstructured::new(&bytes);
        let s1 = generate_federated_subgraphs(&mut u1, &cfg)
            .expect("first generation succeeds");
        let s2 = generate_federated_subgraphs(&mut u2, &cfg)
            .expect("second generation succeeds");
        assert_eq!(s1.len(), s2.len(), "subgraph count drift at seed {seed}");
        for (a, b) in s1.iter().zip(s2.iter()) {
            assert_eq!(a.name, b.name, "subgraph name drift at seed {seed}");
            assert_eq!(
                a.sdl, b.sdl,
                "subgraph SDL drift at seed {seed} for {}",
                a.name
            );
        }
    }
}

#[test]
fn operation_generation_is_deterministic() {
    let cfg = GenConfig::default();
    let op_cfg = OpGenConfig::default();

    // Pick the first seed whose schema composes; reuse for both ops.
    let mut composed_for: Option<(u64, String)> = None;
    for seed in 0..20u64 {
        let bytes = seeded_bytes(seed, 4096);
        let mut u = Unstructured::new(&bytes);
        let Ok(subgraphs) = generate_federated_subgraphs(&mut u, &cfg) else {
            continue;
        };
        if let ComposeOutcome::Composed { supergraph_sdl } = try_compose(&subgraphs) {
            composed_for = Some((seed, supergraph_sdl));
            break;
        }
    }
    let (_seed, supergraph_sdl) =
        composed_for.expect("at least one early seed composes");

    for op_seed in [0, 7, 91, 314u64] {
        let op_bytes = seeded_bytes(op_seed.wrapping_add(0xFEED), 1024);
        let op1 = generate_operation_with_config(&supergraph_sdl, &op_bytes, &op_cfg)
            .expect("first op-gen succeeds");
        let op2 = generate_operation_with_config(&supergraph_sdl, &op_bytes, &op_cfg)
            .expect("second op-gen succeeds");
        assert_eq!(
            op1, op2,
            "op-gen output drift at op_seed {op_seed}"
        );
    }
}
