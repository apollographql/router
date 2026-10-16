//! Runs `query_compare::includes` on two operations given as text, against the plan fixture's
//! supergraph schema.
//!
//! This is the bottom of the plan lane: when the completeness half disagrees with the model, the
//! two operands can be rendered from each side and compared here, with no plan machinery left in
//! the way.
//!
//! A third argument names a supergraph SDL file to use instead of the plan fixture's, so a case
//! from a real graph can be reduced the same way.
//!
//!     cargo run --release --example plan_probe -- 'query { ... }' 'query { ... }' [schema.graphql]

use apollo_compiler::ExecutableDocument;
use apollo_federation::correctness::query_compare;
use query_inclusion_fuzz::plan_fixture::Fixture;

fn main() {
    let mut args = std::env::args().skip(1);
    let (Some(left), Some(right)) = (args.next(), args.next()) else {
        eprintln!("usage: plan_probe <left operation> <right operation>");
        std::process::exit(2);
    };
    let loaded = args.next().map(|path| {
        let sdl = std::fs::read_to_string(&path).expect("read supergraph");
        apollo_federation::Supergraph::new_with_router_specs(&sdl).expect("valid supergraph")
    });
    let fixture;
    let schema = match &loaded {
        Some(supergraph) => &supergraph.schema,
        None => {
            fixture = Fixture::new();
            fixture.supergraph_schema()
        }
    };
    let parse = |source: &str, what: &str| {
        ExecutableDocument::parse_and_validate(schema.schema(), source, "probe.graphql")
            .unwrap_or_else(|error| panic!("{what} operation is not valid:\n{error}"))
    };
    let left = parse(&left, "left");
    let right = parse(&right, "right");
    match query_compare::includes(schema, &left, &right) {
        Ok(()) => println!("includes: true"),
        Err(error) => println!("includes: false\n{error}"),
    }
}
