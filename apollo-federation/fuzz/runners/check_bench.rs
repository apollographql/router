//! Times the new query-plan checker against the legacy one on a real supergraph and operation.
//!
//! The corpus harness reports per-operation timings but cannot be attached to a profiler; this
//! runs one case in a loop so `sample`, `dtrace` or `cargo flamegraph` has something to watch.
//! Planning happens once, up front, so the timed region holds nothing but the check.
//!
//!     cargo run --release --example check_bench -- <schema.graphql> <operation.graphql> \
//!         [--reps N] [--new-only] [--loop-secs S]
//!         [--check-empty-flatten-path] [--check-requires-conflict]

use std::time::Duration;
use std::time::Instant;

use apollo_compiler::ExecutableDocument;
use apollo_federation::correctness;
use apollo_federation::query_plan::query_planner::QueryPlanner;
use apollo_federation::Supergraph;

fn main() {
    let mut args = std::env::args().skip(1);
    let (Some(schema_path), Some(operation_path)) = (args.next(), args.next()) else {
        eprintln!(
            "usage: check_bench <schema.graphql> <operation.graphql> \
             [--reps N] [--new-only] [--loop-secs S] \
             [--check-empty-flatten-path] [--check-requires-conflict]"
        );
        std::process::exit(2);
    };
    let rest: Vec<String> = args.collect();
    let flag = |name: &str| rest.iter().position(|arg| arg == name);
    let value = |name: &str, default: u64| {
        flag(name)
            .and_then(|at| rest.get(at + 1))
            .and_then(|raw| raw.parse().ok())
            .unwrap_or(default)
    };
    let reps = value("--reps", 5) as usize;
    let loop_secs = value("--loop-secs", 0);
    let new_only = flag("--new-only").is_some();

    let started = Instant::now();
    let sdl = std::fs::read_to_string(&schema_path).expect("read supergraph");
    let supergraph = Supergraph::new_with_router_specs(&sdl).expect("valid supergraph");
    // The corpus harness plans with these; fragment generation in particular changes the shape
    // of every fetch operation the checker walks, so a bench without it measures something else.
    let config = apollo_federation::query_plan::query_planner::QueryPlannerConfig {
        generate_query_fragments: flag("--no-generate-fragments").is_none(),
        type_conditioned_fetching: flag("--type-conditioned-fetching").is_some(),
        incremental_delivery:
            apollo_federation::query_plan::query_planner::QueryPlanIncrementalDeliveryConfig {
                enable_defer: true,
            },
        ..Default::default()
    };
    let planner = QueryPlanner::new(&supergraph, config).expect("planner");
    let subgraphs = supergraph
        .extract_subgraphs()
        .expect("subgraphs")
        .into_iter()
        .map(|(name, subgraph)| (name, subgraph.schema))
        .collect();
    let source = std::fs::read_to_string(&operation_path).expect("read operation");
    let document = ExecutableDocument::parse_and_validate(
        planner.api_schema().schema(),
        source,
        &operation_path,
    )
    .expect("valid operation");
    println!("setup: {:?}", started.elapsed());

    let planned = Instant::now();
    let plan = planner
        .build_query_plan(&document, None, Default::default())
        .expect("query plan");
    println!("plan:  {:?}", planned.elapsed());

    // The same invariant `metamorphic` asserts, at a scale where it can discriminate: generating
    // fragments rewrites the plan's selection sets without changing what they select, so deciding
    // the plan must cost about the same either way. It did not -- a plan written with generated
    // fragments once cost 400x the same plan written inline, with no verdict ever disagreeing.
    if flag("--compare-work").is_some() {
        let groups_for = |generate: bool| {
            let config = apollo_federation::query_plan::query_planner::QueryPlannerConfig {
                generate_query_fragments: generate,
                ..Default::default()
            };
            let planner = QueryPlanner::new(&supergraph, config).expect("planner");
            let plan = planner
                .build_query_plan(&document, None, Default::default())
                .expect("query plan");
            let before = correctness::query_compare::groups_decided();
            let verdict = correctness::check_plan(
                planner.api_schema(),
                planner.supergraph_schema(),
                &subgraphs,
                &document,
                &plan,
            );
            (
                correctness::query_compare::groups_decided() - before,
                verdict.is_ok(),
            )
        };
        let (inline, inline_ok) = groups_for(false);
        let (fragments, fragments_ok) = groups_for(true);
        let ratio = fragments as f64 / inline.max(1) as f64;
        println!("groups decided: inline {inline}, generated fragments {fragments} ({ratio:.1}x)");
        if inline_ok != fragments_ok {
            println!("VERDICT DIFFERS: inline {inline_ok}, fragments {fragments_ok}");
        }
        return;
    }

    if flag("--dump-plan").is_some() {
        println!("{plan}");
        return;
    }

    // The optional checks report planner defects that are real but not currently fixable, so
    // they are off unless asked for; see `CheckerOptions`.
    let options = correctness::CheckerOptions {
        check_empty_flatten_path_type_condition: flag("--check-empty-flatten-path").is_some(),
        check_requires_conflict: flag("--check-requires-conflict").is_some(),
    };
    let check_new = || {
        correctness::check_plan_with_options(
            planner.api_schema(),
            planner.supergraph_schema(),
            &subgraphs,
            &document,
            &plan,
            options,
        )
    };
    let check_legacy = || {
        correctness::legacy::check_plan(
            planner.api_schema(),
            planner.supergraph_schema(),
            &subgraphs,
            &document,
            &plan,
        )
    };

    if loop_secs > 0 {
        // Attach a profiler to this process while the loop runs.
        println!(
            "pid {} looping the new checker for {loop_secs}s",
            std::process::id()
        );
        let deadline = Instant::now() + Duration::from_secs(loop_secs);
        let mut runs = 0u64;
        while Instant::now() < deadline {
            let _ = check_new();
            runs += 1;
        }
        println!("runs: {runs}");
        return;
    }

    if let Err(error) = check_new() {
        println!("new checker rejects:\n{error}");
    }

    let mut new_times = Vec::new();
    let mut legacy_times = Vec::new();
    for _ in 0..reps {
        let at = Instant::now();
        let verdict = check_new();
        new_times.push(at.elapsed());
        if !new_only {
            let at = Instant::now();
            let legacy = check_legacy();
            legacy_times.push(at.elapsed());
            if verdict.is_ok() != legacy.is_ok() {
                println!("DISAGREE new={} legacy={}", verdict.is_ok(), legacy.is_ok());
                if let Err(error) = &verdict {
                    println!("new says:\n{error}");
                }
                if let Err(error) = &legacy {
                    println!("legacy says:\n{error}");
                }
            }
        }
    }
    let best = |times: &[Duration]| times.iter().min().copied().unwrap_or_default();
    println!("new:    {:?} (best of {reps})", best(&new_times));
    if !new_only {
        println!("legacy: {:?} (best of {reps})", best(&legacy_times));
        let ratio = best(&new_times).as_secs_f64() / best(&legacy_times).as_secs_f64();
        println!("ratio:  {ratio:.1}x");
    }
}
