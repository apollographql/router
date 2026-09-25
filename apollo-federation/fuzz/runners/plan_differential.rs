//! Three-lane differential for the query-plan checker.
//!
//! | lane | implementation | role |
//! | --- | --- | --- |
//! | `lean` | `Federation.checkQueryPlan` | the model, carrying the `checkQueryPlan_correct` theorem |
//! | `query_plan_check` | `correctness::check_plan` | the port under test |
//! | `legacy` | `correctness::legacy::check_plan` | an independent algorithm for the same question |
//!
//! `lean` against `query_plan_check` is a strict equality check: the port claims to decide what
//! the model decides, so a disagreement is a defect in one of them. `legacy` is advisory — it
//! decides the same question by a different route and is *known* not to ask two of the things the
//! model asks (that every entity case is covered, and that contextual data is available), so its
//! disagreements are counted and reported rather than failed on.
//!
//!     QUERY_PLAN_LEAN_ORACLE=target/query-plan-checker-lean-oracle \
//!         cargo run --release --example plan_differential -- 20000

use apollo_federation::correctness;
use query_inclusion_fuzz::plan_fixture::Fixture;
use query_inclusion_fuzz::plan_model;
use query_inclusion_fuzz::plan_oracle::PlanOracle;
use query_inclusion_fuzz::plan_oracle::PLAN_ORACLE_ENV;

fn main() {
    let budget: usize = std::env::args()
        .nth(1)
        .and_then(|value| value.parse().ok())
        .unwrap_or(20_000);

    let Some(mut oracle) = PlanOracle::from_env() else {
        eprintln!("set {PLAN_ORACLE_ENV} to the binary from scripts/build-plan-oracle.sh");
        std::process::exit(2);
    };
    let fixture = Fixture::new();
    assert_eq!(
        oracle.schema_digest(),
        plan_model::schema_digest(fixture.supergraph_schema()),
        "the two copies of the fixture have drifted apart; every verdict below would be \
         meaningless"
    );

    let mut compared = 0usize;
    let mut discarded = 0usize;
    let mut lean_accepts = 0usize;
    let mut divergences = Vec::new();
    let mut divergence_count = 0usize;
    let mut legacy_divergences = 0usize;

    for bytes in inputs(budget) {
        let lean = oracle.check(&bytes);
        let case = plan_model::decode_case(&bytes);
        // Both sides must discard the same bytes: a grammar that disagrees about what is even a
        // case is a grammar that has drifted, which the digest cannot catch on its own.
        assert_eq!(
            lean.is_none(),
            case.is_none(),
            "the two grammars disagree about whether {bytes:02x?} is a case"
        );
        let (Some(lean), Some(case)) = (lean, case) else {
            discarded += 1;
            continue;
        };
        let Some(operation) = plan_model::build_operation(fixture.api_schema(), &case) else {
            discarded += 1;
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

        compared += 1;
        if lean {
            lean_accepts += 1;
        }
        if lean != new.is_ok() {
            divergence_count += 1;
        }
        if lean != new.is_ok() && divergences.len() < 10 {
            divergences.push(format!(
                "bytes {bytes:02x?}\n  lean accepted: {lean}, port accepted: {}\n  \
                 operation: {}\n  perturbation: {:?}\n  {}",
                new.is_ok(),
                case.operation_source(),
                case.perturbation,
                new.as_ref()
                    .err()
                    .map(|e| e.to_string())
                    .unwrap_or_default(),
            ));
        }
        if lean != legacy.is_ok() {
            legacy_divergences += 1;
        }
    }

    println!("compared:            {compared}");
    println!("discarded:           {discarded}");
    println!("lean accepted:       {lean_accepts}");
    println!("port divergences:    {divergence_count}");
    println!("legacy divergences:  {legacy_divergences} (advisory)");
    if !divergences.is_empty() {
        println!("\nport against the model:");
        for line in &divergences {
            println!("- {line}");
        }
        std::process::exit(1);
    }
}

/// A deterministic spread of byte strings, so a run is reproducible without a corpus.
///
/// The grammar reads six bytes at most, and a counter over them covers the whole space long
/// before the default budget runs out; libFuzzer coverage-guides the same grammar through
/// `fuzz_targets/plan_differential.rs`.
fn inputs(budget: usize) -> Vec<Vec<u8>> {
    let mut inputs = Vec::with_capacity(budget);
    let mut state: u64 = 0x2545_f491_4f6c_dd1d;
    for index in 0..budget {
        // The first bytes walk the space in order so short runs still cover it; the rest are
        // drawn from a cheap PRNG so a long run reaches combinations the counter would take
        // longer to get to.
        let bytes: Vec<u8> = if index < 4096 {
            (0..6)
                .map(|slot| (index >> (slot * 3)) as u8 & 0x07)
                .collect()
        } else {
            (0..6)
                .map(|_| {
                    state ^= state << 13;
                    state ^= state >> 7;
                    state ^= state << 17;
                    state as u8
                })
                .collect()
        };
        inputs.push(bytes);
    }
    inputs
}
