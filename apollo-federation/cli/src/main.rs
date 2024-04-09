use clap::Parser;
use std::fs;
use std::io;
use std::path::PathBuf;
use std::process::ExitCode;

use apollo_compiler::ExecutableDocument;
use apollo_federation::error::FederationError;
use apollo_federation::query_graph;
use apollo_federation::query_plan::query_planner::QueryPlanner;
use apollo_federation::query_plan::query_planner::QueryPlannerConfig;
use apollo_federation::subgraph;

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
    /// Outputs the query graph from a supergraph schema or subgraph schemas
    QueryGraph {
        /// Path(s) to one supergraph schema file or multiple subgraph schemas.
        schemas: Vec<PathBuf>,
    },
    /// Outputs the federated query graph from a supergraph schema or subgraph schemas
    FederatedGraph {
        /// Path(s) to one supergraph schema file or multiple subgraph schemas.
        schemas: Vec<PathBuf>,
    },
    /// Outputs the formatted query plan for the given query and schema
    Plan {
        query: PathBuf,
        /// Path(s) to one supergraph schema file or multiple subgraph schemas.
        schemas: Vec<PathBuf>,
    },
}

fn main() -> ExitCode {
    let args = Args::parse();
    let result = match args.command {
        Command::Api { supegraph_schema } => to_api_schema(&supegraph_schema),
        Command::QueryGraph { schemas } => dot_query_graph(&schemas),
        Command::FederatedGraph { schemas } => dot_federated_graph(&schemas),
        Command::Plan { query, schemas } => plan(&query, &schemas),
    };
    match result {
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }

        Ok(_) => ExitCode::SUCCESS,
    }
}

fn read_input(input_path: &PathBuf) -> String {
    if input_path == std::path::Path::new("-") {
        io::read_to_string(io::stdin()).unwrap()
    } else {
        fs::read_to_string(input_path).unwrap()
    }
}

fn to_api_schema(input_path: &PathBuf) -> Result<(), FederationError> {
    let input = read_input(input_path);
    let supergraph = apollo_federation::Supergraph::new(&input)?;
    let api_schema = supergraph.to_api_schema(apollo_federation::ApiSchemaOptions {
        include_defer: true,
        include_stream: false,
    })?;
    println!("{}", api_schema.schema());
    Ok(())
}

/// Load either single supergraph schema file or compose one from multiple subgraph files.
fn load_supergraph(
    file_paths: &[PathBuf],
) -> Result<apollo_federation::Supergraph, FederationError> {
    if file_paths.is_empty() {
        panic!("Error: missing command arguments");
    } else if file_paths.len() == 1 {
        let doc_str = std::fs::read_to_string(&file_paths[0]).unwrap();
        apollo_federation::Supergraph::new(&doc_str)
    } else {
        let schemas: Vec<_> = file_paths
            .iter()
            .map(|pathname| {
                let doc_str = std::fs::read_to_string(pathname).unwrap();
                let url = format!("file://{}", pathname.to_str().unwrap());
                let basename = pathname.file_stem().unwrap().to_str().unwrap();
                subgraph::Subgraph::parse_and_expand(basename, &url, &doc_str).unwrap()
            })
            .collect();
        Ok(apollo_federation::Supergraph::compose(schemas.iter().collect()).unwrap())
    }
}

fn dot_query_graph(file_paths: &[PathBuf]) -> Result<(), FederationError> {
    let supergraph = load_supergraph(file_paths)?;
    let name: &str = if file_paths.len() == 1 {
        file_paths[0].file_stem().unwrap().to_str().unwrap()
    } else {
        "supergraph"
    };
    let query_graph =
        query_graph::build_query_graph::build_query_graph(name.into(), supergraph.schema)?;
    println!("{}", query_graph::output::to_dot(&query_graph));
    Ok(())
}

fn dot_federated_graph(file_paths: &[PathBuf]) -> Result<(), FederationError> {
    let supergraph = load_supergraph(file_paths)?;
    let api_schema = supergraph.to_api_schema(Default::default())?;
    let query_graph =
        query_graph::build_federated_query_graph(supergraph.schema, api_schema, None, None)?;
    println!("{}", query_graph::output::to_dot(&query_graph));
    Ok(())
}

fn plan(query_path: &PathBuf, schema_paths: &[PathBuf]) -> Result<(), FederationError> {
    let query = read_input(query_path);
    let supergraph = load_supergraph(schema_paths)?;
    let query_doc =
        ExecutableDocument::parse_and_validate(supergraph.schema.schema(), query, query_path)?;
    // TODO: add CLI parameters for config as needed
    let config = QueryPlannerConfig::default();
    let planner = QueryPlanner::new(&supergraph, config)?;
    print!("{}", planner.build_query_plan(&query_doc, None)?);
    Ok(())
}
