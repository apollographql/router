use std::io::Read;
use std::sync::Mutex;
use std::sync::OnceLock;

use apollo_compiler::collections::IndexSet;
use apollo_federation::query_plan::query_planner::QueryPlanner;
use apollo_federation::query_plan::query_planner::QueryPlannerConfig;
use apollo_federation::query_plan::FetchNode;
use apollo_federation::query_plan::PlanNode;
use apollo_federation::query_plan::QueryPlan;
use apollo_federation::query_plan::TopLevelPlanNode;
use apollo_federation::schema::ValidFederationSchema;
use sha1::Digest;

const ROVER_FEDERATION_VERSION: &str = "2.9.0";

const DEFAULT_LINK_DIRECTIVE: &str = r#"@link(url: "https://specs.apollo.dev/federation/v2.9", import: ["@key", "@requires", "@provides", "@external", "@tag", "@extends", "@shareable", "@inaccessible", "@override", "@composeDirective", "@interfaceObject", "@context", "@fromContext", "@cost", "@listSize"])"#;

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
        config = $config: expr,
        $( $subgraph_name: tt: $subgraph_schema: expr),+
        $(,)?
    ) => {{
        $crate::query_plan::build_query_plan_support::api_schema_and_planner(
            insta::_function_name!(),
            $config,
            &[ $( (subgraph_name!($subgraph_name), $subgraph_schema) ),+ ],
        )
    }};
    (
        $( $subgraph_name: tt: $subgraph_schema: expr),+
        $(,)?
    ) => {
        planner!(config = Default::default(), $( $subgraph_name: $subgraph_schema),+)
    };
}

macro_rules! subgraph_name {
    ($x: ident) => {
        stringify!($x)
    };
    ($x: literal) => {
        $x
    };
}

/// Takes a reference to the result of `planner!()`, an operation string, and an expected
/// formatted query plan string.
/// Run `cargo insta review` to diff and accept changes to the generated query plan.
macro_rules! assert_plan {
    ($api_schema_and_planner: expr, $operation: expr, $options: expr, @$expected: literal) => {{
        let (api_schema, planner) = $api_schema_and_planner;
        let document = apollo_compiler::ExecutableDocument::parse_and_validate(
            api_schema.schema(),
            $operation,
            "operation.graphql",
        )
        .unwrap();
        let plan = planner.build_query_plan(&document, None, $options).unwrap();
        insta::assert_snapshot!(plan, @$expected);
        plan
    }};
    ($api_schema_and_planner: expr, $operation: expr, @$expected: literal) => {{
        let (api_schema, planner) = $api_schema_and_planner;
        let document = apollo_compiler::ExecutableDocument::parse_and_validate(
            api_schema.schema(),
            $operation,
            "operation.graphql",
        )
        .unwrap();
        let plan = planner.build_query_plan(&document, None, Default::default()).unwrap();
        insta::assert_snapshot!(plan, @$expected);
        plan
    }};
}

#[track_caller]
pub(crate) fn api_schema_and_planner(
    function_path: &'static str,
    config: QueryPlannerConfig,
    subgraph_names_and_schemas: &[(&str, &str)],
) -> (ValidFederationSchema, QueryPlanner) {
    let supergraph = compose(function_path, subgraph_names_and_schemas);
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
pub(crate) fn compose(
    function_path: &'static str,
    subgraph_names_and_schemas: &[(&str, &str)],
) -> String {
    let unique_names: IndexSet<_> = subgraph_names_and_schemas
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
                format!("extend schema {DEFAULT_LINK_DIRECTIVE}\n\n{}", schema,),
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

    let test_name = function_path.rsplit("::").next().unwrap();
    static SEEN_TEST_NAMES: OnceLock<Mutex<IndexSet<&'static str>>> = OnceLock::new();
    let new = SEEN_TEST_NAMES
        .get_or_init(Default::default)
        .lock()
        .unwrap()
        .insert(test_name);
    assert!(
        new,
        "planner!() can only be used once in test(s) named '{test_name}'"
    );
    let supergraph_path = std::path::PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap())
        .join("tests")
        .join("query_plan")
        .join("supergraphs")
        .join(format!("{test_name}.graphql",));
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

pub(crate) fn find_fetch_nodes_for_subgraph<'plan>(
    subgraph_name: &str,
    plan: &'plan QueryPlan,
) -> Vec<&'plan FetchNode> {
    let mut fetch_nodes = Vec::new();
    if let Some(node) = &plan.node {
        match node {
            TopLevelPlanNode::Fetch(inner) => {
                if inner.subgraph_name.as_ref() == subgraph_name {
                    fetch_nodes.push(&**inner)
                }
            }
            TopLevelPlanNode::Subscription(inner) => {
                if inner.primary.subgraph_name.as_ref() == subgraph_name {
                    fetch_nodes.push(&inner.primary);
                }
                visit_node(subgraph_name, &mut fetch_nodes, inner.rest.as_deref())
            }
            TopLevelPlanNode::Sequence(inner) => {
                for item in &inner.nodes {
                    visit_node(subgraph_name, &mut fetch_nodes, Some(item))
                }
            }
            TopLevelPlanNode::Parallel(inner) => {
                for item in &inner.nodes {
                    visit_node(subgraph_name, &mut fetch_nodes, Some(item))
                }
            }
            TopLevelPlanNode::Flatten(inner) => {
                visit_node(subgraph_name, &mut fetch_nodes, Some(&inner.node))
            }
            TopLevelPlanNode::Defer(inner) => {
                visit_node(
                    subgraph_name,
                    &mut fetch_nodes,
                    inner.primary.node.as_deref(),
                );
                for deferred in &inner.deferred {
                    visit_node(subgraph_name, &mut fetch_nodes, deferred.node.as_deref());
                }
            }
            TopLevelPlanNode::Condition(inner) => {
                visit_node(subgraph_name, &mut fetch_nodes, inner.if_clause.as_deref());
                visit_node(
                    subgraph_name,
                    &mut fetch_nodes,
                    inner.else_clause.as_deref(),
                );
            }
        }
        fn visit_node<'plan>(
            subgraph_name: &str,
            fetch_nodes: &mut Vec<&'plan FetchNode>,
            node: Option<&'plan PlanNode>,
        ) {
            let Some(node) = node else { return };
            match node {
                PlanNode::Fetch(inner) => {
                    if inner.subgraph_name.as_ref() == subgraph_name {
                        fetch_nodes.push(&**inner)
                    }
                }
                PlanNode::Sequence(inner) => {
                    for item in &inner.nodes {
                        visit_node(subgraph_name, fetch_nodes, Some(item))
                    }
                }
                PlanNode::Parallel(inner) => {
                    for item in &inner.nodes {
                        visit_node(subgraph_name, fetch_nodes, Some(item))
                    }
                }
                PlanNode::Flatten(inner) => {
                    visit_node(subgraph_name, fetch_nodes, Some(&inner.node))
                }
                PlanNode::Defer(inner) => {
                    visit_node(subgraph_name, fetch_nodes, inner.primary.node.as_deref());
                    for deferred in &inner.deferred {
                        visit_node(subgraph_name, fetch_nodes, deferred.node.as_deref());
                    }
                }
                PlanNode::Condition(inner) => {
                    visit_node(subgraph_name, fetch_nodes, inner.if_clause.as_deref());
                    visit_node(subgraph_name, fetch_nodes, inner.else_clause.as_deref());
                }
            }
        }
    }
    fetch_nodes
}
