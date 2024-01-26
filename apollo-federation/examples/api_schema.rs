use apollo_compiler::Schema;
use apollo_federation::Supergraph;
use std::process::ExitCode;

fn main() -> ExitCode {
    let (source, name) = match std::env::args().nth(1) {
        Some(filename) => (std::fs::read_to_string(&filename).unwrap(), filename),
        None => {
            return ExitCode::FAILURE;
        }
    };

    let schema = Schema::parse_and_validate(source, name).unwrap();
    let supergraph = Supergraph::from(schema);

    match supergraph.to_api_schema() {
        Ok(result) => println!("{result}"),
        Err(error) => {
            eprintln!("{error}");
            return ExitCode::FAILURE;
        }
    }

    ExitCode::SUCCESS
}
