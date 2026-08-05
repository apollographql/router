//!
//! Please ensure that any tests added to this file use the tokio multi-threaded test executor.
//!

use apollo_compiler::ast::Document;
use apollo_json::JsonKind;
use apollo_json::Value;
use apollo_router::MockedSubgraphs;
use apollo_router::TestHarness;
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

/// A mocked exchange, held as raw JSON for [`MockSubgraph`] to deserialize into
/// a GraphQL request and response.
#[derive(Deserialize)]
struct RequestAndResponse {
    request: serde_json::Value,
    response: serde_json::Value,
}

#[tokio::test(flavor = "multi_thread")]
async fn test_type_conditions_enabled() {
    _test_type_conditions_enabled().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn test_type_conditions_enabled_generate_query_fragments() {
    _test_type_conditions_enabled_generate_query_fragments().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn test_type_conditions_enabled_list_of_list() {
    _test_type_conditions_enabled_list_of_list().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn test_type_conditions_enabled_list_of_list_of_list() {
    _test_type_conditions_enabled_list_of_list_of_list().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn test_type_conditions_disabled() {
    _test_type_conditions_disabled().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn test_type_conditions_enabled_shouldnt_make_article_fetch() {
    _test_type_conditions_enabled_shouldnt_make_article_fetch().await;
}

async fn _test_type_conditions_enabled() -> Response {
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
    let request = supergraph::Request::fake_builder()
        .query(QUERY.to_string())
        .header("Apollo-Expose-Query-Plan", "true")
        .variable("movieResultParam", "movieResultEnabled")
        .variable("articleResultParam", "articleResultEnabled")
        .build()
        .expect("expecting valid request");

    let response = supergraph_service
        .oneshot(request)
        .await
        .unwrap()
        .next_response()
        .await
        .unwrap();

    let response = normalize_response_extensions(response);

    insta::assert_json_snapshot!(response);
    response
}

async fn _test_type_conditions_enabled_generate_query_fragments() -> Response {
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
                include_str!("fixtures/type_conditions/search_query_fragments_enabled.json"),
            ),
            (
                "artworkSubgraph",
                include_str!("fixtures/type_conditions/artwork_query_fragments_enabled.json"),
            ),
        ],
    );
    let supergraph_service = harness.build_supergraph().await.unwrap();
    let request = supergraph::Request::fake_builder()
        .query(QUERY.to_string())
        .header("Apollo-Expose-Query-Plan", "true")
        .variable("movieResultParam", "movieResultEnabled")
        .variable("articleResultParam", "articleResultEnabled")
        .build()
        .expect("expecting valid request");

    let response = supergraph_service
        .oneshot(request)
        .await
        .unwrap()
        .next_response()
        .await
        .unwrap();

    let response = normalize_response_extensions(response);

    insta::assert_json_snapshot!(response);
    response
}

async fn _test_type_conditions_enabled_list_of_list() -> Response {
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
    let request = supergraph::Request::fake_builder()
        .query(QUERY_LIST_OF_LIST.to_string())
        .header("Apollo-Expose-Query-Plan", "true")
        .variable("movieResultParam", "movieResultEnabled")
        .variable("articleResultParam", "articleResultEnabled")
        .build()
        .expect("expecting valid request");

    let response = supergraph_service
        .oneshot(request)
        .await
        .unwrap()
        .next_response()
        .await
        .unwrap();

    let response = normalize_response_extensions(response);

    insta::assert_json_snapshot!(response);
    response
}

// one last to make sure unnesting is correct
async fn _test_type_conditions_enabled_list_of_list_of_list() -> Response {
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
    let request = supergraph::Request::fake_builder()
        .query(QUERY_LIST_OF_LIST_OF_LIST.to_string())
        .header("Apollo-Expose-Query-Plan", "true")
        .variable("movieResultParam", "movieResultEnabled")
        .variable("articleResultParam", "articleResultEnabled")
        .build()
        .expect("expecting valid request");

    let response = supergraph_service
        .oneshot(request)
        .await
        .unwrap()
        .next_response()
        .await
        .unwrap();

    let response = normalize_response_extensions(response);

    insta::assert_json_snapshot!(response);
    response
}

async fn _test_type_conditions_disabled() -> Response {
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

    let response = normalize_response_extensions(response);

    insta::assert_json_snapshot!(response);
    response
}

async fn _test_type_conditions_enabled_shouldnt_make_article_fetch() -> Response {
    let harness = setup_from_mocks(
        json! {{
            "experimental_type_conditioned_fetching": true,
            // will make debugging easier
            "plugins": {
                "experimental.expose_query_plan": true
            },
            // TODO(@goto-bus-stop): need to update the mocks and remove this, #6013
            "supergraph": {
                "generate_query_fragments": false,
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
    let request = supergraph::Request::fake_builder()
        .query(QUERY.to_string())
        .header("Apollo-Expose-Query-Plan", "true")
        .variable("movieResultParam", "movieResultEnabled")
        .variable("articleResultParam", "articleResultEnabled")
        .build()
        .expect("expecting valid request");

    let response = supergraph_service
        .oneshot(request)
        .await
        .unwrap()
        .next_response()
        .await
        .unwrap();

    let response = normalize_response_extensions(response);

    insta::assert_json_snapshot!(response);
    response
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
            builder = builder.with_json(mock.request, mock.response);
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

fn normalize_response_extensions(mut response: Response) -> Response {
    response.extensions = reprint_operations(response.extensions);
    response
}

/// Rebuilds `value` with every `operation` string reprinted by apollo-compiler, so a snapshot holds
/// one canonical spelling of an operation whatever the query planner emitted.
fn reprint_operations(value: Value) -> Value {
    match value.kind() {
        JsonKind::Object => Value::object(value.object_iter().map(|(key, member)| {
            // Bound before the match, so the borrow of `member` ends before the arm that moves it.
            let string = member.as_string();
            let member = match string {
                Some(operation) if key == "operation" => reprint_operation(&operation),
                _ => reprint_operations(member),
            };
            (key, member)
        })),
        JsonKind::Array => Value::array(value.array_iter().map(reprint_operations)),
        _ => value,
    }
}

fn reprint_operation(operation: &str) -> Value {
    Document::parse(operation, "operation")
        .unwrap()
        .serialize()
        .no_indent()
        .to_string()
        .into()
}
