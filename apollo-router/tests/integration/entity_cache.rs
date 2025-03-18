use std::collections::HashMap;
use std::sync::Arc;

use apollo_compiler::Schema;
use apollo_router::_private::MockStore;
use apollo_router::_private::RedisCacheStorage;
use apollo_router::_private::entity_cache::CONTEXT_CACHE_KEYS;
use apollo_router::_private::entity_cache::CacheKeyContext;
use apollo_router::_private::entity_cache::CacheKeysContext;
use apollo_router::_private::entity_cache::EntityCache;
use apollo_router::_private::entity_cache::Subgraph;
use apollo_router::Context;
use apollo_router::MockedSubgraphs;
use apollo_router::TestHarness;
use apollo_router::plugin::test::MockSubgraph;
use apollo_router::plugin::test::MockSubgraphService;
use apollo_router::services::subgraph;
use apollo_router::services::supergraph;
use http::HeaderValue;
use http::header::CACHE_CONTROL;
use tower::Service;
use tower::ServiceExt;

const SCHEMA: &str = include_str!("../../src/testdata/orga_supergraph.graphql");
const SCHEMA_REQUIRES: &str = include_str!("../../src/testdata/supergraph.graphql");

#[tokio::test]
async fn insert() {
    let valid_schema = Arc::new(Schema::parse_and_validate(SCHEMA, "test.graphql").unwrap());
    let query = "query { currentUser { activeOrganization { id creatorUser { __typename id } } } }";

    let redis_cache = RedisCacheStorage::from_mocks(Arc::new(MockStore::default()))
        .await
        .unwrap();
    let map = [
        (
            "user".to_string(),
            Subgraph {
                redis: None,
                private_id: Some("sub".to_string()),
                enabled: true,
                ttl: None,
                ..Default::default()
            },
        ),
        (
            "orga".to_string(),
            Subgraph {
                redis: None,
                private_id: Some("sub".to_string()),
                enabled: true,
                ttl: None,
                ..Default::default()
            },
        ),
    ]
    .into_iter()
    .collect();
    let entity_cache = EntityCache::with_mocks(redis_cache.clone(), map, valid_schema.clone())
        .await
        .unwrap();

    let mocks = serde_json::json!({
        "user": {
            "headers": {CACHE_CONTROL.as_str(): "public"},
            "query": {
                "currentUser": {
                    "activeOrganization": {
                        "__typename": "Organization",
                        "id": "1",
                    }
                }
            }
        },
        "orga": {
            "headers": {CACHE_CONTROL.as_str(): "public"},
            "entities": [
                {
                    "__typename": "Organization",
                    "id": "1",
                    "creatorUser": {
                        "__typename": "User",
                        "id": 2
                    }
                }
            ]
        },
    });
    let service = TestHarness::builder()
        .configuration_json(serde_json::json!({
            "include_subgraph_errors": { "all": true },
            "subgraph_mocks": mocks,
        }))
        .unwrap()
        .schema(SCHEMA)
        .extra_plugin(entity_cache)
        .build_supergraph()
        .await
        .unwrap();

    let request = supergraph::Request::fake_builder()
        .query(query)
        .context(Context::new())
        .build()
        .unwrap();
    let mut response = service.oneshot(request).await.unwrap();
    let cache_keys: CacheKeysContext = response.context.get(CONTEXT_CACHE_KEYS).unwrap().unwrap();
    let mut cache_keys: Vec<CacheKeyContext> = cache_keys.into_values().flatten().collect();
    cache_keys.sort();
    insta::assert_json_snapshot!(cache_keys);

    insta::assert_debug_snapshot!(response.response.headers().get(CACHE_CONTROL));
    let response = response.next_response().await.unwrap();

    insta::assert_json_snapshot!(response);

    // Now testing without any mock subgraphs, all the data should come from the cache
    let entity_cache =
        EntityCache::with_mocks(redis_cache.clone(), HashMap::new(), valid_schema.clone())
            .await
            .unwrap();

    let service = TestHarness::builder()
        .configuration_json(serde_json::json!({"include_subgraph_errors": { "all": true } }))
        .unwrap()
        .schema(SCHEMA)
        .extra_plugin(entity_cache)
        .build_supergraph()
        .await
        .unwrap();

    let request = supergraph::Request::fake_builder()
        .query(query)
        .context(Context::new())
        .build()
        .unwrap();
    let mut response = service.oneshot(request).await.unwrap();

    insta::assert_debug_snapshot!(response.response.headers().get(CACHE_CONTROL));
    let cache_keys: CacheKeysContext = response.context.get(CONTEXT_CACHE_KEYS).unwrap().unwrap();
    let mut cache_keys: Vec<CacheKeyContext> = cache_keys.into_values().flatten().collect();
    cache_keys.sort();
    insta::assert_json_snapshot!(cache_keys);

    let response = response.next_response().await.unwrap();

    insta::assert_json_snapshot!(response);
}

