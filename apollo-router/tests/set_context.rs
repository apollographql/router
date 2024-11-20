//!
//! Please ensure that any tests added to this file use the tokio multi-threaded test executor.
//!

use apollo_router::graphql::Request;
use apollo_router::graphql::Response;
use apollo_router::plugin::test::MockSubgraph;
use apollo_router::services::supergraph;
use apollo_router::MockedSubgraphs;
use apollo_router::TestHarness;
use serde::Deserialize;
use serde_json::json;
use tower::ServiceExt;

#[derive(Deserialize)]
struct SubgraphMock {
    mocks: Vec<RequestAndResponse>,
}

#[derive(Deserialize)]
struct RequestAndResponse {
    request: Request,
    response: Response,
}

macro_rules! snap
{
    ($result:ident) => {
        insta::with_settings!({sort_maps => true}, {
            insta::assert_json_snapshot!($result);
        });
    }
}

fn get_configuration(rust_qp: bool) -> serde_json::Value {
    if rust_qp {
        return json! {{
            "experimental_query_planner_mode": "new",
            "experimental_type_conditioned_fetching": true,
            // will make debugging easier
            "plugins": {
                "experimental.expose_query_plan": true
            },
            "include_subgraph_errors": {
                "all": true
            },
            "supergraph": {
                // TODO(@goto-bus-stop): need to update the mocks and remove this, #6013
                "generate_query_fragments": false,
            }
        }};
    }
    return json! {{
        "experimental_type_conditioned_fetching": true,
        // will make debugging easier
        "plugins": {
            "experimental.expose_query_plan": true
        },
        "include_subgraph_errors": {
            "all": true
        },
        "supergraph": {
            // TODO(@goto-bus-stop): need to update the mocks and remove this, #6013
            "generate_query_fragments": false,
        }
    }}
}


