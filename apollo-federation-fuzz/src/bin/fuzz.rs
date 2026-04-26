//! Plain binary driver. Generates federated subgraph sets and operations
//! from a deterministic seed stream, runs each through the diff harness,
//! and prints aggregate stats. Saves a reproducer JSON for any divergence.
//!
//! Two report modes:
//!   --report correctness   (default) — runs `run_diff`, saves divergences
//!                                       and panics, exit 1 on divergence
//!   --report perf          — builds both planners once per schema, times
//!                            each `plan()` per op with randomised
//!                            head/base order, prints distribution stats

use std::path::PathBuf;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::time::Instant;

use arbitrary::Unstructured;
use clap::Parser;
use clap::ValueEnum;
use serde_json::Value;

use apollo_federation_fuzz::compose::{ComposeOutcome, try_compose};
use apollo_federation_fuzz::diff::{DiffOutcome, normalize as normalize_plan, run_diff};
use apollo_federation_fuzz::harness::{CommonConfig, CommonOptions, PlannerHarness};
use apollo_federation_fuzz::op_gen::{OpGenConfig, generate_operation_with_config};
use apollo_federation_fuzz::subgraph_gen::{
    GenConfig, SubgraphSdl, generate_federated_subgraphs, smoke_test_fixture,
};
use apollo_federation_fuzz::{BasePlanner, HeadPlanner};

#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
enum ReportMode {
    /// Diff plan output between HEAD and BASE; save reproducers on
    /// divergence or panic. Exit 1 if any divergence was found.
    Correctness,
    /// Time `plan()` on both sides; report distribution stats. Skips
    /// reproducer saves. Useful for "is HEAD faster than BASE?" runs.
    Perf,
}

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
    /// Report mode. See ReportMode docs above.
    #[arg(long, value_enum, default_value_t = ReportMode::Correctness)]
    report: ReportMode,
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
    panicked: u64,
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

    if args.report == ReportMode::Perf {
        run_perf_mode(args, cfg, opts, gen_cfg, op_cfg);
        return;
    }

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
            DiffOutcome::PanickedSide {
                head_panic,
                base_panic,
                ..
            } => {
                stats.panicked += 1;
                let summary = format!(
                    "head_panic={head_panic:?}\nbase_panic={base_panic:?}\n",
                );
                let id = save_regression(
                    &args.regressions_dir,
                    i,
                    args.seed,
                    subgraphs,
                    supergraph_sdl,
                    &op_text,
                    &format!("=== PANIC ===\n{summary}"),
                );
                println!("=== PLANNER PANIC iter={i} saved={id} ===\n{summary}");
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

/// Saves a divergence/panic reproducer in the slim format consumed by
/// `tests/regression_replay.rs`. The supergraph SDL and the unified diff
/// are intentionally NOT stored: the supergraph is deterministic from
/// the subgraphs + composer, and the diff is regenerated by re-running
/// both planners. A short `summary:` header captures the kind of
/// finding so the file remains human-readable; full detail lives in
/// `unified_diff` while the run is in progress and is logged to stdout
/// from the call site for live inspection.
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

    let summary = summarize_finding(unified_diff);

    let _ = std::fs::create_dir_all(dir);
    let path = dir.join(format!("{id}.txt"));
    let mut content = String::new();
    content.push_str(&format!("iter={iter} seed={seed} sha1_prefix={id}\n"));
    content.push_str(&format!("summary: {summary}\n\n"));
    content.push_str("=== SUBGRAPHS ===\n");
    for s in subgraphs {
        content.push_str(&format!("# {}\n{}\n", s.name, s.sdl));
    }
    content.push_str("\n=== OPERATION ===\n");
    content.push_str(op_text);
    let _ = std::fs::write(path, content);
    id
}

