use std::io::Read;
use std::sync::Mutex;
use std::sync::OnceLock;

use apollo_compiler::collections::IndexSet;
use apollo_federation::query_plan::FetchNode;
use apollo_federation::query_plan::PlanNode;
use apollo_federation::query_plan::QueryPlan;
use apollo_federation::query_plan::TopLevelPlanNode;
use apollo_federation::query_plan::query_planner::QueryPlanOptions;
use apollo_federation::query_plan::query_planner::QueryPlanner;
use apollo_federation::query_plan::query_planner::QueryPlannerConfig;
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
        $crate::query_plan::build_query_plan_support::test_planner(
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
    (validate_correctness = $validate_correctness: expr, $api_schema_and_planner: expr, $operation: expr, @$expected: literal) => {{
        assert_plan!($api_schema_and_planner, $operation, Default::default(), $validate_correctness, @$expected)
    }};
    ($planner: expr, $operation: expr, $options: expr, $validate_correctness: expr, @$expected: literal) => {{
        let api_schema = $planner.api_schema();
        let document = apollo_compiler::ExecutableDocument::parse_and_validate(
            api_schema.schema(),
            $operation,
            "operation.graphql",
        )
        .expect("valid graphql document");
        let options: apollo_federation::query_plan::query_planner::QueryPlanOptions = $options;
        let plan = $planner.build_query_plan(&document, None, options.clone()).expect("query plan generated");
        insta::assert_snapshot!(plan, @$expected);
        // temporary workaround for correctness errors such as FED-515
        if $validate_correctness {
            apollo_federation::correctness::check_plan($planner.api_schema(), $planner.supergraph_schema(), $planner.subgraph_schemas(), &document, &plan).expect("generated correct plan");
            use $crate::query_plan::build_query_plan_support::CheckBulbTwin as _;
            $planner.check_bulb_twin(&document, options);
        }
        plan
    }};
    ($api_schema_and_planner: expr, $operation: expr, $options: expr, @$expected: literal) => {{
        assert_plan!($api_schema_and_planner, $operation, $options, true, @$expected)
    }};
    ($api_schema_and_planner: expr, $operation: expr, @$expected: literal) => {{
        assert_plan!($api_schema_and_planner, $operation, Default::default(), true, @$expected)
    }};
}

#[track_caller]
pub(crate) fn test_planner(
    function_path: &'static str,
    config: QueryPlannerConfig,
    subgraph_names_and_schemas: &[(&str, &str)],
) -> QueryPlanner {
    let supergraph = compose(function_path, subgraph_names_and_schemas);
    let supergraph = apollo_federation::Supergraph::new_with_router_specs(&supergraph)
        .expect("valid supergraph");
    QueryPlanner::new(&supergraph, config).expect("can create query planner")
}

/// A legacy planner paired with an incremental (BULB) planner over the same
/// supergraph. `assert_plan!` snapshots the legacy plan and additionally
/// requires the BULB plan to pass `check_plan`, without snapshotting it.
pub(crate) struct BulbTwinPlanner {
    legacy: QueryPlanner,
    bulb: QueryPlanner,
}

impl std::ops::Deref for BulbTwinPlanner {
    type Target = QueryPlanner;

    fn deref(&self) -> &QueryPlanner {
        &self.legacy
    }
}

impl BulbTwinPlanner {
    pub(crate) fn bulb(&self) -> &QueryPlanner {
        &self.bulb
    }
}

#[track_caller]
pub(crate) fn test_planner_with_bulb_twin(
    function_path: &'static str,
    config: QueryPlannerConfig,
    subgraph_names_and_schemas: &[(&str, &str)],
) -> BulbTwinPlanner {
    bulb_twin_for(compose(function_path, subgraph_names_and_schemas), config)
}

/// [`test_planner_with_bulb_twin`] over a supergraph cached under
/// `fixture_name`, which every test composing the same subgraphs can share.
#[track_caller]
pub(crate) fn shared_test_planner_with_bulb_twin(
    fixture_name: &'static str,
    config: QueryPlannerConfig,
    subgraph_names_and_schemas: &[(&str, &str)],
) -> BulbTwinPlanner {
    bulb_twin_for(
        compose_shared(fixture_name, subgraph_names_and_schemas),
        config,
    )
}

