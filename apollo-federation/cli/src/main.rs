use std::fs;
use std::io;
use std::num::NonZeroU32;
use std::ops::Range;
use std::path::Path;
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::Error as AnyError;
use anyhow::anyhow;
use apollo_compiler::ExecutableDocument;
use apollo_compiler::parser::LineColumn;
use apollo_federation::ApiSchemaOptions;
use apollo_federation::Supergraph;
use apollo_federation::bail;
use apollo_federation::composition;
use apollo_federation::composition::validate_satisfiability;
use apollo_federation::connectors::expand::ExpansionResult;
use apollo_federation::connectors::expand::expand_connectors;
use apollo_federation::error::CompositionError;
use apollo_federation::error::FederationError;
use apollo_federation::error::SingleFederationError;
use apollo_federation::error::SubgraphLocation;
use apollo_federation::internal_composition_api;
use apollo_federation::query_graph;
use apollo_federation::query_plan::query_planner::QueryPlanner;
use apollo_federation::query_plan::query_planner::QueryPlannerConfig;
use apollo_federation::subgraph::SubgraphError;
use apollo_federation::subgraph::typestate;
use apollo_federation::supergraph as new_supergraph;
use clap::Parser;
use tracing_subscriber::prelude::*;

mod bench;
use bench::BenchOutput;
use bench::run_bench;

#[derive(Parser)]
struct QueryPlannerArgs {
    /// Enable @defer support.
    #[arg(long, default_value_t = false)]
    enable_defer: bool,
    /// Generate fragments to compress subgraph queries.
    #[arg(long, default_value_t = false)]
    generate_fragments: bool,
    /// Enable type conditioned fetching.
    #[arg(long, default_value_t = false)]
    type_conditioned_fetching: bool,
    /// Run GraphQL validation check on generated subgraph queries. (default: true)
    #[arg(long, default_missing_value = "true", require_equals = true, num_args = 0..=1)]
    subgraph_validation: Option<bool>,
    /// Set the `debug.max_evaluated_plans` option.
    #[arg(long)]
    max_evaluated_plans: Option<NonZeroU32>,
    /// Set the `debug.paths_limit` option.
    #[arg(long)]
    paths_limit: Option<u32>,
}

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
        /// Path(s) to one supergraph schema file, `-` for stdin or multiple subgraph schemas.
        schemas: Vec<PathBuf>,
        /// Enable @defer support.
        #[arg(long, default_value_t = false)]
        enable_defer: bool,
    },
    /// Outputs the query graph from a supergraph schema or subgraph schemas
    QueryGraph {
        /// Path(s) to one supergraph schema file, `-` for stdin or multiple subgraph schemas.
        schemas: Vec<PathBuf>,
    },
    /// Outputs the federated query graph from a supergraph schema or subgraph schemas
    FederatedGraph {
        /// Path(s) to one supergraph schema file, `-` for stdin or multiple subgraph schemas.
        schemas: Vec<PathBuf>,
    },
    /// Outputs the formatted query plan for the given query and schema
    Plan {
        #[arg(long)]
        json: bool,
        query: PathBuf,
        /// Path(s) to one supergraph schema file, `-` for stdin or multiple subgraph schemas.
        schemas: Vec<PathBuf>,
        #[command(flatten)]
        planner: QueryPlannerArgs,
    },
    /// Validate one supergraph schema file or multiple subgraph schemas
    Validate {
        /// Path(s) to one supergraph schema file, `-` for stdin or multiple subgraph schemas.
        schemas: Vec<PathBuf>,
    },
    /// Compose a supergraph schema from multiple subgraph schemas
    Compose {
        /// Path(s) to subgraph schemas.
        schemas: Vec<PathBuf>,
    },
    /// Expand and validate a subgraph schema and print the result
    Subgraph {
        /// The path to the subgraph schema file, or `-` for stdin
        subgraph_schema: PathBuf,
    },
    /// Validate the satisfiability of a supergraph schema
    Satisfiability {
        /// The path to the supergraph schema file, or `-` for stdin
        supergraph_schema: PathBuf,
    },
    /// Extract subgraph schemas from a supergraph schema to stdout (or in a directory if specified)
    Extract {
        /// The path to the supergraph schema file, or `-` for stdin
        supergraph_schema: PathBuf,
        /// The output directory for the extracted subgraph schemas
        destination_dir: Option<PathBuf>,
    },
    Bench {
        /// The path to the supergraph schema file
        supergraph_schema: PathBuf,
        /// The path to the directory that contains all operations to run against
        operations_dir: PathBuf,
        #[command(flatten)]
        planner: QueryPlannerArgs,
    },

    /// Expand connector-enabled supergraphs
    Expand {
        /// The path to the supergraph schema file, or `-` for stdin
        supergraph_schema: PathBuf,

        /// The output directory for the extracted subgraph schemas
        destination_dir: Option<PathBuf>,

        /// An optional prefix to match against expanded subgraph names
        #[arg(long)]
        filter_prefix: Option<String>,
    },
}

