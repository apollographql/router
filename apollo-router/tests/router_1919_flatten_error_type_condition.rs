//! ROUTER-1919 regression test.
//!
//! A subgraph-level error with no `_entities`-indexed path (e.g. the error a `traffic_shaping`
//! timeout produces) on a `Flatten` fetch whose path crosses a type-conditioned abstract-typed
//! field must still surface for every entity in the batch, rather than being silently dropped.
//!
//! Schema shape: `Query.containers: [Container!]` (a plain list, `Flatten` with no type
//! condition) -> `Container.result: SearchResult` (a *single-valued* union field, `MovieResult |
//! ArticleResult`) -> `sections: [Section]`. Because `result` is single-valued, routing its two
//! possible types to two separate batched fetches (one per `$movieResultParam`/
//! `$articleResultParam`) requires the query planner to annotate the *`result` `Key` path element
//! itself* with a type condition (`result|[MovieResult]`), rather than a `Flatten` element -- this
//! is exactly the RH-1382 production shape (`...edges.@.node|[ConcreteType].collection`), and is
//! what actually exercises the bug (a schema where the type condition lands on a `Flatten` instead
//! -- e.g. a union-typed *list* field -- does not, since `Path::equal_if_flattened` already treated
//! `Flatten`/`Index` pairs as equal regardless of type condition).
//!
//! Root cause: the declared error path for such a fetch keeps its type condition
//! (`Key("result", Some(["MovieResult"]))`), but the real per-entity paths built during execution
//! have it stripped (`iterate_path` always writes back `Key(k, None)`). `Path::equal_if_flattened`,
//! used to match the error against each real entity path, only special-cased `Index`/`Flatten`
//! pairs and fell back to strict structural equality for everything else -- so a `Key` with a type
//! condition never matched a `Key` without one, the error matched zero of the real entity paths,
//! and was dropped entirely with no trace anywhere in the client response.

use apollo_router::MockedSubgraphs;
use apollo_router::TestHarness;
use apollo_router::graphql::JsonPathElement;
use apollo_router::graphql::Response;
use apollo_router::plugin::test::MockSubgraph;
use apollo_router::services::supergraph;
use serde::Deserialize;
use serde_json::json;
use serde_json_bytes::ByteString;
use serde_json_bytes::Value;
use tower::ServiceExt;

type JsonMap = serde_json_bytes::Map<ByteString, Value>;

#[derive(Deserialize)]
struct SubgraphMock {
    mocks: Vec<RequestAndResponse>,
}

#[derive(Deserialize)]
struct RequestAndResponse {
    request: apollo_router::graphql::Request,
    response: Response,
}

static QUERY: &str = r#"
query Test($movieResultParam: String, $articleResultParam: String) {
  containers {
    id
    result {
      ... on MovieResult {
        sections {
          ... on EntityCollectionSection {
            title
            artwork(params: $movieResultParam)
          }
          ... on GallerySection {
            artwork(params: $movieResultParam)
          }
        }
      }
      ... on ArticleResult {
        sections {
          ... on EntityCollectionSection {
            title
            artwork(params: $articleResultParam)
          }
          ... on GallerySection {
            artwork(params: $articleResultParam)
          }
        }
      }
    }
  }
}"#;

fn setup() -> TestHarness<'static> {
    let mut mocked_subgraphs = MockedSubgraphs::default();

    for (name, m) in [
        (
            "searchSubgraph",
            include_str!("fixtures/router_1919/search.json"),
        ),
        (
            "artworkSubgraph",
            include_str!("fixtures/router_1919/artwork.json"),
        ),
    ] {
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

    let schema = include_str!("fixtures/router_1919/router_1919.graphql");
    TestHarness::builder()
        .try_log_level("info")
        .configuration_json(json! {{
            "experimental_type_conditioned_fetching": true,
            "include_subgraph_errors": {
                "all": true
            }
        }})
        .unwrap()
        .schema(schema)
        .extra_plugin(mocked_subgraphs)
}

#[tokio::test(flavor = "multi_thread")]
async fn test_subgraph_error_not_dropped_on_flattened_type_conditioned_fetch() {
    let harness = setup();
    let supergraph_service = harness.build_supergraph().await.unwrap();
    let mut variables = JsonMap::new();
    variables.insert("movieResultParam", "movieResultEnabled".into());
    variables.insert("articleResultParam", "articleResultEnabled".into());
    let request = supergraph::Request::fake_builder()
        .query(QUERY.to_string())
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

    // The movie-batch artwork fetch (2 entities: EntityCollectionSection "ecs1" + GallerySection
    // "gs1") errored with no path, simulating a subgraph timeout. Before the fix this vanished
    // silently (`errors` would be empty). After the fix it must surface -- once per affected
    // entity, since the error applies to the whole batch.
    assert_eq!(
        response.errors.len(),
        2,
        "expected the error to be attached to each of the 2 entities in the errored (movie) \
         batch, got: {:#?}",
        response.errors
    );

    let mut error_paths: Vec<String> = response
        .errors
        .iter()
        .map(|error| {
            assert_eq!(error.message, "Your request has been timed out");
            assert_eq!(
                error.extensions.get("code").and_then(|c| c.as_str()),
                Some("GATEWAY_TIMEOUT")
            );
            let path = error.path.as_ref().expect("error must have a path");
            assert!(
                !path
                    .iter()
                    .any(|elem| matches!(elem, JsonPathElement::Flatten(_))),
                "error path must be a concrete per-entity path, not a dangling Flatten: {path:?}"
            );
            path.to_string()
        })
        .collect();
    error_paths.sort();
    assert_eq!(
        error_paths,
        vec![
            "/containers/0/result/sections/0",
            "/containers/0/result/sections/1"
        ],
        "the error must be attached to both entities of the errored movie batch specifically, \
         not the (unaffected) article batch"
    );

    // The article-batch artwork fetch was unaffected and its data must still come through
    // normally -- confirms the bug (and the fix) is scoped to the errored fetch only.
    let data = response.data.expect("expected some data");
    let article_container = &data["containers"][1];
    assert_eq!(article_container["id"], serde_json_bytes::json!("c2"));
    assert_eq!(
        article_container["result"]["sections"][0]["title"],
        serde_json_bytes::json!("ecs2 title")
    );
    assert_eq!(
        article_container["result"]["sections"][0]["artwork"],
        serde_json_bytes::json!("articleResultEnabled artwork")
    );
}
