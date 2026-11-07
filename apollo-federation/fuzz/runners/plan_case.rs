//! Prints one case of the plan grammar: the operation, the plan, and what each checker decides,
//! with the port's reason when it rejects. The Lean verdict is added when the oracle is set.
//!
//!     cargo run --release --example plan_case -- 0104000017
//!
//! The argument is the case's bytes in hex, as the other runners print them. An argument starting
//! with `{` or `query` is an operation instead, planned by the real planner.
//!
//!     cargo run --release --example plan_case -- '{ is { h } }'

use apollo_federation::correctness;
use query_inclusion_fuzz::plan_fixture::Fixture;
use query_inclusion_fuzz::plan_model;
use query_inclusion_fuzz::plan_oracle::PlanOracle;

fn main() {
    let hex = std::env::args().nth(1).expect("the case's bytes in hex");
    if hex.starts_with('{') || hex.starts_with("query") {
        return plan_operation(&hex);
    }
    let bytes: Vec<u8> = (0..hex.len())
        .step_by(2)
        .map(|at| u8::from_str_radix(&hex[at..at + 2], 16).expect("hex"))
        .collect();
    let case = plan_model::decode_case(&bytes).expect("the bytes decode to a case");
    let fixture = Fixture::new();
    let operation =
        plan_model::build_operation(fixture.api_schema(), &case).expect("a valid operation");
    let plan = plan_model::build_plan(fixture.subgraph_schemas(), &case);
    println!("perturbation: {:?}", case.perturbation);
    println!("operation:    {}", case.operation_source());
    println!("plan:\n{plan}");
    if let Some(mut oracle) = PlanOracle::from_env() {
        println!("lean:         {}", oracle.check(&bytes).expect("a case"));
    }
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
    println!("port:         {}", new.is_ok());
    if let Err(error) = &new {
        println!("{error}");
    }
    println!("legacy:       {}", legacy.is_ok());
}

/// Plans `source` with the real planner and checks the plan.
fn plan_operation(source: &str) {
    let fixture = Fixture::new();
    let operation = apollo_compiler::ExecutableDocument::parse_and_validate(
        fixture.api_schema().schema(),
        source,
        "operation.graphql",
    )
    .expect("a valid operation");
    let plan = fixture
        .planner()
        .build_query_plan(&operation, None, Default::default())
        .expect("a plan");
    println!("plan:\n{plan}");
    let new = correctness::check_plan(
        fixture.api_schema(),
        fixture.supergraph_schema(),
        fixture.subgraph_schemas(),
        &operation,
        &plan,
    );
    println!("port:         {}", new.is_ok());
    if let Err(error) = &new {
        println!("{error}");
    }
}
