use std::process::ExitCode;

use apollo_compiler::Schema;
use apollo_federation::Supergraph;
use apollo_federation::compat::coerce_and_validate_schema_values;

fn main() -> ExitCode {
    let (source, name) = match std::env::args().nth(1) {
        Some(filename) => (std::fs::read_to_string(&filename).unwrap(), filename),
        None => {
            return ExitCode::FAILURE;
        }
    };

    let mut schema = Schema::parse(source, name).unwrap();
    coerce_and_validate_schema_values(&mut schema).unwrap();
    let schema = schema.validate().unwrap();
    let supergraph = Supergraph::from_schema(
        schema,
        Some(&apollo_federation::default_supported_supergraph_specs()),
    )
    .unwrap();

    match supergraph.to_api_schema(Default::default()) {
        Ok(result) => println!("{}", result.schema()),
        Err(error) => {
            eprintln!("{error}");
            return ExitCode::FAILURE;
        }
    }

    ExitCode::SUCCESS
}
