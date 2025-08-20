//!
//! Please ensure that any tests added to this file use the tokio multi-threaded test executor.
//!

use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::ffi::OsStr;
use std::sync::Arc;

use apollo_router::_private::create_test_service_factory_from_yaml;
use apollo_router::Configuration;
use apollo_router::Context;
use apollo_router::graphql;
use apollo_router::graphql::Error;
use apollo_router::plugin::Plugin;
use apollo_router::plugin::PluginInit;
use apollo_router::services::router;
use apollo_router::services::subgraph;
use apollo_router::services::supergraph;
use apollo_router::test_harness::mocks::persisted_queries::*;
use futures::StreamExt;
use http::HeaderValue;
use http::Method;
use http::StatusCode;
use http::Uri;
use http::header::ACCEPT;
use http::header::CONTENT_TYPE;
use maplit::hashmap;
use mime::APPLICATION_JSON;
use parking_lot::Mutex;
use serde_json_bytes::json;
use tower::BoxError;
use tower::ServiceExt;
use uuid::Uuid;
use walkdir::DirEntry;
use walkdir::WalkDir;

mod integration;

#[tokio::test(flavor = "multi_thread")]
async fn api_schema_hides_field() {
    let request = supergraph::Request::fake_builder()
        .query(r#"{ topProducts { name inStock } }"#)
        .variable("topProductsFirst", 2_i32)
        .variable("reviewsForAuthorAuthorId", 1_i32)
        .build()
        .expect("expecting valid request");

    let (actual, _) = query_rust(request).await;

    let message = &actual.errors[0].message;
    assert!(
        message.contains(r#"Cannot query field "inStock" on type "Product"."#),
        "{message}"
    );
    assert_eq!(
        actual.errors[0].extensions["code"].as_str(),
        Some("GRAPHQL_VALIDATION_FAILED"),
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn validation_errors_from_rust() {
    let request = supergraph::Request::fake_builder()
        .query(r#"{ topProducts { name(notAnArg: true) } } fragment Unused on Product { upc }"#)
        .build()
        .expect("expecting valid request");

    let (response, _) = query_rust_with_config(
        request,
        serde_json::json!({
            "telemetry":{
              "apollo": {
                    "field_level_instrumentation_sampler": "always_off"
                }
            }
        }),
    )
    .await;

    insta::assert_json_snapshot!(response.errors);
}

#[tokio::test(flavor = "multi_thread")]
async fn queries_should_work_over_get() {
    // get request
    let get_request = supergraph::Request::builder()
        .query("{ topProducts { upc name reviews {id product { name } author { id name } } } }")
        .variable("topProductsFirst", 2_usize)
        .variable("reviewsForAuthorAuthorId", 1_usize)
        .header(CONTENT_TYPE, APPLICATION_JSON.essence_str())
        .uri(Uri::from_static("/"))
        .method(Method::GET)
        .context(Context::new())
        .build()
        .unwrap()
        .try_into()
        .unwrap();

    let expected_service_hits = hashmap! {
        "products".to_string()=>2,
        "reviews".to_string()=>1,
        "accounts".to_string()=>1,
    };

    let (actual, registry) = {
        let (router, counting_registry) = setup_router_and_registry(serde_json::json!({})).await;
        (
            query_with_router(router, get_request).await,
            counting_registry,
        )
    };
    assert_eq!(0, actual.errors.len());
    assert_eq!(registry.totals(), expected_service_hits);
}

#[tokio::test(flavor = "multi_thread")]
async fn simple_queries_should_not_work() {
    let message = "This operation has been blocked as a potential Cross-Site Request Forgery (CSRF). \
    Please either specify a 'content-type' header \
    (with a mime-type that is not one of application/x-www-form-urlencoded, multipart/form-data, text/plain) \
    or provide one of the following headers: x-apollo-operation-name, apollo-require-preflight";

    let mut get_request: router::Request = supergraph::Request::builder()
        .query("{ topProducts { upc name reviews {id product { name } author { id name } } } }")
        .variable("topProductsFirst", 2_usize)
        .variable("reviewsForAuthorAuthorId", 1_usize)
        .header(CONTENT_TYPE, APPLICATION_JSON.essence_str())
        .uri(Uri::from_static("/"))
        .method(Method::GET)
        .context(Context::new())
        .build()
        .unwrap()
        .try_into()
        .unwrap();

    get_request
        .router_request
        .headers_mut()
        .remove("content-type");

    let (router, registry) = setup_router_and_registry(serde_json::json!({})).await;

    let actual = query_with_router(router, get_request).await;

    let expected_error = graphql::Error::builder()
        .message(message)
        .extension_code("CSRF_ERROR")
        // Overwrite error ID to avoid comparing random Uuids
        .apollo_id(actual.errors[0].apollo_id())
        .build();

    assert_eq!(
        1,
        actual.errors.len(),
        "CSRF should have rejected this query"
    );
    assert_eq!(expected_error, actual.errors[0]);
    assert_eq!(registry.totals(), hashmap! {});
}

#[tokio::test(flavor = "multi_thread")]
async fn empty_posts_should_not_work() {
    let request = http::Request::builder()
        .header(
            CONTENT_TYPE,
            HeaderValue::from_static(APPLICATION_JSON.essence_str()),
        )
        .method(Method::POST)
        .body(axum::body::Body::empty())
        .unwrap();

    let (router, registry) = setup_router_and_registry(serde_json::json!({})).await;

    let actual = query_with_router(router, request.into()).await;

    assert_eq!(1, actual.errors.len());

    let message = "Invalid GraphQL request";
    let mut extensions_map = serde_json_bytes::map::Map::new();
    extensions_map.insert("code", "INVALID_GRAPHQL_REQUEST".into());
    extensions_map.insert("details", "failed to deserialize the request body into JSON: EOF while parsing a value at line 1 column 0".into());
    let expected_error = graphql::Error::builder()
        .message(message)
        .extension_code("INVALID_GRAPHQL_REQUEST")
        .extensions(extensions_map)
        .apollo_id(actual.errors[0].apollo_id())
        .build();
    assert_eq!(expected_error, actual.errors[0]);
    assert_eq!(registry.totals(), hashmap! {});
}

#[tokio::test(flavor = "multi_thread")]
async fn queries_should_work_with_compression() {
    let request = supergraph::Request::fake_builder()
        .query(r#"{ topProducts { upc name reviews {id product { name } author { id name } } } }"#)
        .variable("topProductsFirst", 2_i32)
        .variable("reviewsForAuthorAuthorId", 1_i32)
        .method(Method::POST)
        .header(CONTENT_TYPE, APPLICATION_JSON.essence_str())
        .header("accept-encoding", "gzip")
        .build()
        .expect("expecting valid request");

    let expected_service_hits = hashmap! {
        "products".to_string()=>2,
        "reviews".to_string()=>1,
        "accounts".to_string()=>1,
    };

    let (actual, registry) = query_rust(request).await;

    assert_eq!(0, actual.errors.len());
    assert_eq!(registry.totals(), expected_service_hits);
}

#[tokio::test(flavor = "multi_thread")]
async fn queries_should_work_over_post() {
    let request = supergraph::Request::fake_builder()
        .query(r#"{ topProducts { upc name reviews {id product { name } author { id name } } } }"#)
        .variable("topProductsFirst", 2_i32)
        .variable("reviewsForAuthorAuthorId", 1_i32)
        .method(Method::POST)
        .build()
        .expect("expecting valid request");

    let expected_service_hits = hashmap! {
        "products".to_string()=>2,
        "reviews".to_string()=>1,
        "accounts".to_string()=>1,
    };

    let (actual, registry) = query_rust(request).await;

    assert_eq!(0, actual.errors.len());
    assert_eq!(registry.totals(), expected_service_hits);
}

#[tokio::test(flavor = "multi_thread")]
async fn service_errors_should_be_propagated() {
    let message = "Unknown operation named \"invalidOperationName\"";
    let mut extensions_map = serde_json_bytes::map::Map::new();
    extensions_map.insert("code", "GRAPHQL_UNKNOWN_OPERATION_NAME".into());

    let request = supergraph::Request::fake_builder()
        .query(r#"{ topProducts { name } }"#)
        .operation_name("invalidOperationName")
        .build()
        .expect("expecting valid request");

    let expected_service_hits = hashmap! {};

    let (actual, registry) = query_rust(request).await;

    let expected_error = apollo_router::graphql::Error::builder()
        .message(message)
        .extensions(extensions_map)
        .extension_code("VALIDATION_ERROR")
        // Overwrite error ID to avoid comparing random Uuids
        .apollo_id(actual.errors[0].apollo_id())
        .build();

    assert_eq!(expected_error, actual.errors[0]);
    assert_eq!(registry.totals(), expected_service_hits);
}

#[tokio::test(flavor = "multi_thread")]
async fn mutation_should_not_work_over_get() {
    // get request
    let get_request: router::Request = supergraph::Request::builder()
        .query(
            r#"mutation {
            createProduct(upc:"8", name:"Bob") {
              upc
              name
              reviews {
                body
              }
            }
            createReview(upc: "8", id:"100", body: "Bif"){
              id
              body
            }
          }"#,
        )
        .variable("topProductsFirst", 2_usize)
        .variable("reviewsForAuthorAuthorId", 1_usize)
        .header(CONTENT_TYPE, APPLICATION_JSON.essence_str())
        .uri(Uri::from_static("/"))
        .method(Method::GET)
        .context(Context::new())
        .build()
        .unwrap()
        .try_into()
        .unwrap();

    // No services should be queried
    let expected_service_hits = hashmap! {};

    let (actual, registry) = {
        let (router, counting_registry) = setup_router_and_registry(serde_json::json!({})).await;
        (
            query_with_router(router, get_request).await,
            counting_registry,
        )
    };

    assert_eq!(1, actual.errors.len());
    assert_eq!(registry.totals(), expected_service_hits);
}

#[tokio::test(flavor = "multi_thread")]
async fn mutation_should_work_over_post() {
    let request = supergraph::Request::fake_builder()
        .query(
            r#"mutation {
            createProduct(upc:"8", name:"Bob") {
              upc
              name
              reviews {
                body
              }
            }
            createReview(upc: "8", id:"100", body: "Bif"){
              id
              body
            }
          }"#,
        )
        .variable("topProductsFirst", 2_i32)
        .variable("reviewsForAuthorAuthorId", 1_i32)
        .method(Method::POST)
        .build()
        .expect("expecting valid request");

    let expected_service_hits = hashmap! {
        "products".to_string()=>1,
        "reviews".to_string()=>2,
    };

    let (actual, registry) = query_rust(request).await;

    assert_eq!(0, actual.errors.len());
    assert_eq!(registry.totals(), expected_service_hits);
}

#[tokio::test(flavor = "multi_thread")]
async fn automated_persisted_queries() {
    let (router, registry) = setup_router_and_registry(serde_json::json!({})).await;

    let persisted = json!({
        "version" : 1u8,
        "sha256Hash" : "9d1474aa069127ff795d3412b11dfc1f1be0853aed7a54c4a619ee0b1725382e"
    });

    let apq_only_request = supergraph::Request::fake_builder()
        .extension("persistedQuery", persisted.clone())
        .build()
        .expect("expecting valid request");

    // First query, apq hash but no query, it will be a cache miss.

    // No services should be queried
    let expected_service_hits = hashmap! {};

    let actual = query_with_router(router.clone(), apq_only_request.try_into().unwrap()).await;

    let expected_apq_miss_error = apollo_router::graphql::Error::builder()
        .message("PersistedQueryNotFound")
        .extension_code("PERSISTED_QUERY_NOT_FOUND")
        .apollo_id(actual.errors[0].apollo_id())
        .build();

    assert_eq!(expected_apq_miss_error, actual.errors[0]);
    assert_eq!(1, actual.errors.len());
    assert_eq!(registry.totals(), expected_service_hits);

    // Second query, apq hash with corresponding query, it will be inserted into the cache.

    let apq_request_with_query = supergraph::Request::fake_builder()
        .extension("persistedQuery", persisted.clone())
        .query("query Query { me { name } }")
        .build()
        .expect("expecting valid request");

    // Services should have been queried once
    let expected_service_hits = hashmap! {
        "accounts".to_string()=>1,
    };

    let actual =
        query_with_router(router.clone(), apq_request_with_query.try_into().unwrap()).await;

    assert_eq!(0, actual.errors.len());
    assert_eq!(registry.totals(), expected_service_hits);

    // Third and last query, apq hash without query, it will trigger an apq cache hit.
    let apq_only_request = supergraph::Request::fake_builder()
        .extension("persistedQuery", persisted)
        .build()
        .expect("expecting valid request");

    // Services should have been queried twice
    let expected_service_hits = hashmap! {
        "accounts".to_string()=>2,
    };

    let actual = query_with_router(router, apq_only_request.try_into().unwrap()).await;

    assert_eq!(0, actual.errors.len());
    assert_eq!(registry.totals(), expected_service_hits);
}

#[tokio::test(flavor = "multi_thread")]
async fn persisted_queries() {
    use hyper::header::HeaderValue;
    use serde_json::json;

    /// Construct a persisted query request from an ID.
    fn pq_request(persisted_query_id: &str) -> router::Request {
        supergraph::Request::fake_builder()
            .extension(
                "persistedQuery",
                json!({
                    "version": 1,
                    "sha256Hash": persisted_query_id
                }),
            )
            .build()
            .expect("expecting valid request")
            .try_into()
            .expect("could not convert supergraph::Request to router::Request")
    }

    // set up a PQM with one query
    const PERSISTED_QUERY_ID: &str = "GetMyNameID";
    const PERSISTED_QUERY_BODY: &str = "query GetMyName { me { name } }";
    let expected_data = serde_json_bytes::json!({
      "me": {
        "name": "Ada Lovelace"
      }
    });
    let manifest = PersistedQueryManifest::from(vec![ManifestOperation {
        id: PERSISTED_QUERY_ID.to_string(),
        body: PERSISTED_QUERY_BODY.to_string(),
        client_name: None,
    }]);
    let (_mock_guard, uplink_config) = mock_pq_uplink(&manifest).await;

    let config = serde_json::json!({
        "persisted_queries": {
            "enabled": true
        },
        "apq": {
            "enabled": false
        }
    });

    let mut config: Configuration = serde_json::from_value(config).unwrap();
    config.uplink = Some(uplink_config);
    let (router, registry) = setup_router_and_registry_with_config(config).await.unwrap();

    // Successfully run a persisted query.
    let actual = query_with_router(router.clone(), pq_request(PERSISTED_QUERY_ID)).await;
    assert!(actual.errors.is_empty());
    assert_eq!(actual.data.as_ref(), Some(&expected_data));
    assert_eq!(registry.totals(), hashmap! {"accounts".to_string() => 1});

    // Error on unpersisted query.
    const UNKNOWN_QUERY_ID: &str = "unknown_query";
    const UNPERSISTED_QUERY_BODY: &str = "query GetYourName { you: me { name } }";
    let expected_data = serde_json_bytes::json!({
      "you": {
        "name": "Ada Lovelace"
      }
    });
    let actual = query_with_router(router.clone(), pq_request(UNKNOWN_QUERY_ID)).await;
    assert_eq!(
        actual.errors,
        vec![
            apollo_router::graphql::Error::builder()
                .message(format!(
                    "Persisted query '{UNKNOWN_QUERY_ID}' not found in the persisted query list"
                ))
                .extension_code("PERSISTED_QUERY_NOT_IN_LIST")
                // Overwrite error ID to avoid comparing random Uuids
                .apollo_id(actual.errors[0].apollo_id())
                .build()
        ]
    );
    assert_eq!(actual.data, None);
    assert_eq!(registry.totals(), hashmap! {"accounts".to_string() => 1});

    // We didn't break normal GETs.
    let actual = query_with_router(
        router.clone(),
        supergraph::Request::fake_builder()
            .query(UNPERSISTED_QUERY_BODY)
            .method(Method::GET)
            .build()
            .unwrap()
            .try_into()
            .unwrap(),
    )
    .await;
    assert!(actual.errors.is_empty());
    assert_eq!(actual.data.as_ref(), Some(&expected_data));
    assert_eq!(registry.totals(), hashmap! {"accounts".to_string() => 2});

    // We didn't break normal POSTs.
    let actual = query_with_router(
        router.clone(),
        supergraph::Request::fake_builder()
            .query(UNPERSISTED_QUERY_BODY)
            .method(Method::POST)
            .build()
            .unwrap()
            .try_into()
            .unwrap(),
    )
    .await;
    assert!(actual.errors.is_empty());
    assert_eq!(actual.data, Some(expected_data));
    assert_eq!(registry.totals(), hashmap! {"accounts".to_string() => 3});

    // Proper error when sending malformed request body
    let actual = query_with_router(
        router.clone(),
        http::Request::builder()
            .uri("http://default")
            .method(Method::POST)
            .header(
                CONTENT_TYPE,
                HeaderValue::from_static(APPLICATION_JSON.essence_str()),
            )
            .body(axum::body::Body::empty())
            .unwrap()
            .into(),
    )
    .await;
    assert_eq!(actual.errors.len(), 1);

    assert_eq!(actual.errors[0].message, "Invalid GraphQL request");
    assert_eq!(
        actual.errors[0].extensions["code"],
        "INVALID_GRAPHQL_REQUEST"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn missing_variables() {
    let request = supergraph::Request::fake_builder()
        .query(
            r#"
            query ExampleQuery(
                $missingVariable: Int!,
                $yetAnotherMissingVariable: ID!,
            ) {
                topProducts(first: $missingVariable) {
                    name
                    reviewsForAuthor(authorID: $yetAnotherMissingVariable) {
                        body
                    }
                }
            }
            "#,
        )
        .method(Method::POST)
        .build()
        .expect("expecting valid request");

    let (mut http_response, _) = http_query_rust(request).await;

    assert_eq!(StatusCode::BAD_REQUEST, http_response.response.status());

    let response = serde_json::from_slice::<graphql::Response>(
        http_response
            .next_response()
            .await
            .unwrap()
            .unwrap()
            .to_vec()
            .as_slice(),
    )
    .unwrap();

    let mut normalized_actual_errors = normalize_errors(response.errors);
    normalized_actual_errors.sort_by_key(|e| e.message.clone());

    let mut expected_errors = vec![
        graphql::Error::builder()
            .message("missing variable `$missingVariable`: for required GraphQL type `Int!`")
            .extension_code("VALIDATION_INVALID_TYPE_VARIABLE")
            .extension("name", "missingVariable")
            .apollo_id(Uuid::nil())
            .build(),
        graphql::Error::builder()
            .message(
                "missing variable `$yetAnotherMissingVariable`: for required GraphQL type `ID!`",
            )
            .extension_code("VALIDATION_INVALID_TYPE_VARIABLE")
            .extension("name", "yetAnotherMissingVariable")
            .apollo_id(Uuid::nil())
            .build(),
    ];

    expected_errors.sort_by_key(|e| e.message.clone());
    assert_eq!(normalized_actual_errors, expected_errors);
}

/// <https://github.com/apollographql/router/issues/2984>
#[tokio::test(flavor = "multi_thread")]
async fn input_object_variable_validation() {
    let schema = r#"
        schema
          @link(url: "https://specs.apollo.dev/link/v1.0")
          @link(url: "https://specs.apollo.dev/join/v0.3", for: EXECUTION)
        {
          query: Query
        }

        directive @join__enumValue(graph: join__Graph!) repeatable on ENUM_VALUE

        directive @join__field(graph: join__Graph, requires: join__FieldSet, provides: join__FieldSet, type: String, external: Boolean, override: String, usedOverridden: Boolean) repeatable on FIELD_DEFINITION | INPUT_FIELD_DEFINITION

        directive @join__graph(name: String!, url: String!) on ENUM_VALUE

        directive @join__implements(graph: join__Graph!, interface: String!) repeatable on OBJECT | INTERFACE

        directive @join__type(graph: join__Graph!, key: join__FieldSet, extension: Boolean! = false, resolvable: Boolean! = true, isInterfaceObject: Boolean! = false) repeatable on OBJECT | INTERFACE | UNION | ENUM | INPUT_OBJECT | SCALAR

        directive @join__unionMember(graph: join__Graph!, member: String!) repeatable on UNION

        directive @link(url: String, as: String, for: link__Purpose, import: [link__Import]) repeatable on SCHEMA

        input CoordinatesInput
          @join__type(graph: SUBGRAPH1)
        {
          latitude: Float!
          longitude: Float!
        }

        scalar join__FieldSet

        enum join__Graph {
          SUBGRAPH1 @join__graph(name: "subgraph1", url: "http://localhost:4001")
        }

        scalar link__Import

        enum link__Purpose {
          """
          `SECURITY` features provide metadata necessary to securely resolve fields.
          """
          SECURITY

          """
          `EXECUTION` features provide metadata necessary for operation execution.
          """
          EXECUTION
        }

        input MyInput
          @join__type(graph: SUBGRAPH1)
        {
          coordinates: [CoordinatesInput]
        }

        type Query
          @join__type(graph: SUBGRAPH1)
        {
          getData(params: MyInput): Int
        }
    "#;
    let request = apollo_router::services::supergraph::Request::fake_builder()
        .query("query($x: MyInput) { getData(params: $x) }")
        .variable(
            "x",
            json!({"coordinates": [{"latitude": 45.5, "longitude": null}]}),
        )
        .build()
        .unwrap();
    let response = apollo_router::TestHarness::builder()
        .schema(schema)
        .build_supergraph()
        .await
        .unwrap()
        .oneshot(request)
        .await
        .unwrap()
        .next_response()
        .await
        .unwrap();
    let normalized_errors = normalize_errors(response.errors);
    insta::assert_debug_snapshot!(normalized_errors, @r###"
    [
        Error {
            message: "missing input value at `$x.coordinates[0].longitude`: for required GraphQL type `Float!`",
            locations: [],
            path: None,
            extensions: {
                "name": String(
                    "x",
                ),
                "code": String(
                    "VALIDATION_INVALID_TYPE_VARIABLE",
                ),
            },
            apollo_id: 00000000-0000-0000-0000-000000000000,
        },
    ]
    "###);
}

const PARSER_LIMITS_TEST_QUERY: &str =
    r#"{ me { reviews { author { reviews { author { name } } } } } }"#;
const PARSER_LIMITS_TEST_QUERY_TOKEN_COUNT: usize = 36;
const PARSER_LIMITS_TEST_QUERY_RECURSION: usize = 6;
#[tokio::test(flavor = "multi_thread")]
async fn query_just_under_recursion_limit() {
    let config = serde_json::json!({
        "limits": {
            "parser_max_recursion": PARSER_LIMITS_TEST_QUERY_RECURSION
        }
    });
    let request = supergraph::Request::fake_builder()
        .query(PARSER_LIMITS_TEST_QUERY)
        .build()
        .expect("expecting valid request");

    let expected_service_hits = hashmap! {
        "reviews".to_string() => 1,
        "accounts".to_string() => 2,
    };

    let (actual, registry) = query_rust_with_config(request, config).await;

    assert_eq!(0, actual.errors.len());
    assert_eq!(registry.totals(), expected_service_hits);
}

#[tokio::test(flavor = "multi_thread")]
async fn query_just_at_recursion_limit() {
    let config = serde_json::json!({
        "limits": {
            "parser_max_recursion": PARSER_LIMITS_TEST_QUERY_RECURSION - 1
        }
    });
    let request = supergraph::Request::fake_builder()
        .query(PARSER_LIMITS_TEST_QUERY)
        .build()
        .expect("expecting valid request");

    let expected_service_hits = hashmap! {};

    let (mut http_response, registry) = http_query_rust_with_config(request, config).await;
    let actual = serde_json::from_slice::<graphql::Response>(
        http_response
            .next_response()
            .await
            .unwrap()
            .unwrap()
            .to_vec()
            .as_slice(),
    )
    .unwrap();

    assert_eq!(1, actual.errors.len());
    let message = &actual.errors[0].message;
    assert!(
        message.contains("parser recursion limit reached"),
        "{message}"
    );
    assert_eq!(registry.totals(), expected_service_hits);
}

#[tokio::test(flavor = "multi_thread")]
async fn query_just_under_token_limit() {
    let config = serde_json::json!({
        "limits": {
            "parser_max_tokens": PARSER_LIMITS_TEST_QUERY_TOKEN_COUNT,
        }
    });
    let request = supergraph::Request::fake_builder()
        .query(PARSER_LIMITS_TEST_QUERY)
        .build()
        .expect("expecting valid request");

    let expected_service_hits = hashmap! {
        "reviews".to_string() => 1,
        "accounts".to_string() => 2,
    };

    let (actual, registry) = query_rust_with_config(request, config).await;

    assert_eq!(actual.errors, []);
    assert_eq!(registry.totals(), expected_service_hits);
}

#[tokio::test(flavor = "multi_thread")]
async fn query_just_at_token_limit() {
    let config = serde_json::json!({
        "limits": {
            "parser_max_tokens": PARSER_LIMITS_TEST_QUERY_TOKEN_COUNT - 1,
        }
    });
    let request = supergraph::Request::fake_builder()
        .query(PARSER_LIMITS_TEST_QUERY)
        .build()
        .expect("expecting valid request");

    let expected_service_hits = hashmap! {};

    let (mut http_response, registry) = http_query_rust_with_config(request, config).await;
    let actual = serde_json::from_slice::<graphql::Response>(
        http_response
            .next_response()
            .await
            .unwrap()
            .unwrap()
            .to_vec()
            .as_slice(),
    )
    .unwrap();

    assert_eq!(1, actual.errors.len());
    assert!(actual.errors[0].message.contains("token limit reached"));
    assert_eq!(registry.totals(), expected_service_hits);
}

#[tokio::test(flavor = "multi_thread")]
async fn normal_query_with_defer_accept_header() {
    let request = supergraph::Request::fake_builder()
        .query(r#"{ me { reviews { author { reviews { author { name } } } } } }"#)
        .header(ACCEPT, "multipart/mixed;deferSpec=20220824")
        .build()
        .expect("expecting valid request");
    let (mut response, _registry) = {
        let (router, counting_registry) = setup_router_and_registry(serde_json::json!({})).await;
        (
            router
                .oneshot(request.try_into().unwrap())
                .await
                .unwrap()
                .into_graphql_response_stream()
                .await,
            counting_registry,
        )
    };
    insta::assert_json_snapshot!(response.next().await.unwrap().unwrap());
    assert!(response.next().await.is_none());
}

#[tokio::test(flavor = "multi_thread")]
async fn defer_path_with_disabled_config() {
    let config = serde_json::json!({
        "supergraph": {
            "defer_support": false,
        },
        "include_subgraph_errors": {
            "all": true
        }
    });
    let request = supergraph::Request::fake_builder()
        .query(
            r#"{
            me {
                id
                ...@defer(label: "name") {
                    name
                }
            }
        }"#,
        )
        .header(ACCEPT, "multipart/mixed;deferSpec=20220824")
        .build()
        .expect("expecting failure due to disabled config defer support");

    let (router, _) = setup_router_and_registry(config).await;

    let mut stream = router
        .oneshot(request.try_into().unwrap())
        .await
        .unwrap()
        .into_graphql_response_stream()
        .await;

    insta::assert_json_snapshot!(stream.next().await.unwrap().unwrap());
    assert!(stream.next().await.is_none());
}

#[tokio::test(flavor = "multi_thread")]
async fn defer_path() {
    let config = serde_json::json!({
        "include_subgraph_errors": {
            "all": true
        }
    });
    let request = supergraph::Request::fake_builder()
        .query(
            r#"{
            me {
                id
                ...@defer(label: "name") {
                    name
                }
            }
        }"#,
        )
        .header(ACCEPT, "multipart/mixed;deferSpec=20220824")
        .build()
        .expect("expecting valid request");

    let (router, _) = setup_router_and_registry(config).await;

    let mut stream = router
        .oneshot(request.try_into().unwrap())
        .await
        .unwrap()
        .into_graphql_response_stream()
        .await;

    insta::assert_json_snapshot!(stream.next().await.unwrap().unwrap());
    insta::assert_json_snapshot!(stream.next().await.unwrap().unwrap());
    assert!(stream.next().await.is_none());
}

#[tokio::test(flavor = "multi_thread")]
async fn defer_path_in_array() {
    let config = serde_json::json!({
        "include_subgraph_errors": {
            "all": true
        }
    });
    let request = supergraph::Request::fake_builder()
        .query(
            r#"{
                me {
                    reviews {
                        id
                        author {
                            id
                            ... @defer(label: "author name") {
                            name
                            }
                        }
                    }
                }
            }"#,
        )
        .header(ACCEPT, "multipart/mixed;deferSpec=20220824")
        .build()
        .expect("expecting valid request");

    let (router, _) = setup_router_and_registry(config).await;

    let mut stream = router
        .oneshot(request.try_into().unwrap())
        .await
        .unwrap()
        .into_graphql_response_stream()
        .await;

    insta::assert_json_snapshot!(stream.next().await.unwrap().unwrap());
    insta::assert_json_snapshot!(stream.next().await.unwrap().unwrap());
    assert!(stream.next().await.is_none());
}

#[tokio::test(flavor = "multi_thread")]
async fn defer_query_without_accept() {
    let config = serde_json::json!({
        "include_subgraph_errors": {
            "all": true
        }
    });
    let request = supergraph::Request::fake_builder()
        .query(
            r#"{
                me {
                    reviews {
                        id
                        author {
                            id
                            ... @defer(label: "author name") {
                            name
                            }
                        }
                    }
                }
            }"#,
        )
        .header(ACCEPT, APPLICATION_JSON.essence_str())
        .build()
        .expect("expecting valid request");

    let (router, _) = setup_router_and_registry(config).await;

    let mut stream = router.oneshot(request.try_into().unwrap()).await.unwrap();
    let first = stream.next_response().await.unwrap().unwrap();
    insta::assert_snapshot!(std::str::from_utf8(first.to_vec().as_slice()).unwrap());
}

#[tokio::test(flavor = "multi_thread")]
async fn defer_empty_primary_response() {
    let config = serde_json::json!({
        "include_subgraph_errors": {
            "all": true
        }
    });
    let request = supergraph::Request::fake_builder()
        .query(
            r#"{
            me {
                ...@defer(label: "name") {
                    name
                }
            }
        }"#,
        )
        .header(ACCEPT, "multipart/mixed;deferSpec=20220824")
        .build()
        .expect("expecting valid request");

    let (router, _) = setup_router_and_registry(config).await;

    let mut stream = router
        .oneshot(request.try_into().unwrap())
        .await
        .unwrap()
        .into_graphql_response_stream()
        .await;

    insta::assert_json_snapshot!(stream.next().await.unwrap().unwrap());
    insta::assert_json_snapshot!(stream.next().await.unwrap().unwrap());
    assert!(stream.next().await.is_none());
}

#[tokio::test(flavor = "multi_thread")]
async fn defer_default_variable() {
    let config = serde_json::json!({
        "include_subgraph_errors": {
            "all": true
        }
    });

    let query = r#"query X($if: Boolean! = true){
        me {
            id
            ...@defer(label: "name", if: $if) {
                name
            }
        }
    }"#;

    let request = supergraph::Request::fake_builder()
        .query(query)
        .header(ACCEPT, "multipart/mixed;deferSpec=20220824")
        .build()
        .expect("expecting valid request");

    let (router, _) = setup_router_and_registry(config.clone()).await;

    let mut stream = router
        .oneshot(request.try_into().unwrap())
        .await
        .unwrap()
        .into_graphql_response_stream()
        .await;

    insta::assert_json_snapshot!(stream.next().await.unwrap().unwrap());
    insta::assert_json_snapshot!(stream.next().await.unwrap().unwrap());
    assert!(stream.next().await.is_none());

    let request = supergraph::Request::fake_builder()
        .query(query)
        .variable("if", false)
        .header(ACCEPT, "multipart/mixed;deferSpec=20220824")
        .build()
        .expect("expecting valid request");

    let (router, _) = setup_router_and_registry(config).await;

    let mut stream = router
        .oneshot(request.try_into().unwrap())
        .await
        .unwrap()
        .into_graphql_response_stream()
        .await;

    insta::assert_json_snapshot!(stream.next().await.unwrap().unwrap());
    assert!(stream.next().await.is_none());
}

#[tokio::test(flavor = "multi_thread")]
async fn include_if_works() {
    let config = serde_json::json!({
        "supergraph": {
            "introspection": true
        },
    });

    let query = "query { ... Test @include(if: false) } fragment Test on Query { __typename }";

    let request = supergraph::Request::fake_builder()
        .query(query)
        .build()
        .expect("expecting valid request");

    let (router, _) = setup_router_and_registry(config).await;

    let mut stream = router
        .oneshot(request.try_into().unwrap())
        .await
        .unwrap()
        .into_graphql_response_stream()
        .await;

    insta::assert_json_snapshot!(stream.next().await.unwrap().unwrap());
}

#[tokio::test(flavor = "multi_thread")]
async fn query_operation_id() {
    let config = serde_json::json!({
        "supergraph": {
            "introspection": true
        },
    });

    let expected_apollo_operation_id = "d1554552698157b05c2a462827fb4367a4548ee5";

    let request: router::Request = supergraph::Request::fake_builder()
        .query(
            r#"query IgnitionMeQuery {
            me {
              id
            }
          }"#,
        )
        .method(Method::POST)
        .build()
        .expect("expecting valid request")
        .try_into()
        .unwrap();

    let (router, _) = setup_router_and_registry(config).await;

    let response = http_query_with_router(router.clone(), request).await;
    assert_eq!(
        expected_apollo_operation_id,
        response
            .context
            .get::<_, String>("apollo::supergraph::operation_id")
            .unwrap()
            .unwrap()
            .as_str()
    );

    // let's do it again to make sure a cached query plan still yields a stats report key hash
    let request: router::Request = supergraph::Request::fake_builder()
        .query(
            r#"query IgnitionMeQuery {
                me {
                    id
                }
            }"#,
        )
        .method(Method::POST)
        .build()
        .expect("expecting valid request")
        .try_into()
        .unwrap();

    let response = http_query_with_router(router.clone(), request).await;
    assert_eq!(
        expected_apollo_operation_id,
        response
            .context
            .get::<_, String>("apollo::supergraph::operation_id")
            .unwrap()
            .unwrap()
            .as_str()
    );

    // let's test failures now
    let parse_failure: router::Request = supergraph::Request::fake_builder()
        .query(r#"that's not even a query!"#)
        .method(Method::POST)
        .build()
        .expect("expecting valid request")
        .try_into()
        .unwrap();

    let response = http_query_with_router(router.clone(), parse_failure).await;
    assert!(
        // "## GraphQLParseFailure\n"
        response
            .context
            .get::<_, String>("apollo::supergraph::operation_id")
            .unwrap()
            .is_none()
    );

    let unknown_operation_name: router::Request = supergraph::Request::fake_builder()
        .query(
            r#"query Me {
                me {
                    id
                }
            }"#,
        )
        .operation_name("NotMe")
        .method(Method::POST)
        .build()
        .expect("expecting valid request")
        .try_into()
        .unwrap();

    let response = http_query_with_router(router.clone(), unknown_operation_name).await;
    // "## GraphQLUnknownOperationName\n"
    assert!(
        response
            .context
            .get::<_, String>("apollo::supergraph::operation_id")
            .unwrap()
            .is_none()
    );

    let validation_error: router::Request = supergraph::Request::fake_builder()
        .query(
            r#"query Me {
            me {
                thisfielddoesntexist
            }
        }"#,
        )
        .operation_name("NotMe")
        .method(Method::POST)
        .build()
        .expect("expecting valid request")
        .try_into()
        .unwrap();

    let response = http_query_with_router(router, validation_error).await;
    // "## GraphQLValidationFailure\n"
    assert!(
        response
            .context
            .get::<_, String>("apollo::supergraph::operation_id")
            .unwrap()
            .is_none()
    );
}

async fn http_query_rust(
    request: supergraph::Request,
) -> (router::Response, CountingServiceRegistry) {
    http_query_rust_with_config(request, serde_json::json!({})).await
}

async fn query_rust(
    request: supergraph::Request,
) -> (apollo_router::graphql::Response, CountingServiceRegistry) {
    query_rust_with_config(
        request,
        serde_json::json!({
            "telemetry":{
              "apollo": {
                    "field_level_instrumentation_sampler": "always_off"
                }
            }
        }),
    )
    .await
}

async fn http_query_rust_with_config(
    request: supergraph::Request,
    config: serde_json::Value,
) -> (router::Response, CountingServiceRegistry) {
    let (router, counting_registry) = setup_router_and_registry(config).await;
    (
        http_query_with_router(router, request.try_into().unwrap()).await,
        counting_registry,
    )
}

async fn query_rust_with_config(
    request: supergraph::Request,
    config: serde_json::Value,
) -> (apollo_router::graphql::Response, CountingServiceRegistry) {
    let (router, counting_registry) = setup_router_and_registry(config).await;
    (
        query_with_router(router, request.try_into().unwrap()).await,
        counting_registry,
    )
}

async fn fallible_setup_router_and_registry(
    config: serde_json::Value,
) -> Result<(router::BoxCloneService, CountingServiceRegistry), BoxError> {
    let counting_registry = CountingServiceRegistry::new();
    let router = apollo_router::TestHarness::builder()
        .with_subgraph_network_requests()
        .configuration_json(config)
        .map_err(|e| Box::new(e) as BoxError)?
        .schema(include_str!("fixtures/supergraph.graphql"))
        .extra_plugin(counting_registry.clone())
        .build_router()
        .await?;
    Ok((router, counting_registry))
}

async fn setup_router_and_registry_with_config(
    config: Configuration,
) -> Result<(router::BoxCloneService, CountingServiceRegistry), BoxError> {
    let counting_registry = CountingServiceRegistry::new();
    let router = apollo_router::TestHarness::builder()
        .with_subgraph_network_requests()
        .configuration(Arc::new(config))
        .schema(include_str!("fixtures/supergraph.graphql"))
        .extra_plugin(counting_registry.clone())
        .build_router()
        .await?;
    Ok((router, counting_registry))
}

async fn setup_router_and_registry(
    config: serde_json::Value,
) -> (router::BoxCloneService, CountingServiceRegistry) {
    fallible_setup_router_and_registry(config).await.unwrap()
}

async fn query_with_router(
    router: router::BoxCloneService,
    request: router::Request,
) -> graphql::Response {
    serde_json::from_slice(
        router
            .oneshot(request)
            .await
            .unwrap()
            .next_response()
            .await
            .unwrap()
            .unwrap()
            .to_vec()
            .as_slice(),
    )
    .unwrap()
}

async fn http_query_with_router(
    router: router::BoxCloneService,
    request: router::Request,
) -> router::Response {
    router.oneshot(request).await.unwrap()
}

#[derive(Debug, Clone)]
struct CountingServiceRegistry {
    counts: Arc<Mutex<HashMap<String, usize>>>,
}

impl CountingServiceRegistry {
    fn new() -> CountingServiceRegistry {
        CountingServiceRegistry {
            counts: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    fn increment(&self, service: &str) {
        let mut counts = self.counts.lock();
        match counts.entry(service.to_owned()) {
            Entry::Occupied(mut e) => {
                *e.get_mut() += 1;
            }
            Entry::Vacant(e) => {
                e.insert(1);
            }
        };
    }

    fn totals(&self) -> HashMap<String, usize> {
        self.counts.lock().clone()
    }
}

#[async_trait::async_trait]
impl Plugin for CountingServiceRegistry {
    type Config = ();

    async fn new(_: PluginInit<Self::Config>) -> Result<Self, BoxError> {
        unreachable!()
    }

    fn subgraph_service(
        &self,
        subgraph_name: &str,
        service: subgraph::BoxService,
    ) -> subgraph::BoxService {
        let name = subgraph_name.to_owned();
        let counters = self.clone();
        service
            .map_request(move |request| {
                counters.increment(&name);
                request
            })
            .boxed()
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn all_stock_router_example_yamls_are_valid() {
    let example_dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../examples");
    let example_directory_entries: Vec<DirEntry> = WalkDir::new(example_dir)
        .into_iter()
        // Filter out `../examples/custom-global-allocator/target/` with its separate workspace
        .filter_entry(|entry| entry.path().file_name() != Some(OsStr::new("target")))
        .map(|entry| {
            entry.unwrap_or_else(|e| panic!("invalid directory entry in {example_dir}: {e}"))
        })
        .collect();
    assert!(
        !example_directory_entries.is_empty(),
        "asserting that example_directory_entries is not empty"
    );
    for example_directory_entry in example_directory_entries {
        let entry_path = example_directory_entry.path();
        let display_path = entry_path.display().to_string();
        let entry_parent = entry_path
            .parent()
            .unwrap_or_else(|| panic!("could not find parent of {display_path}"));

        // skip projects with a `.skipconfigvalidation` file or a `Cargo.toml`
        // we only want to test stock router binary examples, nothing custom
        if !entry_parent.join(".skipconfigvalidation").exists()
            && !entry_parent.join("Cargo.toml").exists()
        {
            // if we aren't on a unix machine and a `.unixonly` sibling file exists
            // don't validate the YAML
            if !cfg!(target_family = "unix") && entry_parent.join(".unixonly").exists() {
                break;
            }
            if let Some(name) = example_directory_entry.file_name().to_str()
                && (name.ends_with("yaml") || name.ends_with("yml"))
            {
                let raw_yaml = std::fs::read_to_string(entry_path)
                    .unwrap_or_else(|e| panic!("unable to read {display_path}: {e}"));
                {
                    let mut configuration: Configuration = serde_yaml::from_str(&raw_yaml)
                        .unwrap_or_else(|e| panic!("unable to parse YAML {display_path}: {e}"));
                    let (_mock_guard, configuration) = if configuration.persisted_queries.enabled {
                        let (_mock_guard, uplink_config) = mock_empty_pq_uplink().await;
                        configuration.uplink = Some(uplink_config);
                        (Some(_mock_guard), configuration)
                    } else {
                        (None, configuration)
                    };
                    setup_router_and_registry_with_config(configuration)
                        .await
                        .unwrap_or_else(|e| {
                            panic!("unable to start up router for {display_path}: {e}");
                        });
                }
            }
        }
    }
}

#[tokio::test]
async fn test_starstuff_supergraph_is_valid() {
    let schema = include_str!("../../examples/graphql/supergraph.graphql");
    apollo_router::TestHarness::builder()
        .schema(schema)
        .build_router()
        .await
        .expect(
            r#"Couldn't parse the supergraph example.
This file is being used in the router documentation, as a quickstart example.
Make sure it is accessible, and the configuration is working with the router."#,
        );

    insta::assert_snapshot!(include_str!("../../examples/graphql/supergraph.graphql"));
}

// This test must use the multi_thread tokio executor or the opentelemetry hang bug will
// be encountered. (See https://github.com/open-telemetry/opentelemetry-rust/issues/536)
#[tokio::test(flavor = "multi_thread")]
async fn test_telemetry_doesnt_hang_with_invalid_schema() {
    create_test_service_factory_from_yaml(
        include_str!("../src/testdata/invalid_supergraph.graphql"),
        r#"
    telemetry:
      exporters:
        tracing:
          common:
            service_name: router
          otlp:
            enabled: true
            endpoint: default
"#,
    )
    .await;
}

// Ensure that, on unix, the router won't start with wrong file permissions
#[cfg(unix)]
#[test]
fn it_will_not_start_with_loose_file_permissions() {
    use std::os::fd::AsRawFd;
    use std::process::Command;

    use crate::integration::IntegrationTest;

    let mut router = Command::new(IntegrationTest::router_location());

    let tester = tempfile::NamedTempFile::new().expect("it created a temporary test file");
    let fd = tester.as_file().as_raw_fd();
    let path = tester.path().to_str().expect("got the tempfile path");

    // Modify our temporary file permissions so that they are definitely too loose.
    unsafe {
        libc::fchmod(fd, 0o777);
    }

    let output = router
        .args(["--apollo-key-path", path])
        .output()
        .expect("router could not start");

    // Assert that our router executed unsuccessfully
    assert!(!output.status.success());
    // It may have been unsuccessful for a variety of reasons, is it the right reason?
    assert_eq!(
        std::str::from_utf8(&output.stderr).expect("output is a string"),
        "Apollo key file permissions (0o777) are too permissive\n"
    )
}

fn normalize_errors(errors: Vec<Error>) -> Vec<Error> {
    errors
        .into_iter()
        .map(|e| {
            Error::builder()
                // Overwrite error ID to avoid comparing random Uuids
                .apollo_id(Uuid::nil())
                .message(e.message)
                .locations(e.locations)
                .and_path(e.path)
                .extensions(e.extensions)
                .build()
        })
        .collect()
}
