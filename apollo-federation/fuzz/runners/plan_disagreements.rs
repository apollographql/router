//! Distils the whole grammar's disagreements into the operation pairs behind them.
//!
//! Sweeps every case the grammar can produce and splits the disagreements by which half of
//! `checkQueryPlan` they are in, since the two need different artifacts to reproduce.
//!
//! **Completeness** disagreements are about two operations. Both operands are taken *from the
//! oracle*, so the pair is evidence rather than a transcription, and each is re-checked with
//! `query_compare::includes` alone: one that still disagrees has no query-plan machinery left in
//! it and is an inclusion test on either side.
//!
//! **Soundness** disagreements are about a fetch in a plan, so the completeness operands say
//! nothing about them. Those get their own pair — what the fetch has already fetched, and what its
//! `@key` demands of it — which is the inclusion test `checkRequirementMatchesCase` makes.
//!
//!     QUERY_PLAN_LEAN_ORACLE=target/query-plan-checker-lean-oracle \
//!         cargo run --release --example plan_disagreements

use std::collections::BTreeMap;

use apollo_compiler::ExecutableDocument;
use apollo_federation::correctness;
use apollo_federation::correctness::query_compare;
use query_inclusion_fuzz::plan_fixture::Fixture;
use query_inclusion_fuzz::plan_model;
use query_inclusion_fuzz::plan_oracle::PlanOracle;
use query_inclusion_fuzz::plan_oracle::PLAN_ORACLE_ENV;

/// One distinct disagreement.
struct Disagreement {
    /// The shortest byte string that reaches it, for re-running the other diagnostics.
    bytes: Vec<u8>,
    perturbation: String,
    operation: String,
    /// What `includes` alone says about the two operands; completeness disagreements only.
    includes_alone: Option<bool>,
}

fn main() {
    let Some(mut oracle) = PlanOracle::from_env() else {
        eprintln!("set {PLAN_ORACLE_ENV} to the binary from scripts/build-plan-oracle.sh");
        std::process::exit(2);
    };
    let fixture = Fixture::new();
    assert_eq!(
        oracle.schema_digest(),
        plan_model::schema_digest(fixture.supergraph_schema()),
        "the two copies of the fixture have drifted apart"
    );

    let mut completeness: BTreeMap<(String, String), Disagreement> = BTreeMap::new();
    let mut soundness: BTreeMap<(String, String), Disagreement> = BTreeMap::new();
    let mut cases = 0usize;
    let mut divergences = 0usize;

    for bytes in plan_model::enumerate_inputs() {
        let Some(case) = plan_model::decode_case(&bytes) else {
            continue;
        };
        let Some(operation) = plan_model::build_operation(fixture.api_schema(), &case) else {
            continue;
        };
        let plan = plan_model::build_plan(fixture.subgraph_schemas(), &case);
        let Some(lean) = oracle.check(&bytes) else {
            continue;
        };
        let port = correctness::check_plan(
            fixture.api_schema(),
            fixture.supergraph_schema(),
            fixture.subgraph_schemas(),
            &operation,
            &plan,
        )
        .is_ok();

        cases += 1;
        if lean == port {
            continue;
        }
        divergences += 1;

        let Some((complete, sound)) = oracle.halves(&bytes) else {
            continue;
        };
        let Some((left, right)) = oracle.operands(&bytes) else {
            continue;
        };
        // A case can fail both halves; the completeness one is the reducible artifact, so it wins.
        if !complete {
            completeness
                .entry((left.clone(), right.clone()))
                .or_insert_with(|| Disagreement {
                    bytes: bytes.clone(),
                    perturbation: format!("{:?}", case.perturbation),
                    operation: case.operation_source(),
                    includes_alone: includes_alone(&fixture, &left, &right),
                });
        } else if !sound {
            let Some((available, required)) = oracle.requirement(&bytes) else {
                continue;
            };
            soundness
                .entry((available.clone(), required.clone()))
                .or_insert_with(|| Disagreement {
                    bytes: bytes.clone(),
                    perturbation: format!("{:?}", case.perturbation),
                    operation: case.operation_source(),
                    includes_alone: includes_alone(&fixture, &available, &required),
                });
        }
    }

    println!("cases: {cases}");
    println!("divergences: {divergences}");
    println!(
        "distinct: {} completeness, {} soundness\n",
        completeness.len(),
        soundness.len()
    );

    let mut pairs: Vec<((String, String), Disagreement)> = completeness.into_iter().collect();
    pairs.sort_by_key(|((left, right), _)| (left.len() + right.len(), left.clone()));

    println!("== completeness: `includes` disagrees about these two operations ==\n");
    for (index, ((left, right), disagreement)) in pairs.iter().enumerate() {
        println!(
            "[C{}] bytes {}  perturbation {}  includes alone: {}",
            index + 1,
            hex(&disagreement.bytes),
            disagreement.perturbation,
            match disagreement.includes_alone {
                Some(true) => "true (lean says false)",
                Some(false) => "false (agrees; the plan walk built different operands)",
                None => "(operands did not parse)",
            }
        );
        println!("     left:  {}", declared(left));
        println!("     right: {}\n", declared(right));
    }

    let mut sound_pairs: Vec<((String, String), Disagreement)> = soundness.into_iter().collect();
    sound_pairs.sort_by_key(|((left, right), _)| (left.len() + right.len(), left.clone()));

    println!("== soundness: `includes` disagrees about what a `@key` demands ==\n");
    for (index, ((left, right), disagreement)) in sound_pairs.iter().enumerate() {
        println!(
            "[S{}] bytes {}  perturbation {}  includes alone: {}",
            index + 1,
            hex(&disagreement.bytes),
            disagreement.perturbation,
            match disagreement.includes_alone {
                Some(true) => "true (lean says false)",
                Some(false) => "false (agrees; the walk built different operands)",
                None => "(operands did not parse)",
            }
        );
        println!("     operation:  {}", disagreement.operation);
        println!("     available:  {}", declared(left));
        println!("     demanded:   {}\n", declared(right));
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Wraps a selection set in an operation declaring exactly the variables it uses.
fn declared(selections: &str) -> String {
    let used: Vec<String> = plan_model::VARIABLES
        .iter()
        .filter(|variable| selections.contains(&format!("${variable}")))
        .map(|variable| format!("${variable}: Boolean!"))
        .collect();
    if used.is_empty() {
        format!("query {{ {selections} }}")
    } else {
        format!("query({}) {{ {selections} }}", used.join(", "))
    }
}

/// `query_compare::includes` on the two rendered operands, with the plan machinery gone.
///
/// The operands come from the oracle as selection sets; both variables are declared so either
/// rendering parses, and declaring one the operation does not use would make it invalid.
fn includes_alone(fixture: &Fixture, left: &str, right: &str) -> Option<bool> {
    let schema = fixture.supergraph_schema();
    let parse = |selections: &str| {
        ExecutableDocument::parse_and_validate(
            schema.schema(),
            declared(selections),
            "operand.graphql",
        )
        .ok()
    };
    let left = parse(left)?;
    let right = parse(right)?;
    Some(query_compare::includes(schema, &left, &right).is_ok())
}
