//! Plain binary driver. Generates federated subgraph sets and operations
//! from a deterministic seed stream, runs each through the diff harness,
//! and prints aggregate stats. Saves a reproducer JSON for any divergence.

use std::path::PathBuf;

use arbitrary::Unstructured;
use clap::Parser;

use apollo_federation_fuzz::compose::{ComposeOutcome, try_compose};
use apollo_federation_fuzz::diff::{DiffOutcome, run_diff};
use apollo_federation_fuzz::harness::{CommonConfig, CommonOptions};
use apollo_federation_fuzz::op_gen::{OpGenConfig, generate_operation_with_config};
use apollo_federation_fuzz::subgraph_gen::{
    GenConfig, SubgraphSdl, generate_federated_subgraphs, smoke_test_fixture,
};
use apollo_federation_fuzz::{BasePlanner, HeadPlanner};

#[derive(Parser, Debug)]
#[command(about = "Differential fuzz of two apollo-federation query planners")]
struct Args {
    /// Total iterations to attempt.
    #[arg(long, default_value_t = 200)]
    iterations: u64,
    /// Deterministic seed.
    #[arg(long, default_value_t = 0)]
    seed: u64,
    /// How many planner runs to execute against each generated supergraph
    /// before regenerating. Set to 1 to maximize schema variety; raise to
    /// amortize the (relatively expensive) composition step.
    #[arg(long, default_value_t = 8)]
    ops_per_schema: u64,
    /// Use the hand-written 2-subgraph fixture instead of generating one.
    /// Useful for repro-debugging without schema noise.
    #[arg(long, default_value_t = false)]
    smoke_fixture: bool,
    /// Print one line per generated/skipped operation.
    #[arg(long, default_value_t = false)]
    verbose: bool,
    /// Save reproducers (subgraphs + supergraph + operation + diff) for any
    /// divergence under this directory.
    #[arg(long, default_value = "regressions")]
    regressions_dir: PathBuf,
    /// Enable `@defer`: op-gen sprinkles the directive on inline fragments
    /// and the planner is configured with `incremental_delivery = true` so
    /// `DeferNode` actually appears in plans.
    #[arg(long, default_value_t = false)]
    enable_defer: bool,
}

#[derive(Default, Debug)]
struct Stats {
    schemas_attempted: u64,
    schemas_composed: u64,
    schemas_compose_failed: u64,
    ops_attempted: u64,
    ops_skipped: u64,
    planned_identical: u64,
    planned_divergent: u64,
    planner_errored: u64,
}

fn main() {
    let args = Args::parse();
    let cfg = CommonConfig {
        incremental_delivery: args.enable_defer,
        ..CommonConfig::default()
    };
    let opts = CommonOptions::default();
    let gen_cfg = GenConfig::default();
    let op_cfg = OpGenConfig {
        defer_chance: if args.enable_defer { 64 } else { 0 },
        ..OpGenConfig::default()
    };

    let mut stats = Stats::default();
    let mut state = args.seed.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut current_schema: Option<(Vec<SubgraphSdl>, String)> = None;
    let mut ops_done_for_schema: u64 = 0;

    for i in 0..args.iterations {
        let need_new_schema = args.smoke_fixture
            && current_schema.is_none()
            || (!args.smoke_fixture
                && (current_schema.is_none() || ops_done_for_schema >= args.ops_per_schema));

        if need_new_schema {
            let bytes = next_bytes(&mut state, 4096);
            let subgraphs = if args.smoke_fixture {
                smoke_test_fixture()
            } else {
                let mut u = Unstructured::new(&bytes);
                match generate_federated_subgraphs(&mut u, &gen_cfg) {
                    Ok(s) => s,
                    Err(e) => {
                        if args.verbose {
                            eprintln!("[{i}] gen subgraph err: {e}");
                        }
                        continue;
                    }
                }
            };
            stats.schemas_attempted += 1;
            match try_compose(&subgraphs) {
                ComposeOutcome::Composed { supergraph_sdl } => {
                    stats.schemas_composed += 1;
                    current_schema = Some((subgraphs, supergraph_sdl));
                    ops_done_for_schema = 0;
                }
                other => {
                    stats.schemas_compose_failed += 1;
                    if args.verbose {
                        eprintln!("[{i}] compose failed: {other:?}");
                    }
                    continue;
                }
            }
        }

        let Some((subgraphs, supergraph_sdl)) = current_schema.as_ref() else {
            continue;
        };

        let op_bytes = next_bytes(&mut state, 1024);
        stats.ops_attempted += 1;
        let op_text = match generate_operation_with_config(supergraph_sdl, &op_bytes, &op_cfg) {
            Ok(op) => op,
            Err(e) => {
                stats.ops_skipped += 1;
                ops_done_for_schema += 1;
                if args.verbose {
                    eprintln!("[{i}] op skip: {e}");
                }
                continue;
            }
        };

        let outcome = run_diff::<HeadPlanner, BasePlanner>(
            supergraph_sdl,
            &op_text,
            None,
            &cfg,
            &opts,
        );
        ops_done_for_schema += 1;

        match outcome {
            DiffOutcome::Identical { .. } => stats.planned_identical += 1,
            DiffOutcome::Divergent {
                unified_diff,
                head: _,
                base: _,
            } => {
                stats.planned_divergent += 1;
                let id = save_regression(
                    &args.regressions_dir,
                    i,
                    args.seed,
                    subgraphs,
                    supergraph_sdl,
                    &op_text,
                    &unified_diff,
                );
                println!("=== DIVERGENCE iter={i} saved={id} ===\n{unified_diff}");
            }
            DiffOutcome::EitherFailed { head, base } => {
                stats.planner_errored += 1;
                if args.verbose {
                    eprintln!(
                        "[{i}] planner err head_ok={} base_ok={}",
                        head.is_ok(),
                        base.is_ok()
                    );
                }
            }
        }
    }

    println!("{stats:?}");

    if stats.planned_divergent > 0 {
        std::process::exit(1);
    }
}

fn next_bytes(state: &mut u64, n: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(n);
    while out.len() < n {
        *state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = *state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^= z >> 31;
        out.extend_from_slice(&z.to_le_bytes());
    }
    out.truncate(n);
    out
}

fn save_regression(
    dir: &PathBuf,
    iter: u64,
    seed: u64,
    subgraphs: &[SubgraphSdl],
    supergraph_sdl: &str,
    op_text: &str,
    unified_diff: &str,
) -> String {
    use sha1::{Digest, Sha1};
    let mut hasher = Sha1::new();
    hasher.update(supergraph_sdl);
    hasher.update(op_text);
    let id = hex::encode(&hasher.finalize()[..6]);

    let _ = std::fs::create_dir_all(dir);
    let path = dir.join(format!("{id}.txt"));
    let mut content = String::new();
    content.push_str(&format!("iter={iter} seed={seed} sha1_prefix={id}\n\n"));
    content.push_str("=== SUBGRAPHS ===\n");
    for s in subgraphs {
        content.push_str(&format!("# {}\n{}\n", s.name, s.sdl));
    }
    content.push_str("\n=== SUPERGRAPH ===\n");
    content.push_str(supergraph_sdl);
    content.push_str("\n=== OPERATION ===\n");
    content.push_str(op_text);
    content.push_str("\n=== DIFF ===\n");
    content.push_str(unified_diff);
    let _ = std::fs::write(path, content);
    id
}
