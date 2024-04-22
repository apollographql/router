//!
//! Please ensure that any tests added to this file use the tokio multi-threaded test executor.
//!

use apollo_compiler::execution::JsonMap;
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

#[tokio::test(flavor = "multi_thread")]
async fn test_type_conditions_enabled() {
    let harness = setup_from_mocks(
        json! {{
            "experimental_type_conditioned_fetching": true,
            // will make debugging easier
            "plugins": {
                "experimental.expose_query_plan": true
            },
            "include_subgraph_errors": {
                "all": true
            }
        }},
        &[
            (
                "searchSubgraph",
                include_str!("fixtures/type_conditions/search.json"),
            ),
            (
                "artworkSubgraph",
                include_str!("fixtures/type_conditions/artwork.json"),
            ),
        ],
    );
    let supergraph_service = harness.build_supergraph().await.unwrap();
    let mut variables = JsonMap::new();
    variables.insert("movieResultParam", "movieResultEnabled".into());
    variables.insert("articleResultParam", "articleResultEnabled".into());
    let request = supergraph::Request::fake_builder()
        .query(QUERY.to_string())
        .header("Apollo-Expose-Query-Plan", "true")
        .variables(variables)
        .build()
        .expect("expecting valid request");

    let response = supergraph_service
        .oneshot(request)
        .await
        .unwrap()
        .next_response()
        .await
        .unwrap();

    insta::assert_json_snapshot!(response);
}

#[tokio::test(flavor = "multi_thread")]
async fn test_type_conditions_enabled_generate_query_fragments() {
    let harness = setup_from_mocks(
        json! {{
            "experimental_type_conditioned_fetching": true,
            "supergraph": {
                "generate_query_fragments": true
            },
            // will make debugging easier
            "plugins": {
                "experimental.expose_query_plan": true
            },
            "include_subgraph_errors": {
                "all": true
            }
        }},
        &[
            (
                "searchSubgraph",
                include_str!("fixtures/type_conditions/search_query_fragments_enabled.json"),
            ),
            (
                "artworkSubgraph",
                include_str!("fixtures/type_conditions/artwork_query_fragments_enabled.json"),
            ),
        ],
    );
    let supergraph_service = harness.build_supergraph().await.unwrap();
    let mut variables = JsonMap::new();
    variables.insert("movieResultParam", "movieResultEnabled".into());
    variables.insert("articleResultParam", "articleResultEnabled".into());
    let request = supergraph::Request::fake_builder()
        .query(QUERY.to_string())
        .header("Apollo-Expose-Query-Plan", "true")
        .variables(variables)
        .build()
        .expect("expecting valid request");

    let response = supergraph_service
        .oneshot(request)
        .await
        .unwrap()
        .next_response()
        .await
        .unwrap();

    insta::assert_json_snapshot!(response);
}

#[tokio::test(flavor = "multi_thread")]
async fn test_type_conditions_enabled_list_of_list() {
    let harness = setup_from_mocks(
        json! {{
            "experimental_type_conditioned_fetching": true,
            // will make debugging easier
            "plugins": {
                "experimental.expose_query_plan": true
            },
            "include_subgraph_errors": {
                "all": true
            }
        }},
        &[
            (
                "searchSubgraph",
                include_str!("fixtures/type_conditions/search_list_of_list.json"),
            ),
            (
                "artworkSubgraph",
                include_str!("fixtures/type_conditions/artwork.json"),
            ),
        ],
    );
    let supergraph_service = harness.build_supergraph().await.unwrap();
    let mut variables = JsonMap::new();
    variables.insert("movieResultParam", "movieResultEnabled".into());
    variables.insert("articleResultParam", "articleResultEnabled".into());
    let request = supergraph::Request::fake_builder()
        .query(QUERY_LIST_OF_LIST.to_string())
        .header("Apollo-Expose-Query-Plan", "true")
        .variables(variables)
        .build()
        .expect("expecting valid request");

    let response = supergraph_service
        .oneshot(request)
        .await
        .unwrap()
        .next_response()
        .await
        .unwrap();

    insta::assert_json_snapshot!(response);
}

// one last to make sure unnesting is correct
#[tokio::test(flavor = "multi_thread")]
async fn test_type_conditions_enabled_list_of_list_of_list() {
    let harness = setup_from_mocks(
        json! {{
            "experimental_type_conditioned_fetching": true,
            // will make debugging easier
            "plugins": {
                "experimental.expose_query_plan": true
            },
            "include_subgraph_errors": {
                "all": true
            }
        }},
        &[
            (
                "searchSubgraph",
                include_str!("fixtures/type_conditions/search_list_of_list_of_list.json"),
            ),
            (
                "artworkSubgraph",
                include_str!("fixtures/type_conditions/artwork.json"),
            ),
        ],
    );
    let supergraph_service = harness.build_supergraph().await.unwrap();
    let mut variables = JsonMap::new();
    variables.insert("movieResultParam", "movieResultEnabled".into());
    variables.insert("articleResultParam", "articleResultEnabled".into());
    let request = supergraph::Request::fake_builder()
        .query(QUERY_LIST_OF_LIST_OF_LIST.to_string())
        .header("Apollo-Expose-Query-Plan", "true")
        .variables(variables)
        .build()
        .expect("expecting valid request");

    let response = supergraph_service
        .oneshot(request)
        .await
        .unwrap()
        .next_response()
        .await
        .unwrap();

    insta::assert_json_snapshot!(response);
}