#[track_caller]
fn bulb_twin_for(supergraph: String, config: QueryPlannerConfig) -> BulbTwinPlanner {
    let supergraph = apollo_federation::Supergraph::new_with_router_specs(&supergraph)
        .expect("valid supergraph");
    let mut bulb_config = config.clone();
    bulb_config.incremental_planner.enabled = true;
    BulbTwinPlanner {
        legacy: QueryPlanner::new(&supergraph, config).expect("can create query planner"),
        bulb: QueryPlanner::new(&supergraph, bulb_config).expect("can create BULB planner"),
    }
}

pub(crate) trait CheckBulbTwin {
    fn check_bulb_twin(
        &self,
        _document: &apollo_compiler::validation::Valid<apollo_compiler::ExecutableDocument>,
        _options: QueryPlanOptions,
    ) {
    }
}

impl CheckBulbTwin for QueryPlanner {}

impl CheckBulbTwin for BulbTwinPlanner {
    #[track_caller]
    fn check_bulb_twin(
        &self,
        document: &apollo_compiler::validation::Valid<apollo_compiler::ExecutableDocument>,
        options: QueryPlanOptions,
    ) {
        let plan = self
            .bulb
            .build_query_plan(document, None, options)
            .expect("BULB query plan generated");
        if let Err(err) = apollo_federation::correctness::check_plan(
            self.bulb.api_schema(),
            self.bulb.supergraph_schema(),
            self.bulb.subgraph_schemas(),
            document,
            &plan,
        ) {
            panic!("BULB plan failed check_plan: {err}\n{plan}");
        }
    }
}

#[track_caller]
pub(crate) fn compose(
    function_path: &'static str,
    subgraph_names_and_schemas: &[(&str, &str)],
) -> String {
    let test_name = function_path.rsplit("::").next().unwrap();
    compose_fixture(test_name, false, subgraph_names_and_schemas)
}

/// Like [`compose`], but caches the supergraph under `fixture_name` instead
/// of the test's name, so tests composing the same subgraphs share one file.
#[track_caller]
pub(crate) fn compose_shared(
    fixture_name: &'static str,
    subgraph_names_and_schemas: &[(&str, &str)],
) -> String {
    compose_fixture(fixture_name, true, subgraph_names_and_schemas)
}

/// Records that the fixture `name` is in use in this process. A test's own
/// fixture is used once. A shared one may be reused, but only for the same
/// subgraphs, so two tests never write different content to one file.
#[track_caller]
fn claim_fixture_name(name: &'static str, shared_hash: Option<&str>) {
    static SEEN: OnceLock<Mutex<std::collections::HashMap<&'static str, Option<String>>>> =
        OnceLock::new();
    let mut seen = SEEN.get_or_init(Default::default).lock().unwrap();
    match (seen.get(name), shared_hash) {
        (None, _) => {
            seen.insert(name, shared_hash.map(str::to_owned));
        }
        (Some(Some(claimed)), Some(hash)) => assert_eq!(
            claimed, hash,
            "shared fixture '{name}' is composed from different subgraphs"
        ),
        _ => panic!("planner!() can only be used once in test(s) named '{name}'"),
    }
}

#[track_caller]
fn compose_fixture(
    name: &'static str,
    shared: bool,
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
                format!("extend schema {DEFAULT_LINK_DIRECTIVE}\n\n{schema}",),
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

    claim_fixture_name(name, shared.then_some(expected_hash.as_str()));
    let supergraph_path = std::path::PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap())
        .join("tests")
        .join("query_plan")
        .join("supergraphs")
        .join(format!("{name}.graphql",));
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
        // Tests sharing a fixture can regenerate it at the same time, so a
        // reader must never see a partly written file.
        let partial = supergraph_path.with_extension(format!("{}.tmp", std::process::id()));
        std::fs::write(&partial, &supergraph).unwrap();
        std::fs::rename(partial, supergraph_path).unwrap();
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
