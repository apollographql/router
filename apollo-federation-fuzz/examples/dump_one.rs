//! Generate one (subgraph set, supergraph, operation, query plan) tuple
//! and print everything. Tries seeds 0, 1, 2, ... until it lands on a case
//! that exercises multiple subgraph fetches (an "interesting" plan).
//!
//! Usage:
//!   cargo run -p apollo-federation-fuzz --example dump_one [start_seed]

use arbitrary::Unstructured;

use apollo_federation_fuzz::compose::{ComposeOutcome, try_compose};
use apollo_federation_fuzz::harness::{CommonConfig, CommonOptions, PlannerHarness};
use apollo_federation_fuzz::op_gen::generate_operation;
use apollo_federation_fuzz::subgraph_gen::{GenConfig, generate_federated_subgraphs};
use apollo_federation_fuzz::HeadPlanner;

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
        .unwrap_or(0);
    // Larger field count so @requires has fodder to wire up.
    let cfg = GenConfig {
        max_fields_per_entity: 8,
        ..GenConfig::default()
    };

    for seed in start_seed..start_seed + 500 {
        let schema_bytes = seeded_bytes(seed, 2048);
        let mut u = Unstructured::new(&schema_bytes);
        let Ok(subgraphs) = generate_federated_subgraphs(&mut u, &cfg) else {
            continue;
        };
        // Bias toward cases that actually emit @requires.
        let has_requires = subgraphs.iter().any(|s| s.sdl.contains("@requires"));
        if !has_requires {
            continue;
        }
        let supergraph_sdl = match try_compose(&subgraphs) {
            ComposeOutcome::Composed { supergraph_sdl } => supergraph_sdl,
            _ => continue,
        };

        for op_seed_offset in 0..50u64 {
            let op_bytes = seeded_bytes(seed.wrapping_add(op_seed_offset).wrapping_add(0xFEED), 512);
            let Ok(op) = generate_operation(&supergraph_sdl, &op_bytes) else {
                continue;
            };
            let Ok(planner) = HeadPlanner::build(&supergraph_sdl, &CommonConfig::default()) else {
                continue;
            };
            let Ok(plan) = planner.plan(&op, None, &CommonOptions::default()) else {
                continue;
            };

            let serialized = serde_json::to_string(&plan).unwrap_or_default();
            let fetch_count = serialized.matches("\"Fetch\":").count();
            // "Interesting" = ≥2 subgraph fetches AND the plan exercises
            // the @requires path (entity-fetch with `requires:` selections).
            if fetch_count < 2 || !serialized.contains("\"requires\":") {
                continue;
            }

            print_dump(seed, op_seed_offset, &subgraphs, &supergraph_sdl, &op, &plan, fetch_count);
            return;
        }
    }
    eprintln!("no interesting @requires-exercising case found in seed range {start_seed}..");
    std::process::exit(1);
}

fn print_dump(
    schema_seed: u64,
    op_seed: u64,
    subgraphs: &[apollo_federation_fuzz::subgraph_gen::SubgraphSdl],
    supergraph_sdl: &str,
    op: &str,
    plan: &serde_json::Value,
    fetch_count: usize,
) {
    println!("# schema_seed={schema_seed} op_seed_offset={op_seed} fetch_count={fetch_count}\n");
    println!("=== GENERATED SUBGRAPHS ===");
    for s in subgraphs {
        println!("# subgraph: {}", s.name);
        println!("{}", s.sdl);
    }
    println!("=== COMPOSED SUPERGRAPH ===");
    println!("{supergraph_sdl}");
    println!("=== GENERATED OPERATION ===");
    println!("{op}");
    println!("=== HEAD QUERY PLAN ===");
    println!("{}", serde_json::to_string_pretty(plan).unwrap_or_default());
}