#[tokio::test]
async fn insert_with_requires() {
    let valid_schema =
        Arc::new(Schema::parse_and_validate(SCHEMA_REQUIRES, "test.graphql").unwrap());
    let query = "query { topProducts { name shippingEstimate price } }";

    let subgraphs = MockedSubgraphs([
        ("products", MockSubgraph::builder().with_json(
                serde_json::json!{{"query":"{ topProducts { __typename upc name price weight } }"}},
                serde_json::json!{{"data": {"topProducts": [{
                    "__typename": "Product",
                    "upc": "1",
                    "name": "Test",
                    "price": 150,
                    "weight": 5
                }]}}}
        ).with_header(CACHE_CONTROL, HeaderValue::from_static("public")).build()),
        ("inventory", MockSubgraph::builder().with_json(
            serde_json::json!{{
                "query": "query($representations: [_Any!]!) { _entities(representations: $representations) { ... on Product { shippingEstimate } } }",
                "variables": {
                    "representations": [
                        {
                            "price": 150,
                            "weight": 5,
                            "upc": "1",
                            "__typename": "Product"
                        }
                    ]
            }}},
            serde_json::json!{{"data": {
                "_entities": [{
                    "shippingEstimate": 15
                }]
            }}}
        ).with_header(CACHE_CONTROL, HeaderValue::from_static("public")).build())
    ].into_iter().collect());

    let redis_cache = RedisCacheStorage::from_mocks(Arc::new(MockStore::default()))
        .await
        .unwrap();
    let map = [
        (
            "products".to_string(),
            Subgraph {
                redis: None,
                private_id: Some("sub".to_string()),
                enabled: true,
                ttl: None,
                ..Default::default()
            },
        ),
        (
            "inventory".to_string(),
            Subgraph {
                redis: None,
                private_id: Some("sub".to_string()),
                enabled: true,
                ttl: None,
                ..Default::default()
            },
        ),
    ]
    .into_iter()
    .collect();
    let entity_cache = EntityCache::with_mocks(redis_cache.clone(), map, valid_schema.clone())
        .await
        .unwrap();

    let service = TestHarness::builder()
        .configuration_json(serde_json::json!({"include_subgraph_errors": { "all": true } }))
        .unwrap()
        .schema(SCHEMA_REQUIRES)
        .extra_plugin(entity_cache)
        .extra_plugin(subgraphs)
        .build_supergraph()
        .await
        .unwrap();

    let request = supergraph::Request::fake_builder()
        .query(query)
        .context(Context::new())
        .build()
        .unwrap();
    let mut response = service.oneshot(request).await.unwrap();
    let cache_keys: CacheKeysContext = response.context.get(CONTEXT_CACHE_KEYS).unwrap().unwrap();
    let mut cache_keys: Vec<CacheKeyContext> = cache_keys.into_values().flatten().collect();
    cache_keys.sort();
    insta::assert_json_snapshot!(cache_keys);

    insta::assert_debug_snapshot!(response.response.headers().get(CACHE_CONTROL));
    let response = response.next_response().await.unwrap();

    insta::assert_json_snapshot!(response);

    // Now testing without any mock subgraphs, all the data should come from the cache
    let entity_cache =
        EntityCache::with_mocks(redis_cache.clone(), HashMap::new(), valid_schema.clone())
            .await
            .unwrap();

    let service = TestHarness::builder()
        .configuration_json(serde_json::json!({"include_subgraph_errors": { "all": true } }))
        .unwrap()
        .schema(SCHEMA_REQUIRES)
        .extra_plugin(entity_cache)
        .build_supergraph()
        .await
        .unwrap();

    let request = supergraph::Request::fake_builder()
        .query(query)
        .context(Context::new())
        .build()
        .unwrap();
    let mut response = service.oneshot(request).await.unwrap();

    insta::assert_debug_snapshot!(response.response.headers().get(CACHE_CONTROL));
    let cache_keys: CacheKeysContext = response.context.get(CONTEXT_CACHE_KEYS).unwrap().unwrap();
    let mut cache_keys: Vec<CacheKeyContext> = cache_keys.into_values().flatten().collect();
    cache_keys.sort();
    insta::assert_json_snapshot!(cache_keys);

    let response = response.next_response().await.unwrap();

    insta::assert_json_snapshot!(response);
}