/// Best-effort one-line tag for the `summary:` header. Mirrors the
/// classification logic in `scripts/slim_regressions.py` so retrofit
/// and live capture produce consistent labels.
fn summarize_finding(unified_diff: &str) -> String {
    if let Some(rest) = unified_diff.strip_prefix("=== PANIC ===\n") {
        for line in rest.lines() {
            if let Some(start) = line.find("=Some(\"") {
                let inner = &line[start + 7..];
                if let Some(end) = inner.find("\")") {
                    return format!("PANIC: {}", &inner[..end]);
                }
            }
        }
        return "PANIC".to_string();
    }
    let mut tags: Vec<&'static str> = Vec::new();
    if unified_diff.contains("on Query") {
        tags.push("PR #7580");
    }
    if unified_diff.contains("\"Condition\":") {
        tags.push("FED-505 Condition");
    }
    if unified_diff
        .lines()
        .any(|l| l.starts_with('+') && l.contains("\"sub_selection\":"))
    {
        tags.push("defer sub_selection (C)");
    }
    let has_minus_field_str = unified_diff
        .lines()
        .any(|l| l.starts_with('-') && l.contains("\"Field\": \""));
    let has_plus_field_obj = unified_diff
        .lines()
        .any(|l| l.starts_with('+') && l.contains("\"Field\": {"));
    if has_minus_field_str && has_plus_field_obj {
        tags.push("defer Field repr (D)");
    }
    if tags.is_empty() {
        "(uncategorised diff — possible Class E)".to_string()
    } else {
        tags.join(" + ")
    }
}

// ---------- Perf-report mode ---------------------------------------------

#[derive(Default, Debug)]
struct PerfStats {
    schemas_attempted: u64,
    schemas_composed: u64,
    schemas_built: u64,
    ops_attempted: u64,
    ops_skipped: u64,
    ops_diverged: u64,
    ops_panicked: u64,
    ops_errored: u64,
    /// Per-op (head_micros, base_micros) for ops where both sides
    /// returned a plan. Order randomised per-op to dampen warm-cache
    /// bias toward whichever side runs first.
    samples: Vec<(u128, u128)>,
    /// Per-op (head_fetch_count, base_fetch_count, head_bytes, base_bytes)
    /// for the same set of ops.
    shapes: Vec<(usize, usize, usize, usize)>,
}

