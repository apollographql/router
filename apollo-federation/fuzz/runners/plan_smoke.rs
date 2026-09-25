//! Walks the whole query-plan grammar and reports what each checker decides.
//!
//! No Lean and no fuzzer: this enumerates every case the grammar can produce and cross-checks
//! three things — the grammar's own claim about whether a case should be correct, the new
//! checker's verdict, and the legacy one's. It is what the differential lane is calibrated
//! against, and it runs in the ordinary test loop.
//!
//!     cargo run --release --example plan_smoke

use std::collections::BTreeMap;

use apollo_federation::correctness;
use query_inclusion_fuzz::plan_fixture::Fixture;
use query_inclusion_fuzz::plan_model;

fn main() {
    let fixture = Fixture::new();
    println!("{}", plan_model::schema_digest(fixture.supergraph_schema()));

    let mut cases = 0usize;
    let mut new_rejects = 0usize;
    let mut legacy_rejects = 0usize;
    let mut divergences = 0usize;
    let mut unexpected: Vec<String> = Vec::new();
    let mut by_perturbation: BTreeMap<String, (usize, usize, usize)> = BTreeMap::new();

    for bytes in plan_model::enumerate_inputs() {
        let Some(case) = plan_model::decode_case(&bytes) else {
            continue;
        };
        let Some(operation) = plan_model::build_operation(fixture.api_schema(), &case) else {
            continue;
        };
        let plan = plan_model::build_plan(fixture.subgraph_schemas(), &case);

        let new = correctness::check_plan(
            fixture.api_schema(),
            fixture.supergraph_schema(),
            fixture.subgraph_schemas(),
            &operation,
            &plan,
        );
        let legacy = correctness::legacy::check_plan(
            fixture.api_schema(),
            fixture.supergraph_schema(),
            fixture.subgraph_schemas(),
            &operation,
            &plan,
        );

        cases += 1;
        let must_accept = plan_model::must_be_accepted(&case);
        let entry = by_perturbation
            .entry(format!("{:?}", case.perturbation))
            .or_default();
        entry.0 += 1;
        if new.is_err() {
            new_rejects += 1;
            entry.1 += 1;
        }
        if legacy.is_err() {
            legacy_rejects += 1;
            entry.2 += 1;
        }
        if new.is_err() != legacy.is_err() {
            divergences += 1;
        }
        if must_accept && new.is_err() && unexpected.len() < 5 {
            unexpected.push(format!(
                "the plan built for this operation was rejected\n  operation: {}\n  {}",
                case.operation_source(),
                new.as_ref()
                    .err()
                    .map(|e| e.to_string())
                    .unwrap_or_default(),
            ));
        }
    }

    println!("cases: {cases}");
    println!("new checker rejected:    {new_rejects}");
    println!("legacy checker rejected: {legacy_rejects}");
    println!("divergences:             {divergences}");
    println!("\nper perturbation (cases / new rejects / legacy rejects):");
    for (name, (total, new, legacy)) in &by_perturbation {
        println!("  {name:<20} {total:>5} {new:>5} {legacy:>5}");
    }
    planner_lane(&fixture);

    if unexpected.is_empty() {
        println!("\nevery unperturbed plan was accepted");
    } else {
        println!("\nunperturbed plans rejected:");
        for line in &unexpected {
            println!("- {line}");
        }
        std::process::exit(1);
    }
}

/// The planner-driven lane: plan each generated operation for real and check the result.
///
/// These are the only plans that are correct by construction rather than by the miniature planner
/// agreeing with itself, so a rejection here is a false positive in the checker under test.
fn planner_lane(fixture: &Fixture) {
    let mut planned = 0usize;
    let mut new_rejects = Vec::new();
    let mut legacy_rejects = 0usize;
    let mut seen = std::collections::BTreeSet::new();

    for bytes in plan_model::enumerate_inputs() {
        let Some(case) = plan_model::decode_case(&bytes) else {
            continue;
        };
        if !seen.insert(case.operation_source()) {
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
        planned += 1;
        if let Err(error) = correctness::check_plan(
            fixture.api_schema(),
            fixture.supergraph_schema(),
            fixture.subgraph_schemas(),
            &operation,
            &plan,
        ) {
            if new_rejects.len() < 5 {
                new_rejects.push(format!(
                    "  operation: {}\n  {error}",
                    case.operation_source()
                ));
            }
        }
        if correctness::legacy::check_plan(
            fixture.api_schema(),
            fixture.supergraph_schema(),
            fixture.subgraph_schemas(),
            &operation,
            &plan,
        )
        .is_err()
        {
            legacy_rejects += 1;
        }
    }

    println!("\nplanner-driven lane");
    println!("  real plans checked:      {planned}");
    println!("  new checker rejected:    {}", new_rejects.len());
    println!("  legacy checker rejected: {legacy_rejects}");
    for line in &new_rejects {
        println!("{line}");
    }
}
