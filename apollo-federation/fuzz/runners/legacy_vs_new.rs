//! TEMPORARY. Times the legacy checker against the new one and records every disagreement.
//!
//! `plan_smoke` already cross-checks the two verdicts, but it only counts: it says how many cases
//! each checker rejected, not *which* cases they disagreed about, and it does not time anything.
//! This runner adds both, so a refactor of `correctness` can be judged on agreement and on cost.
//!
//! Two lanes, the same two that `plan_smoke` walks:
//!   - the **grammar** lane, every case the model can produce, most of them deliberately broken;
//!   - the **planner** lane, real plans from the query planner, which are correct by construction.
//!
//! Operations and plans are built once, up front, so the timed region holds nothing but the two
//! `check_plan` calls. Each lane is swept `--reps` times (default 5) and the two checkers
//! alternate which one goes first on each case, so neither gets a systematic cache advantage.
//! Per-case statistics come from the fastest rep, which is the one least polluted by whatever else
//! the machine was doing.
//!
//!     cargo run --release --example legacy_vs_new -- [--reps N] [--csv PATH]
//!
//! Delete this file and its `[[example]]` entry when the comparison is done.

use std::fmt::Write as _;
use std::time::Duration;
use std::time::Instant;

use apollo_compiler::ExecutableDocument;
use apollo_compiler::validation::Valid;
use apollo_federation::correctness;
use apollo_federation::query_plan::QueryPlan;
use query_inclusion_fuzz::plan_fixture::Fixture;
use query_inclusion_fuzz::plan_model;

/// One case, built and ready to check.
struct Prepared {
    /// The shortest byte string that reaches this case, for reproducing it.
    bytes: Vec<u8>,
    perturbation: String,
    operation_source: String,
    operation: Valid<ExecutableDocument>,
    plan: QueryPlan,
}

/// What the two checkers said about one case.
struct Verdicts {
    new_ok: bool,
    legacy_ok: bool,
    new_error: Option<String>,
    legacy_error: Option<String>,
}

fn main() {
    let mut reps = 5usize;
    let mut csv_path: Option<String> = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--reps" => reps = args.next().and_then(|v| v.parse().ok()).unwrap_or(reps),
            "--csv" => csv_path = args.next(),
            other => {
                eprintln!("unknown argument: {other}");
                std::process::exit(2);
            }
        }
    }
    assert!(reps > 0, "--reps must be at least 1");

    let fixture = Fixture::new();
    println!("{}\n", plan_model::schema_digest(fixture.supergraph_schema()));

    let grammar = prepare_grammar(&fixture);
    let planner = prepare_planner(&fixture);

    let mut csv = String::from("lane,case,perturbation,new,legacy,agree,new_us,legacy_us\n");
    let grammar_report = run_lane(&fixture, "grammar", &grammar, reps, &mut csv);
    let planner_report = run_lane(&fixture, "planner", &planner, reps, &mut csv);

    if let Some(path) = csv_path {
        match std::fs::write(&path, &csv) {
            Ok(()) => println!("\nper-case rows written to {path}"),
            Err(error) => eprintln!("\ncould not write {path}: {error}"),
        }
    }

    // A disagreement is the thing worth failing on; timing is reported either way.
    let disagreements = grammar_report + planner_report;
    if disagreements > 0 {
        println!("\n{disagreements} disagreement(s) found");
        std::process::exit(1);
    }
    println!("\nthe two checkers agreed on every case in both lanes");
}

/// Every case the grammar can produce, with its deliberately broken plan.
fn prepare_grammar(fixture: &Fixture) -> Vec<Prepared> {
    let mut prepared = Vec::new();
    for bytes in plan_model::enumerate_inputs() {
        let Some(case) = plan_model::decode_case(&bytes) else {
            continue;
        };
        let Some(operation) = plan_model::build_operation(fixture.api_schema(), &case) else {
            continue;
        };
        let plan = plan_model::build_plan(fixture.subgraph_schemas(), &case);
        prepared.push(Prepared {
            bytes,
            perturbation: format!("{:?}", case.perturbation),
            operation_source: case.operation_source(),
            operation,
            plan,
        });
    }
    prepared
}

/// Real plans from the query planner, one per distinct operation.
///
/// These are the plans a checker meets in production, so they are the honest timing input; the
/// grammar lane's plans are mostly broken, and a checker can reject those early.
fn prepare_planner(fixture: &Fixture) -> Vec<Prepared> {
    let mut prepared = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for bytes in plan_model::enumerate_inputs() {
        let Some(case) = plan_model::decode_case(&bytes) else {
            continue;
        };
        let source = case.operation_source();
        if !seen.insert(source.clone()) {
            continue;
        }
        let Some(operation) = plan_model::build_operation(fixture.api_schema(), &case) else {
            continue;
        };
        let Ok(plan) = fixture
            .planner()
            .build_query_plan(&operation, None, Default::default())
        else {
            continue;
        };
        prepared.push(Prepared {
            bytes,
            perturbation: "Planned".to_string(),
            operation_source: source,
            operation,
            plan,
        });
    }
    prepared
}