fn run_perf_mode(
    args: Args,
    cfg: CommonConfig,
    opts: CommonOptions,
    gen_cfg: GenConfig,
    op_cfg: OpGenConfig,
) {
    let mut stats = PerfStats::default();
    let mut state = args.seed.wrapping_add(0x9E37_79B9_7F4A_7C15);

    // Both planners are built ONCE per schema (the expensive setup —
    // parsing the supergraph, building internal indexes — should not
    // be inside the per-op timing loop).
    struct Planners {
        head: HeadPlanner,
        base: BasePlanner,
    }
    let mut current: Option<Planners> = None;
    let mut current_supergraph: Option<String> = None;
    let mut ops_done_for_schema: u64 = 0;

    for i in 0..args.iterations {
        let need_new_schema = args.smoke_fixture && current.is_none()
            || (!args.smoke_fixture
                && (current.is_none() || ops_done_for_schema >= args.ops_per_schema));

        if need_new_schema {
            let bytes = next_bytes(&mut state, 4096);
            let subgraphs = if args.smoke_fixture {
                smoke_test_fixture()
            } else {
                let mut u = Unstructured::new(&bytes);
                match generate_federated_subgraphs(&mut u, &gen_cfg) {
                    Ok(s) => s,
                    Err(_) => continue,
                }
            };
            stats.schemas_attempted += 1;
            let supergraph_sdl = match try_compose(&subgraphs) {
                ComposeOutcome::Composed { supergraph_sdl } => {
                    stats.schemas_composed += 1;
                    supergraph_sdl
                }
                _ => continue,
            };
            // Build both planners; bail this schema if either fails.
            let head = match HeadPlanner::build(&supergraph_sdl, &cfg) {
                Ok(p) => p,
                Err(_) => continue,
            };
            let base = match BasePlanner::build(&supergraph_sdl, &cfg) {
                Ok(p) => p,
                Err(_) => continue,
            };
            stats.schemas_built += 1;
            current = Some(Planners { head, base });
            current_supergraph = Some(supergraph_sdl);
            ops_done_for_schema = 0;
        }

        let Some(planners) = current.as_ref() else {
            continue;
        };
        let Some(supergraph_sdl) = current_supergraph.as_ref() else {
            continue;
        };

        let op_bytes = next_bytes(&mut state, 1024);
        stats.ops_attempted += 1;
        let op_text = match generate_operation_with_config(supergraph_sdl, &op_bytes, &op_cfg) {
            Ok(op) => op,
            Err(_) => {
                stats.ops_skipped += 1;
                ops_done_for_schema += 1;
                continue;
            }
        };

        // Randomise head/base order to dampen first-run warm-cache bias.
        let head_first = (next_bytes(&mut state, 1)[0] & 1) == 0;
        let plan_h = || {
            let t = Instant::now();
            let r = catch_unwind(AssertUnwindSafe(|| planners.head.plan(&op_text, None, &opts)));
            (t.elapsed().as_micros(), r)
        };
        let plan_b = || {
            let t = Instant::now();
            let r = catch_unwind(AssertUnwindSafe(|| planners.base.plan(&op_text, None, &opts)));
            (t.elapsed().as_micros(), r)
        };
        let ((head_us, head_res), (base_us, base_res)) = if head_first {
            let h = plan_h();
            let b = plan_b();
            (h, b)
        } else {
            let b = plan_b();
            let h = plan_h();
            (h, b)
        };

        ops_done_for_schema += 1;

        let (head_plan, base_plan) = match (head_res, base_res) {
            (Ok(Ok(h)), Ok(Ok(b))) => (h, b),
            (Err(_), _) | (_, Err(_)) => {
                stats.ops_panicked += 1;
                continue;
            }
            _ => {
                stats.ops_errored += 1;
                continue;
            }
        };

        // Plans should match for perf comparison to be meaningful.
        // Reuse the diff layer's normalisation to avoid spurious
        // mismatches from version-drift wire format. Cheap; runs after
        // timing so it doesn't pollute measurements.
        if !plans_equivalent(&head_plan, &base_plan) {
            stats.ops_diverged += 1;
            continue;
        }

        let h_bytes = serde_json::to_string(&head_plan).map(|s| s.len()).unwrap_or(0);
        let b_bytes = serde_json::to_string(&base_plan).map(|s| s.len()).unwrap_or(0);
        let h_fetch = count_fetch_nodes(&head_plan);
        let b_fetch = count_fetch_nodes(&base_plan);

        stats.samples.push((head_us, base_us));
        stats.shapes.push((h_fetch, b_fetch, h_bytes, b_bytes));

        if args.verbose {
            eprintln!(
                "[{i}] head={head_us}us base={base_us}us h_fetch={h_fetch} b_fetch={b_fetch} ratio={:.2}",
                head_us as f64 / base_us.max(1) as f64
            );
        }
    }

    print_perf_report(&stats);
}

/// Compares plans through the same normaliser the correctness-mode
/// diff layer uses, so version-format drift (e.g. older versions
/// serialising `requires:` as raw SDL strings, planner statistics that
/// don't exist in older versions, null-vs-absent option fields)
/// doesn't artificially exclude ops from the perf sample.
fn plans_equivalent(a: &Value, b: &Value) -> bool {
    normalize_plan(a) == normalize_plan(b)
}

fn count_fetch_nodes(v: &Value) -> usize {
    match v {
        Value::Object(map) => {
            let mut n = 0;
            for (k, vv) in map {
                if k == "Fetch" {
                    n += 1;
                }
                n += count_fetch_nodes(vv);
            }
            n
        }
        Value::Array(arr) => arr.iter().map(count_fetch_nodes).sum(),
        _ => 0,
    }
}

fn percentile(sorted: &[u128], p: f64) -> u128 {
    if sorted.is_empty() {
        return 0;
    }
    let idx = ((sorted.len() - 1) as f64 * p).round() as usize;
    sorted[idx.min(sorted.len() - 1)]
}

