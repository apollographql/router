//! Runs `query_compare::includes` on two documents read from disk, bypassing validation.
//!
//!     cargo run --release --example includes_pair -- <schema> <left.graphql> <right.graphql>

use apollo_compiler::validation::Valid;
use apollo_compiler::ExecutableDocument;
use apollo_federation::correctness::query_compare;
use apollo_federation::Supergraph;

fn main() {
    let mut args = std::env::args().skip(1);
    let schema_path = args.next().expect("schema");
    let left_path = args.next().expect("left");
    let right_path = args.next().expect("right");

    let sdl = std::fs::read_to_string(&schema_path).expect("read supergraph");
    let supergraph = Supergraph::new_with_router_specs(&sdl).expect("valid supergraph");
    let schema = supergraph.schema.clone();

    let read = |path: &str| {
        let source = std::fs::read_to_string(path).expect("read operation");
        // The plan checker synthesizes these with `assume_valid`, so do the same here.
        Valid::assume_valid(
            ExecutableDocument::parse(schema.schema(), source, path).expect("parses"),
        )
    };
    let left = read(&left_path);
    let right = read(&right_path);

    match query_compare::includes(&schema, &left, &right) {
        Ok(()) => println!("ACCEPT: left includes right"),
        Err(e) => println!("REJECT:\n{e}"),
    }
}