/// Sweeps one lane `reps` times and reports agreement and timing. Returns the disagreement count.
fn run_lane(
    fixture: &Fixture,
    lane: &str,
    cases: &[Prepared],
    reps: usize,
    csv: &mut String,
) -> usize {
    let check_new = |case: &Prepared| {
        correctness::check_plan(
            fixture.api_schema(),
            fixture.supergraph_schema(),
            fixture.subgraph_schemas(),
            &case.operation,
            &case.plan,
        )
    };
    let check_legacy = |case: &Prepared| {
        correctness::legacy::check_plan(
            fixture.api_schema(),
            fixture.supergraph_schema(),
            fixture.subgraph_schemas(),
            &case.operation,
            &case.plan,
        )
    };

    // Verdicts are taken once; only the durations are re-measured across reps.
    let verdicts: Vec<Verdicts> = cases
        .iter()
        .map(|case| {
            let new = check_new(case);
            let legacy = check_legacy(case);
            Verdicts {
                new_ok: new.is_ok(),
                legacy_ok: legacy.is_ok(),
                new_error: new.err().map(|e| e.to_string()),
                legacy_error: legacy.err().map(|e| e.to_string()),
            }
        })
        .collect();

    // Fastest rep per case, per checker. Each rep alternates who runs first.
    let mut new_best = vec![Duration::MAX; cases.len()];
    let mut legacy_best = vec![Duration::MAX; cases.len()];
    for rep in 0..reps {
        for (index, case) in cases.iter().enumerate() {
            let (new_time, legacy_time) = if (rep + index) % 2 == 0 {
                let new_time = time(|| drop(check_new(case)));
                let legacy_time = time(|| drop(check_legacy(case)));
                (new_time, legacy_time)
            } else {
                let legacy_time = time(|| drop(check_legacy(case)));
                let new_time = time(|| drop(check_new(case)));
                (new_time, legacy_time)
            };
            new_best[index] = new_best[index].min(new_time);
            legacy_best[index] = legacy_best[index].min(legacy_time);
        }
    }

    for (index, case) in cases.iter().enumerate() {
        let verdict = &verdicts[index];
        let _ = writeln!(
            csv,
            "{lane},{},{},{},{},{},{},{}",
            hex(&case.bytes),
            case.perturbation,
            if verdict.new_ok { "accept" } else { "reject" },
            if verdict.legacy_ok { "accept" } else { "reject" },
            verdict.new_ok == verdict.legacy_ok,
            new_best[index].as_micros(),
            legacy_best[index].as_micros(),
        );
    }

    let new_rejects = verdicts.iter().filter(|v| !v.new_ok).count();
    let legacy_rejects = verdicts.iter().filter(|v| !v.legacy_ok).count();
    let disagreements: Vec<usize> = (0..cases.len())
        .filter(|&i| verdicts[i].new_ok != verdicts[i].legacy_ok)
        .collect();

    println!("== {lane} lane ==");
    println!("  cases:                   {}", cases.len());
    println!("  new checker rejected:    {new_rejects}");
    println!("  legacy checker rejected: {legacy_rejects}");
    println!("  disagreements:           {}", disagreements.len());

    println!("\n  runtime over {reps} rep(s), fastest rep per case:");
    let new_stats = Stats::of(&new_best);
    let legacy_stats = Stats::of(&legacy_best);
    println!("    {:<8} {:>10} {:>10} {:>10} {:>10} {:>10}", "", "total", "mean", "median", "p90", "max");
    new_stats.print("new");
    legacy_stats.print("legacy");
    if new_stats.total.as_nanos() > 0 && legacy_stats.total.as_nanos() > 0 {
        println!(
            "    new/legacy total {:.2}x, median {:.2}x",
            new_stats.total.as_secs_f64() / legacy_stats.total.as_secs_f64(),
            new_stats.median.as_secs_f64() / legacy_stats.median.as_secs_f64(),
        );
    }

    for &index in disagreements.iter().take(20) {
        let case = &cases[index];
        let verdict = &verdicts[index];
        println!("\n  [{}] perturbation {}", hex(&case.bytes), case.perturbation);
        println!("    operation: {}", case.operation_source);
        println!(
            "    new:    {}",
            verdict.new_error.as_deref().unwrap_or("accept")
        );
        println!(
            "    legacy: {}",
            verdict.legacy_error.as_deref().unwrap_or("accept")
        );
    }
    if disagreements.len() > 20 {
        println!("\n  ... {} more", disagreements.len() - 20);
    }
    println!();

    disagreements.len()
}

fn time(mut body: impl FnMut()) -> Duration {
    let start = Instant::now();
    body();
    start.elapsed()
}

struct Stats {
    total: Duration,
    mean: Duration,
    median: Duration,
    p90: Duration,
    max: Duration,
}

impl Stats {
    fn of(durations: &[Duration]) -> Self {
        let mut sorted = durations.to_vec();
        sorted.sort_unstable();
        let total: Duration = sorted.iter().sum();
        let at = |q: f64| {
            sorted
                .get(((sorted.len() as f64 * q) as usize).min(sorted.len().saturating_sub(1)))
                .copied()
                .unwrap_or_default()
        };
        Self {
            total,
            mean: total.checked_div(sorted.len() as u32).unwrap_or_default(),
            median: at(0.5),
            p90: at(0.9),
            max: sorted.last().copied().unwrap_or_default(),
        }
    }

    fn print(&self, label: &str) {
        println!(
            "    {label:<8} {:>10} {:>10} {:>10} {:>10} {:>10}",
            format!("{:.1}ms", self.total.as_secs_f64() * 1000.0),
            format!("{:.1}us", self.mean.as_secs_f64() * 1e6),
            format!("{:.1}us", self.median.as_secs_f64() * 1e6),
            format!("{:.1}us", self.p90.as_secs_f64() * 1e6),
            format!("{:.1}us", self.max.as_secs_f64() * 1e6),
        );
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}