impl QueryPlannerArgs {
    fn apply(&self, config: &mut QueryPlannerConfig) {
        config.incremental_delivery.enable_defer = self.enable_defer;
        config.generate_query_fragments = self.generate_fragments;
        config.type_conditioned_fetching = self.type_conditioned_fetching;
        config.subgraph_graphql_validation = self.subgraph_validation.unwrap_or(true);
        if let Some(max_evaluated_plans) = self.max_evaluated_plans {
            config.debug.max_evaluated_plans = max_evaluated_plans;
        }
        config.debug.paths_limit = self.paths_limit;
    }
}

impl From<QueryPlannerArgs> for QueryPlannerConfig {
    fn from(value: QueryPlannerArgs) -> Self {
        let mut config = QueryPlannerConfig::default();
        value.apply(&mut config);
        config
    }
}

/// Set up the tracing subscriber
fn init_tracing() {
    let fmt_layer = tracing_subscriber::fmt::layer()
        .without_time()
        .with_target(false);
    let filter_layer = tracing_subscriber::EnvFilter::from_default_env();
    tracing_subscriber::registry()
        .with(fmt_layer)
        .with(filter_layer)
        .init();
}

fn main() -> ExitCode {
    init_tracing();
    let args = Args::parse();
    let result = match args.command {
        Command::Api {
            schemas,
            enable_defer,
        } => cmd_api_schema(&schemas, enable_defer),
        Command::QueryGraph { schemas } => cmd_query_graph(&schemas),
        Command::FederatedGraph { schemas } => cmd_federated_graph(&schemas),
        Command::Plan {
            json,
            query,
            schemas,
            planner,
        } => cmd_plan(json, &query, &schemas, planner),
        Command::Validate { schemas } => cmd_validate(&schemas),
        Command::Subgraph { subgraph_schema } => cmd_subgraph(&subgraph_schema),
        Command::Satisfiability { supergraph_schema } => cmd_satisfiability(&supergraph_schema),
        Command::Compose { schemas } => cmd_compose(&schemas),
        Command::Extract {
            supergraph_schema,
            destination_dir,
        } => cmd_extract(&supergraph_schema, destination_dir.as_ref()),
        Command::Bench {
            supergraph_schema,
            operations_dir,
            planner,
        } => cmd_bench(&supergraph_schema, &operations_dir, planner),
        Command::Expand {
            supergraph_schema,
            destination_dir,
            filter_prefix,
        } => cmd_expand(
            &supergraph_schema,
            destination_dir.as_ref(),
            filter_prefix.as_deref(),
        ),
    };
    match result {
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }

        Ok(_) => ExitCode::SUCCESS,
    }
}

fn read_input(input_path: &Path) -> String {
    if input_path == std::path::Path::new("-") {
        io::read_to_string(io::stdin()).unwrap()
    } else {
        fs::read_to_string(input_path).unwrap()
    }
}

fn cmd_api_schema(file_paths: &[PathBuf], enable_defer: bool) -> Result<(), AnyError> {
    let supergraph = load_supergraph(file_paths)?;
    let api_schema = supergraph.to_api_schema(apollo_federation::ApiSchemaOptions {
        include_defer: enable_defer,
        include_stream: false,
    })?;
    println!("{}", api_schema.schema());
    Ok(())
}

