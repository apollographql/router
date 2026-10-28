//! Reproduction for router#10054, run through both checkers.
//!
//! The issue reports `extract_boolean_hypotheses` materializing `2^K` clauses for the union
//! fallback group, where `K` is the number of distinct Boolean variables at one response key. It
//! names `compare_operations` and `check_plan` as the entry points that reach it; `check_plan` has
//! since been routed to the ported checker, so this runs the same pair through both.
//!
//!     cargo run --release --example union_case_split -- <K> <reject|accept> <new|legacy>

use std::time::Instant;

use apollo_compiler::schema::Schema;
use apollo_compiler::ExecutableDocument;
use apollo_federation::correctness;
use apollo_federation::correctness::query_compare;
use apollo_federation::schema::ValidFederationSchema;

const SCHEMA: &str = r#"
    type Query { widget: Widget! }
    type Widget { id: ID!, label: String!, vendor: Vendor! }
    type Vendor { id: ID!, name: String! }
"#;

fn main() {
    let mut args = std::env::args().skip(1);
    let k: usize = args.next().expect("K").parse().expect("K is a number");
    let arm = args.next().unwrap_or_else(|| "reject".to_string());
    let lane = args.next().unwrap_or_else(|| "new".to_string());

    let schema = ValidFederationSchema::new(
        Schema::parse_and_validate(SCHEMA, "schema.graphql").expect("valid schema"),
    )
    .expect("federation schema");

    let left = "query Left { widget { id label } }".to_string();
    let declarations = (0..k)
        .map(|i| format!("$v{i}: Boolean!"))
        .collect::<Vec<_>>()
        .join(", ");
    let variants = (0..k)
        .map(|i| format!("    label @include(if: $v{i})"))
        .collect::<Vec<_>>()
        .join("\n");
    // The accept arm adds one unconditional `label`, so the first hypothesis group already
    // settles it and the union group is built but never used.
    let unconditional = if arm == "accept" { "    label\n" } else { "" };
    let right = format!(
        "query Right({declarations}) {{\n  widget {{\n    id\n{unconditional}{variants}\n  }}\n}}"
    );

    let parse = |source: &str, name: &str| {
        ExecutableDocument::parse_and_validate(schema.schema(), source.to_string(), name)
            .expect("valid operation")
    };
    let left = parse(&left, "left.graphql");
    let right = parse(&right, "right.graphql");

    let at = Instant::now();
    let verdict = if lane == "legacy" {
        // `compare_operations` asks whether the first is a subset of the second.
        correctness::compare_operations(&schema, &left, &right)
            .map_err(|e| e.to_string())
            .map(|_| ())
    } else {
        // `includes` asks whether the first *contains* the second, so the pair is the other way up.
        query_compare::includes(&schema, &right, &left)
            .map_err(|e| e.to_string())
            .map(|_| ())
    };
    let took = at.elapsed();
    let answer = match &verdict {
        Ok(()) => "accepted".to_string(),
        Err(e) => format!("rejected ({} chars of explanation)", e.len()),
    };
    println!("K={k} arm={arm} lane={lane}: {answer} in {took:?}");
}