fn print_perf_report(stats: &PerfStats) {
    println!("=== PERF REPORT ===");
    println!(
        "schemas: attempted={} composed={} built={}",
        stats.schemas_attempted, stats.schemas_composed, stats.schemas_built
    );
    println!(
        "ops:     attempted={} skipped={} diverged={} panicked={} errored={} sampled={}",
        stats.ops_attempted,
        stats.ops_skipped,
        stats.ops_diverged,
        stats.ops_panicked,
        stats.ops_errored,
        stats.samples.len()
    );

    if stats.samples.is_empty() {
        println!("(no perf samples — perf comparison requires at least one op where both planners produced byte-equal JSON)");
        return;
    }

    let mut head_us: Vec<u128> = stats.samples.iter().map(|(h, _)| *h).collect();
    let mut base_us: Vec<u128> = stats.samples.iter().map(|(_, b)| *b).collect();
    head_us.sort();
    base_us.sort();

    // Ratios: head/base, expressed as f64. Sort separately for percentiles.
    let mut ratios: Vec<f64> = stats
        .samples
        .iter()
        .map(|(h, b)| *h as f64 / (*b).max(1) as f64)
        .collect();
    ratios.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let ratio_pct = |p: f64| -> f64 {
        if ratios.is_empty() {
            return f64::NAN;
        }
        let idx = ((ratios.len() - 1) as f64 * p).round() as usize;
        ratios[idx.min(ratios.len() - 1)]
    };

    println!();
    println!("plan() wall-clock micros:");
    println!(
        "  head: min={:>6} p50={:>7} p95={:>8} p99={:>8} max={:>9}",
        head_us[0],
        percentile(&head_us, 0.5),
        percentile(&head_us, 0.95),
        percentile(&head_us, 0.99),
        head_us[head_us.len() - 1]
    );
    println!(
        "  base: min={:>6} p50={:>7} p95={:>8} p99={:>8} max={:>9}",
        base_us[0],
        percentile(&base_us, 0.5),
        percentile(&base_us, 0.95),
        percentile(&base_us, 0.99),
        base_us[base_us.len() - 1]
    );

    println!();
    println!("ratio (head / base):");
    println!(
        "  min={:.2} p50={:.2} p95={:.2} p99={:.2} max={:.2}",
        ratios[0],
        ratio_pct(0.5),
        ratio_pct(0.95),
        ratio_pct(0.99),
        ratios[ratios.len() - 1]
    );
    let median = ratio_pct(0.5);
    println!(
        "  verdict (median ratio): {}",
        if median < 0.95 {
            format!("HEAD faster (median {:.2}x base)", median)
        } else if median > 1.05 {
            format!("HEAD slower (median {:.2}x base)", median)
        } else {
            format!("within ±5% (median {:.2}x base) — likely noise", median)
        }
    );

    // Plan-shape stats are noise-free; useful as a sanity check on the
    // perf signal ("is HEAD measurably faster because plans got smaller?").
    let mut h_fetch: Vec<usize> = stats.shapes.iter().map(|s| s.0).collect();
    let mut b_fetch: Vec<usize> = stats.shapes.iter().map(|s| s.1).collect();
    let mut h_bytes: Vec<usize> = stats.shapes.iter().map(|s| s.2).collect();
    let mut b_bytes: Vec<usize> = stats.shapes.iter().map(|s| s.3).collect();
    h_fetch.sort();
    b_fetch.sort();
    h_bytes.sort();
    b_bytes.sort();
    let median_usize = |v: &[usize]| -> usize { v[v.len() / 2] };

    println!();
    println!("plan-shape (median across sampled ops):");
    println!(
        "  fetch nodes: head={} base={}",
        median_usize(&h_fetch),
        median_usize(&b_fetch)
    );
    println!(
        "  json bytes:  head={} base={}",
        median_usize(&h_bytes),
        median_usize(&b_bytes)
    );
}