fn compose_files_inner(
    file_paths: &[PathBuf],
) -> Result<composition::Supergraph<composition::Satisfiable>, Vec<CompositionError>> {
    let mut subgraphs = Vec::new();
    let mut errors = Vec::new();
    for path in file_paths {
        let doc_str = std::fs::read_to_string(path).unwrap();
        let url = format!("file://{}", path.to_str().unwrap());
        let basename = path.file_stem().unwrap().to_str().unwrap();
        let result = typestate::Subgraph::parse(basename, &url, &doc_str);
        match result {
            Ok(subgraph) => {
                subgraphs.push(subgraph);
            }
            Err(err) => {
                errors.push(err);
            }
        }
    }
    if !errors.is_empty() {
        // Subgraph errors
        let mut composition_errors = Vec::new();
        for error in errors {
            composition_errors.extend(error.to_composition_errors());
        }
        return Err(composition_errors);
    }

    composition::compose(subgraphs)
}

/// Compose a supergraph from multiple subgraph files.
fn compose_files(
    file_paths: &[PathBuf],
) -> Result<composition::Supergraph<composition::Satisfiable>, AnyError> {
    match compose_files_inner(file_paths) {
        Ok(supergraph) => Ok(supergraph),
        Err(errors) => {
            // Print composition errors
            print_composition_errors(&errors);
            let num_errors = errors.len();
            Err(anyhow!("Error: found {num_errors} composition error(s)."))
        }
    }
}

fn print_composition_errors(errors: &[CompositionError]) {
    for error in errors {
        eprintln!(
            "{code}: {message}",
            code = error.code().definition().code(),
            message = error
        );
        print_subgraph_locations(error.locations());
        eprintln!(); // line break
    }
}

fn print_subgraph_locations(locations: &[SubgraphLocation]) {
    if locations.is_empty() {
        eprintln!("locations: <unknown>");
    } else {
        eprintln!("locations:");
        for loc in locations {
            eprintln!(
                "  [{subgraph}] {start_line}:{start_column} - {end_line}:{end_column}",
                subgraph = loc.subgraph,
                start_line = loc.range.start.line,
                start_column = loc.range.start.column,
                end_line = loc.range.end.line,
                end_column = loc.range.end.column,
            );
        }
    }
}

fn load_supergraph_file(
    file_path: &Path,
) -> Result<apollo_federation::Supergraph, FederationError> {
    let doc_str = read_input(file_path);
    apollo_federation::Supergraph::new_with_router_specs(&doc_str)
}

/// Load either single supergraph schema file or compose one from multiple subgraph files.
/// If the single file is "-", read from stdin.
fn load_supergraph(file_paths: &[PathBuf]) -> Result<apollo_federation::Supergraph, AnyError> {
    let supergraph = if file_paths.is_empty() {
        bail!("Error: missing command arguments");
    } else if file_paths.len() == 1 {
        load_supergraph_file(&file_paths[0])?
    } else {
        let supergraph = compose_files(file_paths)?;
        // Convert the new Supergraph struct into the old one.
        let schema_doc = supergraph.schema().schema().to_string();
        apollo_federation::Supergraph::new_with_router_specs(&schema_doc)?
    };
    Ok(supergraph)
}

fn cmd_query_graph(file_paths: &[PathBuf]) -> Result<(), AnyError> {
    let supergraph = load_supergraph(file_paths)?;
    let api_schema = supergraph.to_api_schema(Default::default())?;
    let query_graph = query_graph::build_supergraph_api_query_graph(supergraph.schema, api_schema)?;
    println!("{}", query_graph::output::to_dot(&query_graph));
    Ok(())
}

fn cmd_federated_graph(file_paths: &[PathBuf]) -> Result<(), AnyError> {
    let supergraph = load_supergraph(file_paths)?;
    let api_schema = supergraph.to_api_schema(Default::default())?;
    let query_graph =
        query_graph::build_federated_query_graph(supergraph.schema, api_schema, None, None)?;
    println!("{}", query_graph::output::to_dot(&query_graph));
    Ok(())
}