#[tokio::test]
async fn no_cache_control() {
    let valid_schema = Arc::new(Schema::parse_and_validate(SCHEMA, "test.graphql").unwrap());
    let query = "query { currentUser { activeOrganization { id creatorUser { __typename id } } } }";

    let subgraphs = MockedSubgraphs([
        ("user", MockSubgraph::builder().with_json(
                serde_json::json!{{"query":"{currentUser{activeOrganization{__typename id}}}"}},
                serde_json::json!{{"data": {"currentUser": { "activeOrganization": {
                    "__typename": "Organization",
                    "id": "1"
                } }}}}
        ).build()),
        ("orga", MockSubgraph::builder().with_json(
            serde_json::json!{{
                "query": "query($representations:[_Any!]!){_entities(representations:$representations){... on Organization{creatorUser{__typename id}}}}",
            "variables": {
                "representations": [
                    {
                        "id": "1",
                        "__typename": "Organization",
                    }
                ]
            }}},
            serde_json::json!{{"data": {
                "_entities": [{
                    "creatorUser": {
                        "__typename": "User",
                        "id": 2
                    }
                }]
            }}}
        ).build())
    ].into_iter().collect());

    let redis_cache = RedisCacheStorage::from_mocks(Arc::new(MockStore::default()))
        .await
        .unwrap();
    let entity_cache =
        EntityCache::with_mocks(redis_cache.clone(), HashMap::new(), valid_schema.clone())
            .await
            .unwrap();

    let service = TestHarness::builder()
        .configuration_json(serde_json::json!({"include_subgraph_errors": { "all": true } }))
        .unwrap()
        .schema(SCHEMA)
        .extra_plugin(entity_cache)
        .extra_plugin(subgraphs)
        .build_supergraph()
        .await
        .unwrap();

    let request = supergraph::Request::fake_builder()
        .query(query)
        .context(Context::new())
        .build()
        .unwrap();
    let mut response = service.oneshot(request).await.unwrap();

    insta::assert_debug_snapshot!(response.response.headers().get(CACHE_CONTROL));
    let response = response.next_response().await.unwrap();

    insta::assert_json_snapshot!(response);

    // Now testing without any mock subgraphs, all the data should come from the cache
    let entity_cache =
        EntityCache::with_mocks(redis_cache.clone(), HashMap::new(), valid_schema.clone())
            .await
            .unwrap();

    let service = TestHarness::builder()
        .configuration_json(serde_json::json!({"include_subgraph_errors": { "all": true } }))
        .unwrap()
        .schema(SCHEMA)
        .extra_plugin(entity_cache)
        .build_supergraph()
        .await
        .unwrap();

    let request = supergraph::Request::fake_builder()
        .query(query)
        .context(Context::new())
        .build()
        .unwrap();
    let mut response = service.oneshot(request).await.unwrap();

    insta::assert_debug_snapshot!(response.response.headers().get(CACHE_CONTROL));
    let response = response.next_response().await.unwrap();

    insta::assert_json_snapshot!(response);
}

