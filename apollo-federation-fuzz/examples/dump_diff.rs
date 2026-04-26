//! Print HEAD's plan and BASE's plan side-by-side for one generated case.
//! Use to manually verify whether the diff harness is silently smoothing
//! real differences via normalization.

use arbitrary::Unstructured;

use apollo_federation_fuzz::compose::{ComposeOutcome, try_compose};
use apollo_federation_fuzz::harness::{CommonConfig, CommonOptions, PlannerHarness};
use apollo_federation_fuzz::op_gen::generate_operation;
use apollo_federation_fuzz::subgraph_gen::{GenConfig, generate_federated_subgraphs};
use apollo_federation_fuzz::{BasePlanner, HeadPlanner};

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

fn main() {
    let start_seed: u64 = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(1);
    let cfg = GenConfig {
        max_fields_per_entity: 8,
        ..GenConfig::default()
    };

    for seed in start_seed..start_seed + 200 {
        let schema_bytes = seeded_bytes(seed, 2048);
        let mut u = Unstructured::new(&schema_bytes);
        let Ok(subgraphs) = generate_federated_subgraphs(&mut u, &cfg) else {
            continue;
        };
        let Ok(composed) = (match try_compose(&subgraphs) {
            ComposeOutcome::Composed { supergraph_sdl } => Ok(supergraph_sdl),
            _ => Err(()),
        }) else {
            continue;
        };

        for op_seed_offset in 0..50u64 {
            let op_bytes = seeded_bytes(seed.wrapping_add(op_seed_offset).wrapping_add(0xFEED), 512);
            let Ok(op) = generate_operation(&composed, &op_bytes) else {
                continue;
            };

            let head = HeadPlanner::build(&composed, &CommonConfig::default()).unwrap();
            let base = BasePlanner::build(&composed, &CommonConfig::default()).unwrap();
            let head_plan = head.plan(&op, None, &CommonOptions::default()).unwrap();
            let base_plan = base.plan(&op, None, &CommonOptions::default()).unwrap();

            let head_pretty = serde_json::to_string_pretty(&head_plan).unwrap();
            let base_pretty = serde_json::to_string_pretty(&base_plan).unwrap();

            // Skip identical cases — find one where raw JSON differs.
            if head_pretty == base_pretty {
                continue;
            }

            println!("# RAW divergence at seed={seed} op_seed_offset={op_seed_offset}");
            println!("=== OPERATION ===\n{op}\n");
            println!("=== HEAD (2.13.1) ===\n{head_pretty}\n");
            println!("=== BASE (2.5.0) ===\n{base_pretty}\n");
            // Show normalized comparison too.
            use apollo_federation_fuzz::diff::{DiffOutcome, run_diff};
            let outcome = run_diff::<HeadPlanner, BasePlanner>(
                &composed, &op, None, &CommonConfig::default(), &CommonOptions::default(),
            );
            println!("=== NORMALIZED OUTCOME ===");
            match outcome {
                DiffOutcome::Identical { .. } => println!("Identical after normalize (suspicious)"),
                DiffOutcome::Divergent { unified_diff, .. } => println!("{unified_diff}"),
                DiffOutcome::EitherFailed { .. } => println!("EitherFailed"),
                DiffOutcome::PanickedSide { head_panic, base_panic, .. } => {
                    println!("PanickedSide head={head_panic:?} base={base_panic:?}");
                }
            }
            return;
        }
    }
    eprintln!("no raw-JSON divergence found in seeds {start_seed}..");
}