fn cmd_plan(
    use_json: bool,
    query_path: &Path,
    schema_paths: &[PathBuf],
    planner: QueryPlannerArgs,
) -> Result<(), AnyError> {
    let query = read_input(query_path);
    let supergraph = load_supergraph(schema_paths)?;

    let config = QueryPlannerConfig::from(planner);
    let planner = QueryPlanner::new(&supergraph, config)?;

    let query_doc =
        ExecutableDocument::parse_and_validate(planner.api_schema().schema(), query, query_path)
            .map_err(FederationError::from)?;
    let query_plan = planner.build_query_plan(&query_doc, None, Default::default())?;
    if use_json {
        println!("{}", serde_json::to_string_pretty(&query_plan).unwrap());
    } else {
        println!("{query_plan}");
    }

    // Check the query plan
    let subgraphs_by_name = supergraph
        .extract_subgraphs()
        .unwrap()
        .into_iter()
        .map(|(name, subgraph)| (name, subgraph.schema))
        .collect();
    let result = apollo_federation::correctness::check_plan(
        planner.api_schema(),
        &supergraph.schema,
        &subgraphs_by_name,
        &query_doc,
        &query_plan,
    );
    match result {
        Ok(_) => Ok(()),
        Err(err) => Err(anyhow!("{err}")),
    }
}

fn cmd_validate(file_paths: &[PathBuf]) -> Result<(), AnyError> {
    load_supergraph(file_paths)?;
    println!("[SUCCESS]");
    Ok(())
}

fn subgraph_parse_and_validate(
    name: &str,
    url: &str,
    doc_str: &str,
) -> Result<typestate::Subgraph<typestate::Validated>, SubgraphError> {
    typestate::Subgraph::parse(name, url, doc_str)?
        .expand_links()?
        .assume_upgraded()
        .validate()
}

fn cmd_subgraph(file_path: &Path) -> Result<(), AnyError> {
    let doc_str = read_input(file_path);
    let name = file_path
        .file_stem()
        .and_then(|name| name.to_str().map(|x| x.to_string()))
        .unwrap_or_else(|| "subgraph".to_string());
    let url = format!("http://{name}");
    let subgraph = match subgraph_parse_and_validate(&name, &url, &doc_str) {
        Ok(subgraph) => subgraph,
        Err(err) => {
            let composition_errors: Vec<_> = err.to_composition_errors().collect();
            print_composition_errors(&composition_errors);
            let num_errors = composition_errors.len();
            return Err(anyhow!(
                "Error: found {num_errors} error(s) in subgraph schema"
            ));
        }
    };

    // Extra subgraph validation for @cacheTag directive
    let result = internal_composition_api::validate_cache_tag_directives(&name, &url, &doc_str)?;
    if !result.errors.is_empty() {
        for err in &result.errors {
            eprintln!(
                "{code}: {message}",
                code = err.code(),
                message = err.message()
            );
            print_locations(&err.locations);
            eprintln!(); // line break
        }
        let num_errors = result.errors.len();
        return Err(anyhow!(
            "Error: found {num_errors} error(s) in subgraph schema"
        ));
    }

    println!("{}", subgraph.schema_string());
    Ok(())
}

fn print_locations(locations: &[Range<LineColumn>]) {
    if locations.is_empty() {
        eprintln!("locations: <unknown>");
    } else {
        eprintln!("locations:");
        for loc in locations {
            eprintln!(
                "  {start_line}:{start_column} - {end_line}:{end_column}",
                start_line = loc.start.line,
                start_column = loc.start.column,
                end_line = loc.end.line,
                end_column = loc.end.column,
            );
        }
    }
}

fn cmd_satisfiability(file_path: &Path) -> Result<(), AnyError> {
    let doc_str = read_input(file_path);
    let supergraph = new_supergraph::Supergraph::parse(&doc_str).unwrap();
    _ = validate_satisfiability(supergraph).expect("Supergraph should be satisfiable");
    Ok(())
}

fn cmd_compose(file_paths: &[PathBuf]) -> Result<(), AnyError> {
    let supergraph = compose_files(file_paths)?;
    println!("{}", supergraph.schema().schema());
    let hints = supergraph.hints();
    if !hints.is_empty() {
        eprintln!("{num_hints} HINTS generated:", num_hints = hints.len());
        for hint in hints {
            eprintln!(); // line break
            eprintln!(
                "{code}: {message}",
                code = hint.code(),
                message = hint.message()
            );
            print_subgraph_locations(&hint.locations);
        }
    }
    Ok(())
}