#[tokio::test]
async fn private() {
    let query = "query { currentUser { activeOrganization { id creatorUser { __typename id } } } }";
    let valid_schema = Arc::new(Schema::parse_and_validate(SCHEMA, "test.graphql").unwrap());

    let subgraphs = MockedSubgraphs([
        ("user", MockSubgraph::builder().with_json(
                serde_json::json!{{"query":"{currentUser{activeOrganization{__typename id}}}"}},
                serde_json::json!{{"data": {"currentUser": { "activeOrganization": {
                    "__typename": "Organization",
                    "id": "1"
                } }}}}
            ).with_header(CACHE_CONTROL, HeaderValue::from_static("private"))
            .build()),
        ("orga", MockSubgraph::builder().with_json(
            serde_json::json!{{
                "query": "query($representations:[_Any!]!){_entities(representations:$representations){... on Organization{creatorUser{__typename id}}}}",
            "variables": {
                "representations": [
                    {
                        "id": "1",
                        "__typename": "Organization",
                    }
                ]
            }}},
            serde_json::json!{{"data": {
                "_entities": [{
                    "creatorUser": {
                        "__typename": "User",
                        "id": 2
                    }
                }]
            }}}
        ).with_header(CACHE_CONTROL, HeaderValue::from_static("private")).build())
    ].into_iter().collect());

    let redis_cache = RedisCacheStorage::from_mocks(Arc::new(MockStore::default()))
        .await
        .unwrap();
    let map = [
        (
            "user".to_string(),
            Subgraph {
                redis: None,
                private_id: Some("sub".to_string()),
                enabled: true,
                ttl: None,
                ..Default::default()
            },
        ),
        (
            "orga".to_string(),
            Subgraph {
                redis: None,
                private_id: Some("sub".to_string()),
                enabled: true,
                ttl: None,
                ..Default::default()
            },
        ),
    ]
    .into_iter()
    .collect();
    let entity_cache = EntityCache::with_mocks(redis_cache.clone(), map, valid_schema.clone())
        .await
        .unwrap();

    let mut service = TestHarness::builder()
        .configuration_json(serde_json::json!({"include_subgraph_errors": { "all": true } }))
        .unwrap()
        .schema(SCHEMA)
        .extra_plugin(entity_cache.clone())
        .extra_plugin(subgraphs)
        .build_supergraph()
        .await
        .unwrap();

    let context = Context::new();
    context.insert_json_value("sub", "1234".into());

    let request = supergraph::Request::fake_builder()
        .query(query)
        .context(context)
        .build()
        .unwrap();
    let mut response = service.ready().await.unwrap().call(request).await.unwrap();
    let cache_keys: CacheKeysContext = response.context.get(CONTEXT_CACHE_KEYS).unwrap().unwrap();
    let mut cache_keys: Vec<CacheKeyContext> = cache_keys.into_values().flatten().collect();
    cache_keys.sort();
    insta::assert_json_snapshot!(cache_keys);

    let response = response.next_response().await.unwrap();
    insta::assert_json_snapshot!(response);

    println!("\nNOW WITHOUT SUBGRAPHS\n");
    // Now testing without any mock subgraphs, all the data should come from the cache
    let mut service = TestHarness::builder()
        .configuration_json(serde_json::json!({"include_subgraph_errors": { "all": true } }))
        .unwrap()
        .schema(SCHEMA)
        .extra_plugin(entity_cache)
        .build_supergraph()
        .await
        .unwrap();

    let context = Context::new();
    context.insert_json_value("sub", "1234".into());

    let request = supergraph::Request::fake_builder()
        .query(query)
        .context(context)
        .build()
        .unwrap();
    let mut response = service.ready().await.unwrap().call(request).await.unwrap();
    let cache_keys: CacheKeysContext = response.context.get(CONTEXT_CACHE_KEYS).unwrap().unwrap();
    let mut cache_keys: Vec<CacheKeyContext> = cache_keys.into_values().flatten().collect();
    cache_keys.sort();
    insta::assert_json_snapshot!(cache_keys);

    let response = response.next_response().await.unwrap();

    insta::assert_json_snapshot!(response);

    println!("\nNOW WITH DIFFERENT SUB\n");

    let context = Context::new();
    context.insert_json_value("sub", "5678".into());
    let request = supergraph::Request::fake_builder()
        .query(query)
        .context(context)
        .build()
        .unwrap();
    let mut response = service.ready().await.unwrap().call(request).await.unwrap();
    assert!(
        response
            .context
            .get::<_, CacheKeysContext>(CONTEXT_CACHE_KEYS)
            .ok()
            .flatten()
            .is_none()
    );
    insta::assert_json_snapshot!(cache_keys);

    let response = response.next_response().await.unwrap();
    insta::assert_json_snapshot!(response);
}

