use std::io::Read;

use apollo_federation::query_plan::query_planner::QueryPlanner;
use apollo_federation::query_plan::query_planner::QueryPlannerConfig;
use apollo_federation::schema::ValidFederationSchema;
use sha1::Digest;

const ROVER_FEDERATION_VERSION: &str = "2.7.4";

// TODO: use 2.7 when join v0.4 is fully supported in this crate
const IMPLICIT_LINK_DIRECTIVE: &str = r#"@link(url: "https://specs.apollo.dev/federation/v2.6", import: ["@key", "@requires", "@provides", "@external", "@tag", "@extends", "@shareable", "@inaccessible", "@override", "@composeDirective", "@interfaceObject"])"#;

/// Runs composition on the given subgraph schemas and return `(api_schema, query_planner)`
///
/// Results of composition are cached in `tests/query_plan/supergraphs`.
/// When needed, composition is done by starting a Rover subprocess
/// (this requires a recent-enough version of `rover` to be in `$PATH`)
/// but only if the `USE_ROVER=1` env variable is set.
///
/// Panics if composition is needed but `USE_ROVER` is unset.
///
/// This can all be remove when composition is implemented in Rust.
macro_rules! planner {
    (
        $( config = $config: expr, )?
        $( $subgraph_name: ident: $subgraph_schema: expr),+
        $(,)?
    ) => {{
        #[allow(unused_mut)]
        let mut config = Default::default();
        $( config = $config )?
        $crate::query_plan::build_query_plan_support::api_schema_and_planner(
            insta::_function_name!(),
            config,
            &[ $( (stringify!($subgraph_name), $subgraph_schema) ),+ ],
        )
    }};
}

/// Takes a reference to the result of `planner!()`, an operation string, and an expected
/// formatted query plan string.
/// Run `cargo insta review` to diff and accept changes to the generated query plan.
macro_rules! assert_plan {
    ($api_schema_and_planner: expr, $operation: expr, @$expected: literal) => {{
        let (api_schema, planner) = $api_schema_and_planner;
        let document = apollo_compiler::ExecutableDocument::parse_and_validate(
            api_schema.schema(),
            $operation,
            "operation.graphql",
        )
        .unwrap();
        insta::assert_snapshot!(planner.build_query_plan(&document, None).unwrap(), @$expected);
    }};
}

#[track_caller]
pub(crate) fn api_schema_and_planner(
    test_name: &str,
    config: QueryPlannerConfig,
    subgraph_names_and_schemas: &[(&str, &str)],
) -> (ValidFederationSchema, QueryPlanner) {
    let supergraph = compose(test_name, subgraph_names_and_schemas);
    let supergraph = apollo_federation::Supergraph::new(&supergraph).unwrap();
    let planner = QueryPlanner::new(&supergraph, config).unwrap();
    let api_schema_config = apollo_federation::ApiSchemaOptions {
        include_defer: true,
        include_stream: false,
    };
    let api_schema = supergraph.to_api_schema(api_schema_config).unwrap();
    (api_schema, planner)
}

#[track_caller]
pub(crate) fn compose(test_name: &str, subgraph_names_and_schemas: &[(&str, &str)]) -> String {
    let unique_names: std::collections::HashSet<_> = subgraph_names_and_schemas
        .iter()
        .map(|(name, _)| name)
        .collect();
    assert!(
        unique_names.len() == subgraph_names_and_schemas.len(),
        "subgraph names must be unique"
    );

    let subgraph_names_and_schemas: Vec<_> = subgraph_names_and_schemas
        .iter()
        .map(|(name, schema)| {
            (
                *name,
                format!("extend schema {IMPLICIT_LINK_DIRECTIVE}\n\n{}", schema,),
            )
        })
        .collect();

    let mut hasher = sha1::Sha1::new();
    hasher.update(ROVER_FEDERATION_VERSION);
    for (name, schema) in &subgraph_names_and_schemas {
        hasher.update(b"\xFF");
        hasher.update(name);
        hasher.update(b"\xFF");
        hasher.update(schema);
    }
    let expected_hash = hex::encode(hasher.finalize());
    let prefix = "# Composed from subgraphs with hash: ";

    let supergraph_path = std::path::PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap())
        .join("tests")
        .join("query_plan")
        .join("supergraphs")
        .join(format!("{}.graphql", test_name.replace("::", "__")));
    let supergraph = match std::fs::read_to_string(&supergraph_path) {
        Ok(contents) => {
            if let Some(hash) = contents
                .lines()
                .next()
                .unwrap_or_default()
                .strip_prefix(prefix)
            {
                if hash == expected_hash {
                    Ok(contents)
                } else {
                    Err("outdated")
                }
            } else {
                Err("malformed")
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err("missing"),
        Err(e) => panic!("{e}"),
    };
    supergraph.unwrap_or_else(|reason| {
        if std::env::var_os("USE_ROVER").is_none() {
            panic!(
                "Composed supergraph schema file {} is {reason}. \
                 Make sure `rover` is in $PATH and re-run with `USE_ROVER=1` \
                 env variable to update it.",
                supergraph_path.display()
            )
        }
        let temp_dir = tempfile::tempdir().unwrap();
        let temp_dir = temp_dir.path();
        let mut config = format!("federation_version: ={ROVER_FEDERATION_VERSION}\nsubgraphs:\n");
        for (name, schema) in subgraph_names_and_schemas {
            let subgraph_path = temp_dir.join(format!("{name}.graphql"));
            config.push_str(&format!(
                "  {name}:\n    routing_url: none\n    schema:\n      file: {}\n",
                subgraph_path.display()
            ));
            std::fs::write(subgraph_path, schema).unwrap();
        }
        let config_path = temp_dir.join("rover.yaml");
        std::fs::write(&config_path, config).unwrap();
        let mut rover = std::process::Command::new("rover")
            .args(["supergraph", "compose", "--config"])
            .arg(config_path)
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let mut supergraph = format!("{prefix}{expected_hash}\n");
        rover
            .stdout
            .take()
            .unwrap()
            .read_to_string(&mut supergraph)
            .unwrap();
        assert!(rover.wait().unwrap().success());
        std::fs::write(supergraph_path, &supergraph).unwrap();
        supergraph
    })
}