fn cmd_extract(file_path: &Path, dest: Option<&PathBuf>) -> Result<(), AnyError> {
    let supergraph = load_supergraph_file(file_path)?;
    let subgraphs = supergraph.extract_subgraphs()?;
    if let Some(dest) = dest {
        fs::create_dir_all(dest).map_err(|_| SingleFederationError::Internal {
            message: "Error: directory creation failed".into(),
        })?;
        for (name, subgraph) in subgraphs {
            let subgraph_path = dest.join(format!("{}.graphql", name));
            fs::write(subgraph_path, subgraph.schema.schema().to_string()).map_err(|_| {
                SingleFederationError::Internal {
                    message: "Error: file output failed".into(),
                }
            })?;
        }
    } else {
        for (name, subgraph) in subgraphs {
            println!("[Subgraph `{}`]", name);
            println!("{}", subgraph.schema.schema());
            println!(); // newline
        }
    }
    Ok(())
}

fn cmd_expand(
    file_path: &Path,
    dest: Option<&PathBuf>,
    filter_prefix: Option<&str>,
) -> Result<(), AnyError> {
    let original_supergraph = load_supergraph_file(file_path)?;
    let ExpansionResult::Expanded { raw_sdl, .. } = expand_connectors(
        &original_supergraph.schema.schema().serialize().to_string(),
        &ApiSchemaOptions::default(),
    )?
    else {
        bail!("supplied supergraph has no connectors to expand",);
    };

    // Validate the schema
    // TODO: If expansion errors here due to bugs, it can be very hard to trace
    // what specific portion of the expansion process failed. Work will need to be
    // done to expansion to allow for returning an error type that carries the error
    // and the expanded subgraph as seen until the error.
    let expanded = Supergraph::new_with_router_specs(&raw_sdl)?;

    let subgraphs = expanded.extract_subgraphs()?;
    if let Some(dest) = dest {
        fs::create_dir_all(dest).map_err(|_| SingleFederationError::Internal {
            message: "Error: directory creation failed".into(),
        })?;
        for (name, subgraph) in subgraphs {
            // Skip any files not matching the prefix, if specified
            if let Some(prefix) = filter_prefix
                && !name.starts_with(prefix)
            {
                continue;
            }

            let subgraph_path = dest.join(format!("{}.graphql", name));
            fs::write(subgraph_path, subgraph.schema.schema().to_string()).map_err(|_| {
                SingleFederationError::Internal {
                    message: "Error: file output failed".into(),
                }
            })?;
        }
    } else {
        // Print out the schemas as YAML so that it can be piped into rover
        // TODO: It would be nice to use rover's supergraph type here instead of manually printing
        println!("federation_version: 2");
        println!("subgraphs:");
        for (name, subgraph) in subgraphs {
            // Skip any files not matching the prefix, if specified
            if let Some(prefix) = filter_prefix
                && !name.starts_with(prefix)
            {
                continue;
            }

            let schema_str = subgraph.schema.schema().serialize().initial_indent_level(4);
            println!("  {name}:");
            println!("    routing_url: none");
            println!("    schema:");
            println!("      sdl: |");
            println!("{schema_str}");
            println!(); // newline
        }
    }

    Ok(())
}

fn _cmd_bench(
    file_path: &Path,
    operations_dir: &PathBuf,
    config: QueryPlannerConfig,
) -> Result<Vec<BenchOutput>, FederationError> {
    let supergraph = load_supergraph_file(file_path)?;
    run_bench(supergraph, operations_dir, config)
}

fn cmd_bench(
    file_path: &Path,
    operations_dir: &PathBuf,
    planner: QueryPlannerArgs,
) -> Result<(), AnyError> {
    let results = _cmd_bench(file_path, operations_dir, planner.into())?;
    println!("| operation_name | time (ms) | evaluated_plans (max 10000) | error |");
    println!("|----------------|----------------|-----------|-----------------------------|");
    for r in results {
        println!("{}", r);
    }
    Ok(())
}

#[test]
fn test_bench() {
    insta::assert_json_snapshot!(
        _cmd_bench(
            Path::new("./fixtures/starstuff.graphql"),
            &PathBuf::from("./fixtures/queries"),
            Default::default(),
        ).unwrap(),
        { "[].timing" => 1.234 },
    );
}