#[tokio::test]
async fn no_data() {
    let query = "query { currentUser { allOrganizations { id name } } }";
    let valid_schema = Arc::new(Schema::parse_and_validate(SCHEMA, "test.graphql").unwrap());

    let subgraphs = MockedSubgraphs([
        ("user", MockSubgraph::builder().with_json(
                serde_json::json!{{"query":"{currentUser{allOrganizations{__typename id}}}"}},
                serde_json::json!{{"data": {"currentUser": { "allOrganizations": [
                    {
                        "__typename": "Organization",
                        "id": "1"
                    },
                    {
                        "__typename": "Organization",
                        "id": "3"
                    }
                ] }}}}
        ).with_header(CACHE_CONTROL, HeaderValue::from_static("no-store")).build()),
        ("orga", MockSubgraph::builder().with_json(
            serde_json::json!{{
                "query": "query($representations:[_Any!]!){_entities(representations:$representations){...on Organization{name}}}",
            "variables": {
                "representations": [
                    {
                        "id": "1",
                        "__typename": "Organization",
                    },
                    {
                        "id": "3",
                        "__typename": "Organization",
                    }
                ]
            }}},
            serde_json::json!{{
                "data": {
                    "_entities": [{
                    "name": "Organization 1",
                },
                {
                    "name": "Organization 3"
                }]
            }
            }}
        ).with_header(CACHE_CONTROL, HeaderValue::from_static("public, max-age=3600")).build())
    ].into_iter().collect());

    let redis_cache = RedisCacheStorage::from_mocks(Arc::new(MockStore::default()))
        .await
        .unwrap();
    let map = [
        (
            "user".to_string(),
            Subgraph {
                redis: None,
                private_id: Some("sub".to_string()),
                enabled: true,
                ttl: None,
                ..Default::default()
            },
        ),
        (
            "orga".to_string(),
            Subgraph {
                redis: None,
                private_id: Some("sub".to_string()),
                enabled: true,
                ttl: None,
                ..Default::default()
            },
        ),
    ]
    .into_iter()
    .collect();
    let entity_cache = EntityCache::with_mocks(redis_cache.clone(), map, valid_schema.clone())
        .await
        .unwrap();

    let service = TestHarness::builder()
        .configuration_json(serde_json::json!({"include_subgraph_errors": { "all": true } }))
        .unwrap()
        .schema(SCHEMA)
        .extra_plugin(entity_cache)
        .extra_plugin(subgraphs)
        .build_supergraph()
        .await
        .unwrap();

    let request = supergraph::Request::fake_builder()
        .query(query)
        .context(Context::new())
        .build()
        .unwrap();
    let mut response = service.oneshot(request).await.unwrap();

    let cache_keys: CacheKeysContext = response.context.get(CONTEXT_CACHE_KEYS).unwrap().unwrap();
    let mut cache_keys: Vec<CacheKeyContext> = cache_keys.into_values().flatten().collect();
    cache_keys.sort();
    insta::assert_json_snapshot!(cache_keys, {
        "[].cache_control" => insta::dynamic_redaction(|value, _path| {
            let cache_control = value.as_str().unwrap().to_string();
            assert!(cache_control.contains("max-age="));
            assert!(cache_control.contains("public"));
            "[REDACTED]"
        })
    });

    let response = response.next_response().await.unwrap();
    insta::assert_json_snapshot!(response);

    let entity_cache =
        EntityCache::with_mocks(redis_cache.clone(), HashMap::new(), valid_schema.clone())
            .await
            .unwrap();

    let subgraphs = MockedSubgraphs(
        [(
            "user",
            MockSubgraph::builder()
                .with_json(
                    serde_json::json! {{"query":"{currentUser{allOrganizations{__typename id}}}"}},
                    serde_json::json! {{"data": {"currentUser": { "allOrganizations": [
                        {
                            "__typename": "Organization",
                            "id": "1"
                        },
                        {
                            "__typename": "Organization",
                            "id": "2"
                        },
                        {
                            "__typename": "Organization",
                            "id": "3"
                        }
                    ] }}}},
                )
                .with_header(CACHE_CONTROL, HeaderValue::from_static("no-store"))
                .build(),
        )]
        .into_iter()
        .collect(),
    );

    let service = TestHarness::builder()
        .configuration_json(serde_json::json!({"include_subgraph_errors": { "all": true } }))
        .unwrap()
        .schema(SCHEMA)
        .extra_plugin(entity_cache)
        .subgraph_hook(|name, service| {
            if name == "orga" {
                let mut subgraph = MockSubgraphService::new();
                subgraph
                    .expect_call()
                    .times(1)
                    .returning(move |_req: subgraph::Request| Err("orga not found".into()));
                subgraph.boxed()
            } else {
                service
            }
        })
        .extra_plugin(subgraphs)
        .build_supergraph()
        .await
        .unwrap();

    let request = supergraph::Request::fake_builder()
        .query(query)
        .context(Context::new())
        .build()
        .unwrap();
    let mut response = service.oneshot(request).await.unwrap();

    let cache_keys: CacheKeysContext = response.context.get(CONTEXT_CACHE_KEYS).unwrap().unwrap();
    let mut cache_keys: Vec<CacheKeyContext> = cache_keys.into_values().flatten().collect();
    cache_keys.sort();
    insta::assert_json_snapshot!(cache_keys, {
        "[].cache_control" => insta::dynamic_redaction(|value, _path| {
            let cache_control = value.as_str().unwrap().to_string();
            assert!(cache_control.contains("max-age="));
            assert!(cache_control.contains("public"));
            "[REDACTED]"
        })
    });
    let response = response.next_response().await.unwrap();

    insta::assert_json_snapshot!(response);
}

