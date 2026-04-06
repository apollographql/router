//! Integration tests for @interfaceObject with multiple root-field aliases.
//!
//! Verifies that when the same root field is queried twice under different aliases,
//! and @interfaceObject subgraphs contribute extra fields via entity fetches,
//! nullable fields are not nullified in either alias result.
//!
//! Please ensure that any tests added to this file use the tokio multi-threaded
//! test executor.

use apollo_router::MockedSubgraphs;
use apollo_router::TestHarness;
use apollo_router::graphql::Request;
use apollo_router::graphql::Response;
use apollo_router::plugin::test::MockSubgraph;
use apollo_router::services::supergraph;
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

fn setup(mocks: &[(&'static str, &'static str)]) -> TestHarness<'static> {
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

    TestHarness::builder()
        .try_log_level("info")
        .configuration_json(json! {{
            "include_subgraph_errors": { "all": true },
            "supergraph": { "generate_query_fragments": false },
        }})
        .unwrap()
        .schema(include_str!(
            "fixtures/interface_object_multi_alias/supergraph.graphql"
        ))
        .extra_plugin(mocked_subgraphs)
}

async fn run(query: &str, mocks: &[(&'static str, &'static str)]) -> Response {
    let supergraph_service = setup(mocks).build_supergraph().await.unwrap();
    let request = supergraph::Request::fake_builder()
        .query(query.to_string())
        .build()
        .expect("valid request");
    supergraph_service
        .oneshot(request)
        .await
        .unwrap()
        .next_response()
        .await
        .unwrap()
}

// Two aliases to the same root field, both requesting fields from the owner
// subgraph (name) and from @interfaceObject subgraphs (score from B, rank from C).
// All nullable fields must be non-null in both alias results.
#[tokio::test(flavor = "multi_thread")]
async fn nullable_fields_not_null_with_two_aliases_and_two_interface_object_subgraphs() {
    static QUERY: &str = r#"
        query TwoAliases {
            p1: products {
                __typename
                id
                name
                score
                rank
            }
            p2: products {
                __typename
                id
                name
                score
                rank
            }
        }
    "#;

    let response = run(
        QUERY,
        &[
            ("OWNER", include_str!("fixtures/interface_object_multi_alias/owner.json")),
            ("B", include_str!("fixtures/interface_object_multi_alias/subgraph_b.json")),
            ("C", include_str!("fixtures/interface_object_multi_alias/subgraph_c.json")),
        ],
    )
    .await;

    // No errors
    assert!(
        response.errors.is_empty(),
        "expected no errors, got: {:?}",
        response.errors
    );

    let data = response.data.as_ref().expect("expected data");

    // Both aliases must have non-null nullable fields.
    // A bug in alias path tracking during entity-fetch merging would cause
    // name/score/rank to be null in one or both aliases.
    for alias in &["p1", "p2"] {
        let items = data[alias].as_array().expect("expected array");
        assert!(!items.is_empty(), "{alias} should be non-empty");
        for item in items {
            assert!(
                !item["name"].is_null(),
                "{alias}[].name must not be null, got: {item}"
            );
            assert!(
                !item["score"].is_null(),
                "{alias}[].score must not be null, got: {item}"
            );
            assert!(
                !item["rank"].is_null(),
                "{alias}[].rank must not be null, got: {item}"
            );
        }
    }
}
