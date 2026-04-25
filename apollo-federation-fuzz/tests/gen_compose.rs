//! Phase-C generator stress test: assert the constructive subgraph
//! generator yields composition-valid graphs at a high rate, and that the
//! resulting supergraphs accept generated operations through both planners.

use arbitrary::Unstructured;

use apollo_federation_fuzz::compose::{ComposeOutcome, try_compose};
use apollo_federation_fuzz::diff::{DiffOutcome, run_diff};
use apollo_federation_fuzz::harness::{CommonConfig, CommonOptions};
use apollo_federation_fuzz::op_gen::generate_operation;
use apollo_federation_fuzz::subgraph_gen::{GenConfig, generate_federated_subgraphs};
use apollo_federation_fuzz::{BasePlanner, HeadPlanner};

/// Deterministic SplitMix64 -> byte stream so each iteration uses an
/// independent reproducible seed.
fn seeded_bytes(seed: u64, bytes: usize) -> Vec<u8> {
    let mut state = seed.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut out = Vec::with_capacity(bytes);
    while out.len() < bytes {
        state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^= z >> 31;
        out.extend_from_slice(&z.to_le_bytes());
    }
    out.truncate(bytes);
    out
}

#[test]
fn generated_subgraphs_compose_at_high_rate() {
    let cfg = GenConfig::default();
    let mut composed = 0;
    let mut failed = 0;
    let trials = 100;

    for seed in 0..trials {
        let bytes = seeded_bytes(seed, 4096);
        let mut u = Unstructured::new(&bytes);
        let subgraphs = match generate_federated_subgraphs(&mut u, &cfg) {
            Ok(s) => s,
            Err(_) => {
                // Out of input bytes — count as a generator skip, not a fail.
                continue;
            }
        };
        match try_compose(&subgraphs) {
            ComposeOutcome::Composed { .. } => composed += 1,
            _ => failed += 1,
        }
    }

    let total = composed + failed;
    assert!(total > 0, "no successful generation runs");
    let rate = composed as f64 / total as f64;
    eprintln!(
        "compose rate: {composed}/{total} = {:.1}%",
        rate * 100.0
    );
    // A constructive @key + @shareable generator should compose almost
    // always; allow some headroom for legitimately inconsistent random
    // shareable-field type choices.
    assert!(
        rate >= 0.85,
        "composition success rate too low: {rate:.2}"
    );
}

#[test]
fn generated_supergraph_plans_agree_across_versions() {
    let cfg = GenConfig::default();
    let common_cfg = CommonConfig::default();
    let common_opts = CommonOptions::default();

    let mut planned = 0;
    let mut divergent = 0;
    let mut errored = 0;
    let mut compose_failed = 0;
    let mut op_skipped = 0;
    let trials = 200;

    for seed in 0..trials {
        let schema_bytes = seeded_bytes(seed, 2048);
        let mut su = Unstructured::new(&schema_bytes);
        let Ok(subgraphs) = generate_federated_subgraphs(&mut su, &cfg) else {
            continue;
        };
        let supergraph_sdl = match try_compose(&subgraphs) {
            ComposeOutcome::Composed { supergraph_sdl } => supergraph_sdl,
            _ => {
                compose_failed += 1;
                continue;
            }
        };

        // Use a different sub-seed for the operation so generator state is
        // independent of schema state.
        let op_bytes = seeded_bytes(seed.wrapping_add(0x1234_5678), 1024);
        let op_text = match generate_operation(&supergraph_sdl, &op_bytes) {
            Ok(op) => op,
            Err(_) => {
                op_skipped += 1;
                continue;
            }
        };

        match run_diff::<HeadPlanner, BasePlanner>(
            &supergraph_sdl,
            &op_text,
            None,
            &common_cfg,
            &common_opts,
        ) {
            DiffOutcome::Identical { .. } => planned += 1,
            DiffOutcome::Divergent { unified_diff, .. } => {
                divergent += 1;
                eprintln!(
                    "DIVERGENCE seed={seed}\n--- subgraphs ---\n{}\n--- supergraph ---\n{}\n--- operation ---\n{}\n--- diff ---\n{}",
                    subgraphs
                        .iter()
                        .map(|s| format!("# {}\n{}", s.name, s.sdl))
                        .collect::<Vec<_>>()
                        .join("\n"),
                    supergraph_sdl,
                    op_text,
                    unified_diff,
                );
            }
            DiffOutcome::EitherFailed { .. } => errored += 1,
        }
    }

    eprintln!(
        "trials={trials} planned={planned} divergent={divergent} errored={errored} compose_failed={compose_failed} op_skipped={op_skipped}"
    );

    // Phase-C invariant: V_head and V_base are the *same* algorithm modulo
    // a patch version, so any divergence is itself a finding.
    assert_eq!(divergent, 0, "planners diverged in {divergent} cases");
}