#[tokio::test]
async fn missing_entities() {
    let query = "query { currentUser { allOrganizations { id name } } }";
    let valid_schema = Arc::new(Schema::parse_and_validate(SCHEMA, "test.graphql").unwrap());
    let subgraphs = MockedSubgraphs([
        ("user", MockSubgraph::builder().with_json(
                serde_json::json!{{"query":"{currentUser{allOrganizations{__typename id}}}"}},
                serde_json::json!{{"data": {"currentUser": { "allOrganizations": [
                    {
                        "__typename": "Organization",
                        "id": "1"
                    },
                    {
                        "__typename": "Organization",
                        "id": "2"
                    }
                ] }}}}
        ).with_header(CACHE_CONTROL, HeaderValue::from_static("no-store")).build()),
        ("orga", MockSubgraph::builder().with_json(
            serde_json::json!{{
                "query": "query($representations:[_Any!]!){_entities(representations:$representations){...on Organization{name}}}",
            "variables": {
                "representations": [
                    {
                        "id": "1",
                        "__typename": "Organization",
                    },
                    {
                        "id": "2",
                        "__typename": "Organization",
                    }
                ]
            }}},
            serde_json::json!{{
                "data": {
                    "_entities": [
                        {
                            "name": "Organization 1",
                        },
                        {
                            "name": "Organization 2"
                        }
                    ]
            }
            }}
        ).with_header(CACHE_CONTROL, HeaderValue::from_static("public, max-age=3600")).build())
    ].into_iter().collect());

    let redis_cache = RedisCacheStorage::from_mocks(Arc::new(MockStore::default()))
        .await
        .unwrap();
    let map = [
        (
            "user".to_string(),
            Subgraph {
                redis: None,
                private_id: Some("sub".to_string()),
                enabled: true,
                ttl: None,
                ..Default::default()
            },
        ),
        (
            "orga".to_string(),
            Subgraph {
                redis: None,
                private_id: Some("sub".to_string()),
                enabled: true,
                ttl: None,
                ..Default::default()
            },
        ),
    ]
    .into_iter()
    .collect();
    let entity_cache = EntityCache::with_mocks(redis_cache.clone(), map, valid_schema.clone())
        .await
        .unwrap();

    let service = TestHarness::builder()
        .configuration_json(serde_json::json!({"include_subgraph_errors": { "all": true } }))
        .unwrap()
        .schema(SCHEMA)
        .extra_plugin(entity_cache)
        .extra_plugin(subgraphs)
        .build_supergraph()
        .await
        .unwrap();

    let request = supergraph::Request::fake_builder()
        .query(query)
        .context(Context::new())
        .build()
        .unwrap();
    let mut response = service.oneshot(request).await.unwrap();
    let response = response.next_response().await.unwrap();
    insta::assert_json_snapshot!(response);

    let entity_cache =
        EntityCache::with_mocks(redis_cache.clone(), HashMap::new(), valid_schema.clone())
            .await
            .unwrap();

    let subgraphs = MockedSubgraphs([
            ("user", MockSubgraph::builder().with_json(
                    serde_json::json!{{"query":"{currentUser{allOrganizations{__typename id}}}"}},
                    serde_json::json!{{"data": {"currentUser": { "allOrganizations": [
                        {
                            "__typename": "Organization",
                            "id": "1"
                        },
                        {
                            "__typename": "Organization",
                            "id": "2"
                        },
                        {
                            "__typename": "Organization",
                            "id": "3"
                        }
                    ] }}}}
            ).with_header(CACHE_CONTROL, HeaderValue::from_static("no-store")).build()),
            ("orga", MockSubgraph::builder().with_json(
                serde_json::json!{{
                    "query": "query($representations:[_Any!]!){_entities(representations:$representations){...on Organization{name}}}",
                "variables": {
                    "representations": [
                        {
                            "id": "3",
                            "__typename": "Organization",
                        }
                    ]
                }}},
                serde_json::json!{{
                    "data": null,
                    "errors": [{
                        "message": "Organization not found",
                    }]
                }}
            ).with_header(CACHE_CONTROL, HeaderValue::from_static("public, max-age=3600")).build())
        ].into_iter().collect());

    let service = TestHarness::builder()
        .configuration_json(serde_json::json!({"include_subgraph_errors": { "all": true } }))
        .unwrap()
        .schema(SCHEMA)
        .extra_plugin(entity_cache)
        .extra_plugin(subgraphs)
        .build_supergraph()
        .await
        .unwrap();

    let request = supergraph::Request::fake_builder()
        .query(query)
        .context(Context::new())
        .build()
        .unwrap();
    let mut response = service.oneshot(request).await.unwrap();
    let response = response.next_response().await.unwrap();
    insta::assert_json_snapshot!(response);
}

