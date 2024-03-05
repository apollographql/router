//!
//! Please ensure that any tests added to this file use the tokio multi-threaded test executor.
//!

use apollo_router::plugin::test::MockSubgraph;
use apollo_router::services::supergraph;
use apollo_router::{MockedSubgraphs, TestHarness};
use serde_json::json;
use tower::ServiceExt;

mod integration;


#[tokio::test(flavor = "multi_thread")]
async fn test_type_conditions_enabled() {
    let harness = setup(json! {{
        "experimental_type_conditioned_fetching": true
    }});
    let supergraph_service = harness.build_supergraph().await.unwrap();
    let request = supergraph::Request::fake_builder()
        .query(QUERY.to_string())
        .build()
        .expect("expecting valid request")
        .try_into()
        .unwrap();

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
    let harness = setup(json! {{
        "experimental_type_conditioned_fetching": false
    }});
    let supergraph_service = harness.build_supergraph().await.unwrap();
    let request = supergraph::Request::fake_builder()
        .query(QUERY.to_string())
        .build()
        .expect("expecting valid request")
        .try_into()
        .unwrap();

    let response = supergraph_service
        .oneshot(request)
        .await
        .unwrap()
        .next_response()
        .await
        .unwrap();

    insta::assert_json_snapshot!(response);
}

fn setup(configuration: serde_json::Value) -> TestHarness<'static> {
    let search_service =  MockSubgraph::builder().with_json(json!{{
        "query":"query Search__searchSubgraph__0{search{__typename ...on MovieResult{sections{__typename ...on EntityCollectionSection{__typename id}...on GallerySection{__typename id}}id}...on ArticleResult{id sections{__typename ...on GallerySection{__typename id}...on EntityCollectionSection{__typename id}}}}}",
        "operationName":"Search__searchSubgraph__0"
    }},
json!{{
        "data": {
            "search":[
                {
                    "__typename":"ArticleResult",
                    "id":"ff70d1f5-d1ac-46dd-8ed1-5f2d81ff2db0",
                    "sections":[
                        {
                            "__typename":"EntityCollectionSection",
                            "id":"a7487f33-bd37-48a6-a843-c0cda86f5049"
                        },
                        {
                            "__typename":"EntityCollectionSection",
                            "id":"cdb43f1d-df2d-4293-a328-8d38a0cdd742"
                        }
                    ]
                },
                {
                    "__typename":"ArticleResult",
                    "id":"5092bbea-8bc3-4c4f-a9eb-003604ed9add",
                    "sections":[
                        {
                            "__typename":"GallerySection",
                            "id":"798e75ae-9378-41de-a014-af9f9a5e99eb"
                        },
                        {
                            "__typename":"GallerySection",
                            "id":"f756501a-7377-4081-861b-0097cbfb7f41"
                        }
                    ]
                }
            ]
        }
    }}).build();

    let artwork_service = MockSubgraph::builder().with_json(json!{{
        "query":"query Search__artworkSubgraph__1($representations:[_Any!]!){_entities(representations:$representations){...on EntityCollectionSection{artwork title}...on GallerySection{artwork}}}","operationName":"Search__artworkSubgraph__1",
        "variables":{
            "representations":[
                {
                    "__typename":"EntityCollectionSection",
                    "id":"a7487f33-bd37-48a6-a843-c0cda86f5049"
                },
                {
                    "__typename":"EntityCollectionSection",
                    "id":"cdb43f1d-df2d-4293-a328-8d38a0cdd742"
                },
                {
                    "__typename":"GallerySection",
                    "id":"798e75ae-9378-41de-a014-af9f9a5e99eb"
                },
                {
                    "__typename":"GallerySection",
                    "id":"f756501a-7377-4081-861b-0097cbfb7f41"
                }
            ]
        }
    }},
json!{{
        "data":{
            "_entities":[
                {
                    "artwork":"Hello World",
                    "title":"Hello World"
                },
                {
                    "artwork":"Hello World",
                    "title":"Hello World"
                }
            ]
        }
    }}).build();

    let mut mocks = MockedSubgraphs::default();
    mocks.insert("searchSubgraph", search_service);
    mocks.insert("artworkSubgraph", artwork_service);

    let schema = include_str!("fixtures/type_conditions.graphql");
    TestHarness::builder()
        .try_log_level("info")
        .configuration_json(configuration)
        .unwrap()
        .schema(schema)
        .extra_plugin(mocks)
}

static QUERY: &str = "
query Search {
    search {
      ... on MovieResult {
        sections {
          ... on EntityCollectionSection {
            artwork
            id
            title
          }
          ... on GallerySection {
            artwork
            id
          }
        }
        id
      }
      ... on ArticleResult {
        id
        sections {
          ... on GallerySection {
            artwork
          }
          ... on EntityCollectionSection {
            artwork
          }
        }
      }
    }
}";
