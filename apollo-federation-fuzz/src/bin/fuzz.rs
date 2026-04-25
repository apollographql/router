//! Plain binary driver. Composes the smoke-test fixture, then loops a
//! configurable number of iterations generating operations and diffing the
//! two planner versions.

use clap::Parser;

use apollo_federation_fuzz::compose::{ComposeOutcome, try_compose};
use apollo_federation_fuzz::diff::{DiffOutcome, run_diff};
use apollo_federation_fuzz::harness::{CommonConfig, CommonOptions};
use apollo_federation_fuzz::op_gen::generate_operation;
use apollo_federation_fuzz::subgraph_gen::smoke_test_fixture;
use apollo_federation_fuzz::{BasePlanner, HeadPlanner};

#[derive(Parser, Debug)]
#[command(about = "Differential fuzz of two apollo-federation query planners")]
struct Args {
    /// Total operations to attempt.
    #[arg(long, default_value_t = 100)]
    iterations: u64,
    /// Deterministic seed.
    #[arg(long, default_value_t = 0)]
    seed: u64,
    /// Print one line per generated/skipped operation.
    #[arg(long, default_value_t = false)]
    verbose: bool,
}

fn main() {
    let args = Args::parse();

    let subgraphs = smoke_test_fixture();
    let supergraph_sdl = match try_compose(&subgraphs) {
        ComposeOutcome::Composed { supergraph_sdl } => supergraph_sdl,
        ComposeOutcome::ParseFailed { errors } => {
            eprintln!("smoke fixture failed to parse: {errors:?}");
            std::process::exit(2);
        }
        ComposeOutcome::CompositionFailed { errors } => {
            eprintln!("smoke fixture failed to compose:");
            for e in errors {
                eprintln!("  - {e}");
            }
            std::process::exit(2);
        }
    };

    let cfg = CommonConfig::default();
    let opts = CommonOptions::default();

    let mut state: u64 = args.seed.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut planned = 0u64;
    let mut skipped = 0u64;
    let mut identical = 0u64;
    let mut divergent = 0u64;
    let mut errored = 0u64;

    for i in 0..args.iterations {
        // SplitMix64 step for deterministic byte-stream seeding.
        state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^= z >> 31;
        let mut bytes = vec![0u8; 256];
        for (j, b) in bytes.iter_mut().enumerate() {
            *b = ((z >> ((j as u64 * 8) % 64)) & 0xFF) as u8;
        }

        let op = match generate_operation(&supergraph_sdl, &bytes) {
            Ok(op) => op,
            Err(e) => {
                skipped += 1;
                if args.verbose {
                    eprintln!("[{i}] skip op-gen: {e}");
                }
                continue;
            }
        };

        let outcome = run_diff::<HeadPlanner, BasePlanner>(
            &supergraph_sdl,
            &op,
            None,
            &cfg,
            &opts,
        );
        planned += 1;
        match outcome {
            DiffOutcome::Identical { .. } => identical += 1,
            DiffOutcome::Divergent { unified_diff, .. } => {
                divergent += 1;
                println!("=== DIVERGENCE iter={i} ===\nOPERATION:\n{op}\n{unified_diff}");
            }
            DiffOutcome::EitherFailed { head, base } => {
                errored += 1;
                if args.verbose {
                    eprintln!("[{i}] either-failed head={:?} base={:?}", head.is_ok(), base.is_ok());
                }
            }
        }
    }

    println!(
        "iterations={} planned={} skipped={} identical={} divergent={} errored={}",
        args.iterations, planned, skipped, identical, divergent, errored
    );

    if divergent > 0 {
        std::process::exit(1);
    }
}