async fn run_single_request(query: &str, rust_qp: bool, mocks: &[(&'static str, &'static str)]) -> Response {
    let configuration = get_configuration(rust_qp);
    let harness = setup_from_mocks(configuration, mocks);
    let supergraph_service = harness.build_supergraph().await.unwrap();
    let request = supergraph::Request::fake_builder()
        .query(query.to_string())
        .header("Apollo-Expose-Query-Plan", "true")
        .variables(Default::default())
        .build()
        .expect("expecting valid request");

    supergraph_service
        .oneshot(request)
        .await
        .unwrap()
        .next_response()
        .await
        .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn test_set_context() {
    static QUERY: &str = r#"
        query Query {
            t {
                __typename
                id
                u {
                    __typename
                    field
                }
            }
        }"#;

    let response = run_single_request(
        QUERY,
        false,
        &[
            ("Subgraph1", include_str!("fixtures/set_context/one.json")),
            ("Subgraph2", include_str!("fixtures/set_context/two.json")),
        ],
    )
    .await;

    snap!(response);
}

#[tokio::test(flavor = "multi_thread")]
async fn test_set_context_rust_qp() {
    static QUERY: &str = r#"
        query set_context_rust_qp {
            t {
                __typename
                id
                u {
                    __typename
                    field
                }
            }
        }"#;

    let response = run_single_request(
        QUERY,
        true,
        &[
            ("Subgraph1", include_str!("fixtures/set_context/one_rust_qp.json")),
            ("Subgraph2", include_str!("fixtures/set_context/two.json")),
        ],
    )
    .await;

    snap!(response);
}

#[tokio::test(flavor = "multi_thread")]
async fn test_set_context_no_typenames() {
    static QUERY_NO_TYPENAMES: &str = r#"
        query Query {
            t {
                id
                u {
                    field
                }
            }
        }"#;

    let response = run_single_request(
        QUERY_NO_TYPENAMES,
        false,
        &[
            ("Subgraph1", include_str!("fixtures/set_context/one.json")),
            ("Subgraph2", include_str!("fixtures/set_context/two.json")),
        ],
    )
    .await;

    snap!(response);
}

#[tokio::test(flavor = "multi_thread")]
async fn test_set_context_no_typenames_rust_qp() {
    static QUERY_NO_TYPENAMES: &str = r#"
        query set_context_no_typenames_rust_qp {
            t {
                id
                u {
                    field
                }
            }
        }"#;

    let response = run_single_request(
        QUERY_NO_TYPENAMES,
        true,
        &[
            ("Subgraph1", include_str!("fixtures/set_context/one_rust_qp.json")),
            ("Subgraph2", include_str!("fixtures/set_context/two.json")),
        ],
    )
    .await;

    snap!(response);
}

#[tokio::test(flavor = "multi_thread")]
async fn test_set_context_list() {
    static QUERY_WITH_LIST: &str = r#"
        query Query {
            t {
                id
                uList {
                    field
                }
            }
        }"#;

    let response = run_single_request(
        QUERY_WITH_LIST,
        false,
        &[
            ("Subgraph1", include_str!("fixtures/set_context/one.json")),
            ("Subgraph2", include_str!("fixtures/set_context/two.json")),
        ],
    )
    .await;

    snap!(response);
}

#[tokio::test(flavor = "multi_thread")]
async fn test_set_context_list_rust_qp() {
    static QUERY_WITH_LIST: &str = r#"
        query set_context_list_rust_qp {
            t {
                id
                uList {
                    field
                }
            }
        }"#;

    let response = run_single_request(
        QUERY_WITH_LIST,
        true,
        &[
            ("Subgraph1", include_str!("fixtures/set_context/one_rust_qp.json")),
            ("Subgraph2", include_str!("fixtures/set_context/two.json")),
        ],
    )
    .await;

    snap!(response);
}

#[tokio::test(flavor = "multi_thread")]
async fn test_set_context_list_of_lists() {
    static QUERY_WITH_LIST_OF_LISTS: &str = r#"
        query QueryLL {
            tList {
                id
                uList {
                    field
                }
            }
        }"#;

    let response = run_single_request(
        QUERY_WITH_LIST_OF_LISTS,
        false,
        &[
            ("Subgraph1", include_str!("fixtures/set_context/one.json")),
            ("Subgraph2", include_str!("fixtures/set_context/two.json")),
        ],
    )
    .await;

    snap!(response);
}

#[tokio::test(flavor = "multi_thread")]
async fn test_set_context_list_of_lists_rust_qp() {
    static QUERY_WITH_LIST_OF_LISTS: &str = r#"
        query QueryLL {
            tList {
                id
                uList {
                    field
                }
            }
        }"#;

    let response = run_single_request(
        QUERY_WITH_LIST_OF_LISTS,
        true,
        &[
            ("Subgraph1", include_str!("fixtures/set_context/one_rust_qp.json")),
            ("Subgraph2", include_str!("fixtures/set_context/two.json")),
        ],
    )
    .await;

    snap!(response);
}

#[tokio::test(flavor = "multi_thread")]
async fn test_set_context_union() {
    static QUERY_WITH_UNION: &str = r#"
        query QueryUnion {
            k {
                ... on A {
                    v {
                        field
                    }
                }
                ... on B {
                    v {
                        field
                    }
                }
            }
        }"#;

    let response = run_single_request(
        QUERY_WITH_UNION,
        false,
        &[
            ("Subgraph1", include_str!("fixtures/set_context/one.json")),
            ("Subgraph2", include_str!("fixtures/set_context/two.json")),
        ],
    )
    .await;

    snap!(response);
}

#[tokio::test(flavor = "multi_thread")]
async fn test_set_context_union_rust_qp() {
    static QUERY_WITH_UNION: &str = r#"
        query QueryUnion {
            k {
                ... on A {
                    v {
                        field
                    }
                }
                ... on B {
                    v {
                        field
                    }
                }
            }
        }"#;

    let response = run_single_request(
        QUERY_WITH_UNION,
        true,
        &[
            ("Subgraph1", include_str!("fixtures/set_context/one_rust_qp.json")),
            ("Subgraph2", include_str!("fixtures/set_context/two.json")),
        ],
    )
    .await;

    snap!(response);
}

#[tokio::test(flavor = "multi_thread")]
async fn test_set_context_with_null() {
    static QUERY: &str = r#"
        query Query_Null_Param {
            t {
                id
                u {
                    field
                }
            }
        }"#;

    let response = run_single_request(
        QUERY,
        false,
        &[
            (
                "Subgraph1",
                include_str!("fixtures/set_context/one_null_param.json"),
            ),
            ("Subgraph2", include_str!("fixtures/set_context/two.json")),
        ],
    )
    .await;

    insta::assert_json_snapshot!(response);
}

#[tokio::test(flavor = "multi_thread")]
async fn test_set_context_with_null_rust_qp() {
    static QUERY: &str = r#"
        query Query_Null_Param {
            t {
                id
                u {
                    field
                }
            }
        }"#;

    let response = run_single_request(
        QUERY,
        true,
        &[
            (
                "Subgraph1",
                include_str!("fixtures/set_context/one_null_param.json"),
            ),
            ("Subgraph2", include_str!("fixtures/set_context/two.json")),
        ],
    )
    .await;

    insta::assert_json_snapshot!(response);
}

// this test returns the contextual value with a different than expected type
// this currently works, but perhaps should do type valdiation in the future to reject
#[tokio::test(flavor = "multi_thread")]
async fn test_set_context_type_mismatch() {
    static QUERY: &str = r#"
        query Query_type_mismatch {
            t {
                id
                u {
                    field
                }
            }
        }"#;

    let response = run_single_request(
        QUERY,
        false,
        &[
            ("Subgraph1", include_str!("fixtures/set_context/one.json")),
            ("Subgraph2", include_str!("fixtures/set_context/two.json")),
        ],
    )
    .await;

    snap!(response);
}

// this test returns the contextual value with a different than expected type
// this currently works, but perhaps should do type valdiation in the future to reject
#[tokio::test(flavor = "multi_thread")]
async fn test_set_context_type_mismatch_rust_qp() {
    static QUERY: &str = r#"
        query Query_type_mismatch {
            t {
                id
                u {
                    field
                }
            }
        }"#;

    let response = run_single_request(
        QUERY,
        true,
        &[
            ("Subgraph1", include_str!("fixtures/set_context/one_rust_qp.json")),
            ("Subgraph2", include_str!("fixtures/set_context/two.json")),
        ],
    )
    .await;

    snap!(response);
}

// fetch from unrelated (to context) subgraph fails
// validates that the error propagation is correct
#[tokio::test(flavor = "multi_thread")]
async fn test_set_context_unrelated_fetch_failure() {
    static QUERY: &str = r#"
        query Query_fetch_failure {
            t {
                id
                u {
                    field
                    b
                }
            }
        }"#;

    let response = run_single_request(
        QUERY,
        false,
        &[
            (
                "Subgraph1",
                include_str!("fixtures/set_context/one_fetch_failure.json"),
            ),
            ("Subgraph2", include_str!("fixtures/set_context/two.json")),
        ],
    )
    .await;

    snap!(response);
}

// fetch from unrelated (to context) subgraph fails
// validates that the error propagation is correct
#[tokio::test(flavor = "multi_thread")]
async fn test_set_context_unrelated_fetch_failure_rust_qp() {
    static QUERY: &str = r#"
        query Query_fetch_failure {
            t {
                id
                u {
                    field
                    b
                }
            }
        }"#;

    let response = run_single_request(
        QUERY,
        true,
        &[
            (
                "Subgraph1",
                include_str!("fixtures/set_context/one_fetch_failure.json"),
            ),
            ("Subgraph2", include_str!("fixtures/set_context/two.json")),
        ],
    )
    .await;

    snap!(response);
}

// subgraph fetch fails where context depends on results of fetch.
// validates that no fetch will get called that passes context
#[tokio::test(flavor = "multi_thread")]
async fn test_set_context_dependent_fetch_failure() {
    static QUERY: &str = r#"
        query Query_fetch_dependent_failure {
            t {
                id
                u {
                    field
                }
            }
        }"#;

    let response = run_single_request(
        QUERY,
        false,
        &[
            (
                "Subgraph1",
                include_str!("fixtures/set_context/one_dependent_fetch_failure.json"),
            ),
            ("Subgraph2", include_str!("fixtures/set_context/two.json")),
        ],
    )
    .await;

    snap!(response);
}

// subgraph fetch fails where context depends on results of fetch.
// validates that no fetch will get called that passes context
#[tokio::test(flavor = "multi_thread")]
async fn test_set_context_dependent_fetch_failure_rust_qp() {
    static QUERY: &str = r#"
        query Query_fetch_dependent_failure {
            t {
                id
                u {
                    field
                }
            }
        }"#;

    let response = run_single_request(
        QUERY,
        true,
        &[
            (
                "Subgraph1",
                include_str!("fixtures/set_context/one_dependent_fetch_failure.json"),
            ),
            ("Subgraph2", include_str!("fixtures/set_context/two.json")),
        ],
    )
    .await;

    snap!(response);
}

fn setup_from_mocks(
    configuration: serde_json::Value,
    mocks: &[(&'static str, &'static str)],
) -> TestHarness<'static> {
    let mut mocked_subgraphs = MockedSubgraphs::default();

    for (name, m) in mocks {
        let subgraph_mock: SubgraphMock = serde_json::from_str(m).unwrap();

        let mut builder = MockSubgraph::builder();

        for mock in subgraph_mock.mocks {
            builder = builder.with_json(
                serde_json::to_value(mock.request).unwrap(),
                serde_json::to_value(mock.response).unwrap(),
            );
        }

        mocked_subgraphs.insert(name, builder.build());
    }

    let schema = include_str!("fixtures/set_context/supergraph.graphql");
    TestHarness::builder()
        .try_log_level("info")
        .configuration_json(configuration)
        .unwrap()
        .schema(schema)
        .extra_plugin(mocked_subgraphs)
}
