use apollo_router::plugin::test::MockSubgraph;
use apollo_router::services::supergraph::Request;
use serde_json::json;
use tower::ServiceExt;

use crate::integration::IntegrationTest;
use crate::integration::common::Query;

#[tokio::test]
async fn simple() {
    let request = Request::fake_builder()
        .query("{ __schema { queryType { name } } }")
        .build()
        .unwrap();
    let response = make_request(request).await;
    insta::assert_json_snapshot!(response, @r###"
    {
      "data": {
        "__schema": {
          "queryType": {
            "name": "Query"
          }
        }
      }
    }
    "###);
}

#[tokio::test]
async fn top_level_inline_fragment() {
    let request = Request::fake_builder()
        .query("{ ... { __schema { queryType { name } } } }")
        .build()
        .unwrap();
    let response = make_request(request).await;
    insta::assert_json_snapshot!(response, @r###"
    {
      "data": {
        "__schema": {
          "queryType": {
            "name": "Query"
          }
        }
      }
    }
    "###);
}

#[tokio::test]
async fn variable() {
    let request = Request::fake_builder()
        .query(
            r#"
                query($d: Boolean!) {
                    __type(name: "Query") {
                        fields(includeDeprecated: $d) {
                            name
                        }
                    }
                }
            "#,
        )
        .variable("d", true)
        .build()
        .unwrap();
    let response = make_request(request).await;
    insta::assert_json_snapshot!(response, @r###"
    {
      "errors": [
        {
          "message": "missing value for non-null variable 'd'",
          "locations": [
            {
              "line": 2,
              "column": 23
            }
          ]
        }
      ]
    }
    "###);
}

#[tokio::test]
async fn two_operations() {
    let request = Request::fake_builder()
        .query(
            r#"
                query ThisOp { __schema { queryType { name } } }
                query OtherOp { me { id } }
            "#,
        )
        .operation_name("ThisOp")
        .build()
        .unwrap();
    let response = make_request(request).await;
    insta::assert_json_snapshot!(response, @r###"
    {
      "data": {
        "__schema": {
          "queryType": {
            "name": "Query"
          }
        }
      }
    }
    "###);
}

#[tokio::test]
async fn mixed() {
    let request = Request::fake_builder()
        .query(
            r#"{
                __schema { queryType { name } }
                me { id }
            }"#,
        )
        .build()
        .unwrap();
    let response = make_request(request).await;
    insta::assert_json_snapshot!(response, @r###"
    {
      "errors": [
        {
          "message": "Mixed queries with both schema introspection and concrete fields are not supported yet: https://github.com/apollographql/router/issues/2789",
          "extensions": {
            "code": "MIXED_INTROSPECTION"
          }
        }
      ]
    }
    "###);
}

const QUERY_DEPTH_2: &str = r#"{
  __type(name: "Query") {
    fields {
      type {
        fields {
          type {
            kind
          }
        }
      }
    }
  }
}"#;

const QUERY_DEPTH_3: &str = r#"{
  __type(name: "Query") {
    fields {
      type {
        fields {
          type {
            fields {
              name
            }
          }
        }
      }
    }
  }
}"#;

#[tokio::test]
async fn just_under_max_depth() {
    let request = Request::fake_builder()
        .query(QUERY_DEPTH_2)
        .build()
        .unwrap();
    let response = make_request(request).await;
    insta::assert_json_snapshot!(response, @r###"
    {
      "data": {
        "__type": {
          "fields": [
            {
              "type": {
                "fields": [
                  {
                    "type": {
                      "kind": "NON_NULL"
                    }
                  },
                  {
                    "type": {
                      "kind": "SCALAR"
                    }
                  },
                  {
                    "type": {
                      "kind": "SCALAR"
                    }
                  },
                  {
                    "type": {
                      "kind": "LIST"
                    }
                  }
                ]
              }
            },
            {
              "type": {
                "fields": null
              }
            }
          ]
        }
      }
    }
    "###);
}

#[tokio::test]
async fn just_over_max_depth() {
    let request = Request::fake_builder()
        .query(QUERY_DEPTH_3)
        .build()
        .unwrap();
    let response = make_request(request).await;
    insta::assert_json_snapshot!(response, @r###"
    {
      "errors": [
        {
          "message": "Maximum introspection depth exceeded",
          "locations": [
            {
              "line": 7,
              "column": 13
            }
          ]
        }
      ]
    }
    "###);
}

#[tokio::test]
async fn just_over_max_depth_with_check_disabled() {
    let request = Request::fake_builder()
        .query(QUERY_DEPTH_3)
        .build()
        .unwrap();
    let response = make_request_with_extra_config(request, |conf| {
        conf.as_object_mut().unwrap().insert(
            "limits".to_owned(),
            json!({"introspection_max_depth": false}),
        );
    })
    .await;
    insta::assert_json_snapshot!(response, @r###"
    {
      "data": {
        "__type": {
          "fields": [
            {
              "type": {
                "fields": [
                  {
                    "type": {
                      "fields": null
                    }
                  },
                  {
                    "type": {
                      "fields": null
                    }
                  },
                  {
                    "type": {
                      "fields": null
                    }
                  },
                  {
                    "type": {
                      "fields": null
                    }
                  }
                ]
              }
            },
            {
              "type": {
                "fields": null
              }
            }
          ]
        }
      }
    }
    "###);
}

async fn make_request(request: Request) -> apollo_router::graphql::Response {
    make_request_with_extra_config(request, |_| {}).await
}

async fn make_request_with_extra_config(
    request: Request,
    modify_config: impl FnOnce(&mut serde_json::Value),
) -> apollo_router::graphql::Response {
    let mut conf = json!({
        "supergraph": {
            "introspection": true,
        },
        "include_subgraph_errors": {
            "all": true,
        },
    });
    modify_config(&mut conf);
    apollo_router::TestHarness::builder()
        .configuration_json(conf)
        .unwrap()
        .subgraph_hook(|subgraph_name, default| match subgraph_name {
            "accounts" => MockSubgraph::builder()
                .with_json(
                    json!({"query": "{me{id}}"}),
                    json!({"data": {"me": {"id": 1}}}),
                )
                .build()
                .boxed(),
            _ => default,
        })
        .build_supergraph()
        .await
        .unwrap()
        .oneshot(request)
        .await
        .unwrap()
        .next_response()
        .await
        .unwrap()
}

#[tokio::test]
async fn integration() {
    let mut router = IntegrationTest::builder()
        .config(
            "
                supergraph:
                    introspection: true
            ",
        )
        .supergraph("tests/fixtures/schema_to_introspect.graphql")
        .build()
        .await;
    router.start().await;
    router.assert_started().await;
    let query = json!({
        "query": include_str!("../fixtures/introspect_full_schema.graphql"),
    });
    let (_trace_id, response) = router
        .execute_query(Query::builder().body(query).build())
        .await;
    insta::assert_json_snapshot!(response.json::<serde_json::Value>().await.unwrap());
    router.graceful_shutdown().await;
}
