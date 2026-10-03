//! Aliases the planner generates for fields that share a response name but cannot be merged in a
//! subgraph fetch (and the key rewrites that restore the client's response names).
use apollo_compiler::ExecutableDocument;
use apollo_federation::query_plan::FetchDataRewrite;
use apollo_federation::query_plan::query_planner::QueryPlanner;

use crate::composition::test_helpers::ServiceDefinition;
use crate::composition::test_helpers::compose_as_fed2_subgraphs;
use crate::query_plan::build_query_plan_support::find_fetch_nodes_for_subgraph;

const SCHEMA: &str = r#"
  type Query { node: U }
  union U = A | B
  type A { a: String }
  type B { b: String }
"#;

fn planner() -> QueryPlanner {
    let composed = compose_as_fed2_subgraphs(&[ServiceDefinition {
        name: "S",
        type_defs: SCHEMA,
    }])
    .unwrap();
    let supergraph = apollo_federation::Supergraph::new_with_router_specs(
        &composed.schema().schema().to_string(),
    )
    .unwrap();
    QueryPlanner::new(&supergraph, Default::default()).unwrap()
}

/// Plans `operation`, checks the plan is correct for the client operation, and renders the
/// single fetch's subgraph operation followed by its output rewrites.
fn plan_fetch(operation: &str) -> String {
    let planner = planner();
    let doc = ExecutableDocument::parse_and_validate(
        planner.api_schema().schema(),
        operation,
        "q.graphql",
    )
    .unwrap();
    let plan = planner
        .build_query_plan(&doc, None, Default::default())
        .unwrap();
    apollo_federation::correctness::check_plan(
        planner.api_schema(),
        planner.supergraph_schema(),
        planner.subgraph_schemas(),
        &doc,
        &plan,
    )
    .expect("generated correct plan");
    let fetches = find_fetch_nodes_for_subgraph("S", &plan);
    assert_eq!(fetches.len(), 1, "{plan}");
    let mut rendered = fetches[0].operation_document.as_serialized().to_string();
    for rewrite in &fetches[0].output_rewrites {
        let FetchDataRewrite::KeyRenamer(renamer) = rewrite.as_ref() else {
            panic!("unexpected rewrite: {rewrite:?}");
        };
        let path: Vec<_> = renamer.path.iter().map(|elem| elem.to_string()).collect();
        rendered.push_str(&format!(
            "\n{} -> {}",
            path.join("/"),
            renamer.rename_key_to
        ));
    }
    rendered
}

#[test]
fn planner_preserves_later_client_alias() {
    // `x: b` must be aliased in the fetch because it conflicts with `x: a`; the generated alias
    // must not reuse `x__alias_0`, which the client requests as its own response key.
    insta::assert_snapshot!(
        plan_fetch("{ node { ... on A { x: a } ... on B { x: b x__alias_0: b } } }"),
        @r###"
    { node { __typename ... on A { x: a } ... on B { x__alias_1: b x__alias_0: b } } }
    node/... on B/x__alias_1 -> x
    "###
    );
}

#[test]
fn planner_preserves_earlier_client_alias() {
    insta::assert_snapshot!(
        plan_fetch("{ node { ... on A { x: a } ... on B { x__alias_0: b x: b } } }"),
        @r###"
    { node { __typename ... on A { x: a } ... on B { x__alias_0: b x__alias_1: b } } }
    node/... on B/x__alias_1 -> x
    "###
    );
}
