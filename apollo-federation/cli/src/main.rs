use clap::Parser;
use std::fs;
use std::io;
use std::path::PathBuf;

/// CLI arguments. See <https://docs.rs/clap/latest/clap/_derive/index.html>
#[derive(Parser)]
struct Args {
    #[command(subcommand)]
    command: Command,
}

#[derive(clap::Subcommand)]
enum Command {
    /// Converts a supergraph schema to the corresponding API schema
    Api {
        /// The path to the supegraph schema file, or `-` for stdin
        supegraph_schema: PathBuf,
    },
}

fn main() {
    let args = Args::parse();
    match args.command {
        Command::Api { supegraph_schema } => to_api_schema(supegraph_schema),
    }
}

fn to_api_schema(input_path: PathBuf) {
    let input = if input_path == std::path::Path::new("-") {
        io::read_to_string(io::stdin()).unwrap()
    } else {
        fs::read_to_string(input_path).unwrap()
    };
    let supergraph = apollo_federation::Supergraph::new(&input).unwrap();
    let api_schema = supergraph
        .to_api_schema(apollo_federation::ApiSchemaOptions {
            include_defer: true,
            include_stream: false,
        })
        .unwrap();
    println!("{}", api_schema.schema())
}