/*FIXME: reactivate test if we manage to make fred return the response to SCAN in mocks
#[tokio::test(flavor = "multi_thread")]
async fn invalidate() {
    let query = "query { currentUser { activeOrganization { id creatorUser { __typename id } } } }";

    let subgraphs = MockedSubgraphs([
        ("user", MockSubgraph::builder().with_json(
                serde_json::json!{{"query":"{currentUser{activeOrganization{__typename id}}}"}},
                serde_json::json!{{"data": {"currentUser": { "activeOrganization": {
                    "__typename": "Organization",
                    "id": "1"
                } }}}}
        ).with_header(CACHE_CONTROL, HeaderValue::from_static("public")).build()),
        ("orga", MockSubgraph::builder().with_json(
            serde_json::json!{{
                "query": "query($representations:[_Any!]!){_entities(representations:$representations){...on Organization{creatorUser{__typename id}}}}",
            "variables": {
                "representations": [
                    {
                        "id": "1",
                        "__typename": "Organization",
                    }
                ]
            }}},
            serde_json::json!{{"data": {
                "_entities": [{
                    "creatorUser": {
                        "__typename": "User",
                        "id": 2
                    }
                }]
            }}}
        ).with_header(CACHE_CONTROL, HeaderValue::from_static("public")).build())
    ].into_iter().collect());

    let redis_cache = RedisCacheStorage::from_mocks(Arc::new(MockStore::default()))
        .await
        .unwrap();
    let entity_cache = EntityCache::with_mocks(redis_cache.clone(), HashMap::new())
        .await
        .unwrap();
    let mut invalidation = entity_cache.invalidation.clone();

    let service = TestHarness::builder()
        .configuration_json(serde_json::json!({"include_subgraph_errors": { "all": true } }))
        .unwrap()
        .schema(SCHEMA)
        .extra_plugin(entity_cache.clone())
        .extra_plugin(subgraphs)
        .build_supergraph()
        .await
        .unwrap();

    let request = supergraph::Request::fake_builder()
        .query(query)
        .context(Context::new())
        .build()
        .unwrap();
    let mut response = service.oneshot(request).await.unwrap();

    insta::assert_debug_snapshot!(response.response.headers().get(CACHE_CONTROL));
    let response = response.next_response().await.unwrap();

    insta::assert_json_snapshot!(response);

    // Now testing without any mock subgraphs, all the data should come from the cache
    let service = TestHarness::builder()
        .configuration_json(serde_json::json!({"include_subgraph_errors": { "all": true } }))
        .unwrap()
        .schema(SCHEMA)
        .extra_plugin(entity_cache.clone())
        .build_supergraph()
        .await
        .unwrap();

    let request = supergraph::Request::fake_builder()
        .query(query)
        .context(Context::new())
        .build()
        .unwrap();
    let mut response = service.clone().oneshot(request).await.unwrap();

    insta::assert_debug_snapshot!(response.response.headers().get(CACHE_CONTROL));
    let response = response.next_response().await.unwrap();

    insta::assert_json_snapshot!(response);

    // now we invalidate data
    invalidation
        .invalidate(vec![InvalidationRequest::Subgraph {
            subgraph: "orga".to_string(),
        }])
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(2000)).await;

    panic!();
    let service = TestHarness::builder()
        .configuration_json(serde_json::json!({"include_subgraph_errors": { "all": true } }))
        .unwrap()
        .schema(SCHEMA)
        .extra_plugin(entity_cache)
        .build_supergraph()
        .await
        .unwrap();

    let request = supergraph::Request::fake_builder()
        .query(query)
        .context(Context::new())
        .build()
        .unwrap();
    let mut response = service.clone().oneshot(request).await.unwrap();

    insta::assert_debug_snapshot!(response.response.headers().get(CACHE_CONTROL));
    let response = response.next_response().await.unwrap();

    insta::assert_json_snapshot!(response);
    panic!()
}*/
