use std::process::ExitCode;

use apollo_compiler::Schema;
use apollo_federation::Supergraph;

fn main() -> ExitCode {
    let (source, name) = match std::env::args().nth(1) {
        Some(filename) => (std::fs::read_to_string(&filename).unwrap(), filename),
        None => {
            return ExitCode::FAILURE;
        }
    };

    let schema = Schema::parse_and_validate(source, name).unwrap();
    let supergraph = Supergraph::from_schema(schema).unwrap();

    match supergraph.to_api_schema(Default::default()) {
        Ok(result) => println!("{}", result.schema()),
        Err(error) => {
            eprintln!("{error}");
            return ExitCode::FAILURE;
        }
    }

    ExitCode::SUCCESS
}