#[tokio::test(flavor = "multi_thread")]
async fn test_type_conditions_disabled() {
    let harness = setup_from_mocks(
        json! {{
            "experimental_type_conditioned_fetching": false,
            // will make debugging easier
            "plugins": {
                "experimental.expose_query_plan": true
            },
            "include_subgraph_errors": {
                "all": true
            }
        }},
        &[
            (
                "searchSubgraph",
                include_str!("fixtures/type_conditions/search.json"),
            ),
            (
                "artworkSubgraph",
                include_str!("fixtures/type_conditions/artwork_disabled.json"),
            ),
        ],
    );
    let supergraph_service = harness.build_supergraph().await.unwrap();
    let mut variables = JsonMap::new();
    variables.insert("movieResultParam", "movieResultDisabled".into());
    variables.insert("articleResultParam", "articleResultDisabled".into());
    let request = supergraph::Request::fake_builder()
        .query(QUERY.to_string())
        .header("Apollo-Expose-Query-Plan", "true")
        .build()
        .expect("expecting valid request");

    let response = supergraph_service
        .oneshot(request)
        .await
        .unwrap()
        .next_response()
        .await
        .unwrap();

    insta::assert_json_snapshot!(response);
}

#[tokio::test(flavor = "multi_thread")]
async fn test_type_conditions_enabled_shouldnt_make_article_fetch() {
    let harness = setup_from_mocks(
        json! {{
            "experimental_type_conditioned_fetching": true,
            // will make debugging easier
            "plugins": {
                "experimental.expose_query_plan": true
            },
            "include_subgraph_errors": {
                "all": true
            }
        }},
        &[
            (
                "searchSubgraph",
                include_str!("fixtures/type_conditions/search_no_articles.json"),
            ),
            (
                "artworkSubgraph",
                include_str!("fixtures/type_conditions/artwork_no_articles.json"),
            ),
        ],
    );
    let supergraph_service = harness.build_supergraph().await.unwrap();
    let mut variables = JsonMap::new();
    variables.insert("movieResultParam", "movieResultEnabled".into());
    variables.insert("articleResultParam", "articleResultEnabled".into());
    let request = supergraph::Request::fake_builder()
        .query(QUERY.to_string())
        .header("Apollo-Expose-Query-Plan", "true")
        .variables(variables)
        .build()
        .expect("expecting valid request");

    let response = supergraph_service
        .oneshot(request)
        .await
        .unwrap()
        .next_response()
        .await
        .unwrap();

    insta::assert_json_snapshot!(response);
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

    let schema = include_str!("fixtures/type_conditions/type_conditions.graphql");
    TestHarness::builder()
        .try_log_level("info")
        .configuration_json(configuration)
        .unwrap()
        .schema(schema)
        .extra_plugin(mocked_subgraphs)
}

static QUERY: &str = r#"
query Search($movieResultParam: String, $articleResultParam: String) {
    search {
      ... on MovieResult {
        sections {
          ... on EntityCollectionSection {
            id
            title
            artwork(params: $movieResultParam)
          }
          ... on GallerySection {
            artwork(params: $movieResultParam)
            id
          }
        }
        id
      }
      ... on ArticleResult {
        id
        sections {
          ... on GallerySection {
            artwork(params: $articleResultParam)
          }
          ... on EntityCollectionSection {
            artwork(params: $articleResultParam)
            title
          }
        }
      }
    }
}"#;

static QUERY_LIST_OF_LIST: &str = r#"
query Search($movieResultParam: String, $articleResultParam: String) {
    searchListOfList {
      ... on MovieResult {
        sections {
          ... on EntityCollectionSection {
            id
            title
            artwork(params: $movieResultParam)
          }
          ... on GallerySection {
            artwork(params: $movieResultParam)
            id
          }
        }
        id
      }
      ... on ArticleResult {
        id
        sections {
          ... on GallerySection {
            artwork(params: $articleResultParam)
          }
          ... on EntityCollectionSection {
            artwork(params: $articleResultParam)
            title
          }
        }
      }
    }
}"#;

static QUERY_LIST_OF_LIST_OF_LIST: &str = r#"
query Search($movieResultParam: String, $articleResultParam: String) {
    searchListOfListOfList {
      ... on MovieResult {
        sections {
          ... on EntityCollectionSection {
            id
            title
            artwork(params: $movieResultParam)
          }
          ... on GallerySection {
            artwork(params: $movieResultParam)
            id
          }
        }
        id
      }
      ... on ArticleResult {
        id
        sections {
          ... on GallerySection {
            artwork(params: $articleResultParam)
          }
          ... on EntityCollectionSection {
            artwork(params: $articleResultParam)
            title
          }
        }
      }
    }
}"#;
