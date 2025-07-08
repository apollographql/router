use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use apollo_compiler::Schema;
use http::HeaderName;
use http::HeaderValue;
use http::header::CACHE_CONTROL;
use serde_json_bytes::ByteString;
use tower::Service;
use tower::ServiceExt;

use super::plugin::ResponseCache;
use crate::Context;
use crate::MockedSubgraphs;
use crate::TestHarness;
use crate::plugin::test::MockSubgraph;
use crate::plugin::test::MockSubgraphService;
use crate::plugins::response_cache::invalidation::InvalidationRequest;
use crate::plugins::response_cache::plugin::CACHE_DEBUG_EXTENSIONS_KEY;
use crate::plugins::response_cache::plugin::CACHE_DEBUG_HEADER_NAME;
use crate::plugins::response_cache::plugin::CONTEXT_DEBUG_CACHE_KEYS;
use crate::plugins::response_cache::plugin::CacheKeysContext;
use crate::plugins::response_cache::plugin::Subgraph;
use crate::plugins::response_cache::plugin::hash_representation;
use crate::plugins::response_cache::postgres::PostgresCacheConfig;
use crate::plugins::response_cache::postgres::PostgresCacheStorage;
use crate::plugins::response_cache::postgres::default_batch_size;
use crate::plugins::response_cache::postgres::default_cleanup_interval;
use crate::plugins::response_cache::postgres::default_pool_size;
use crate::services::subgraph;
use crate::services::supergraph;

const SCHEMA: &str = include_str!("../../testdata/orga_supergraph_cache_key.graphql");
const SCHEMA_REQUIRES: &str = include_str!("../../testdata/supergraph_cache_key.graphql");
const SCHEMA_NESTED_KEYS: &str =
    include_str!("../../testdata/supergraph_nested_fields_cache_key.graphql");

#[tokio::test]
async fn insert() {
    let valid_schema = Arc::new(Schema::parse_and_validate(SCHEMA, "test.graphql").unwrap());
    let query = "query { currentUser { activeOrganization { id creatorUser { __typename id } } } }";

    let subgraphs = serde_json::json!({
        "user": {
            "query": {
                "currentUser": {
                    "activeOrganization": {
                        "__typename": "Organization",
                        "id": "1",
                    }
                }
            },
            "headers": {"cache-control": "public"},
        },
        "orga": {
            "entities": [
                {
                    "__typename": "Organization",
                    "id": "1",
                    "creatorUser": {
                        "__typename": "User",
                        "id": 2
                    }
                }
            ],
            "headers": {"cache-control": "public"},
        },
    });

    let pg_cache = PostgresCacheStorage::new(&PostgresCacheConfig {
        cleanup_interval: default_cleanup_interval(),
        url: "postgres://127.0.0.1".parse().unwrap(),
        username: None,
        password: None,
        timeout: Some(std::time::Duration::from_secs(5)),
        required_to_start: true,
        pool_size: default_pool_size(),
        batch_size: default_batch_size(),
        namespace: Some(String::from("test_insert_simple")),
    })
    .await
    .unwrap();
    let map = [
        (
            "user".to_string(),
            Subgraph {
                postgres: None,
                private_id: Some("sub".to_string()),
                enabled: true.into(),
                ttl: None,
                ..Default::default()
            },
        ),
        (
            "orga".to_string(),
            Subgraph {
                postgres: None,
                private_id: Some("sub".to_string()),
                enabled: true.into(),
                ttl: None,
                ..Default::default()
            },
        ),
    ]
    .into_iter()
    .collect();
    let response_cache =
        ResponseCache::for_test(pg_cache.clone(), map, valid_schema.clone(), true, false)
            .await
            .unwrap();

    let service = TestHarness::builder()
        .configuration_json(serde_json::json!({
            "include_subgraph_errors": { "all": true },
            "experimental_mock_subgraphs": subgraphs,
        }))
        .unwrap()
        .schema(SCHEMA)
        .extra_private_plugin(response_cache.clone())
        .build_supergraph()
        .await
        .unwrap();

    let request = supergraph::Request::fake_builder()
        .query(query)
        .context(Context::new())
        .header(
            HeaderName::from_static(CACHE_DEBUG_HEADER_NAME),
            HeaderValue::from_static("true"),
        )
        .build()
        .unwrap();
    let mut response = service.oneshot(request).await.unwrap();
    let mut cache_keys: CacheKeysContext = response
        .context
        .get(CONTEXT_DEBUG_CACHE_KEYS)
        .unwrap()
        .unwrap();
    cache_keys.iter_mut().for_each(|ck| {
        ck.invalidation_keys.sort();
        ck.cache_control.created = 0;
    });
    cache_keys.sort_by(|a, b| a.invalidation_keys.cmp(&b.invalidation_keys));
    insta::with_settings!({
        description => "Make sure everything is in status 'new' and we have all the entities and root fields"
    }, {
        insta::assert_json_snapshot!(cache_keys);
    });
    let mut entity_key = serde_json_bytes::Map::new();
    entity_key.insert(
        ByteString::from("id"),
        serde_json_bytes::Value::String(ByteString::from("1")),
    );
    let hashed_entity_key = hash_representation(&entity_key);
    let prefix_key =
        format!("version:1.0:subgraph:orga:type:Organization:entity:{hashed_entity_key}");
    assert!(
        cache_keys
            .iter()
            .flat_map(|c| &c.invalidation_keys)
            .any(|cache_key| cache_key.starts_with(&prefix_key))
    );

    assert!(
        response
            .response
            .headers()
            .get(CACHE_CONTROL)
            .and_then(|h| h.to_str().ok())
            .unwrap()
            .contains("max-age="),
    );
    assert!(
        response
            .response
            .headers()
            .get(CACHE_CONTROL)
            .and_then(|h| h.to_str().ok())
            .unwrap()
            .contains(",public"),
    );
    let mut response = response.next_response().await.unwrap();
    assert!(
        response
            .extensions
            .remove(CACHE_DEBUG_EXTENSIONS_KEY)
            .is_some()
    );
    insta::assert_json_snapshot!(response, @r###"
    {
      "data": {
        "currentUser": {
          "activeOrganization": {
            "id": "1",
            "creatorUser": {
              "__typename": "User",
              "id": 2
            }
          }
        }
      }
    }
    "###);

    let service = TestHarness::builder()
        .configuration_json(serde_json::json!({"include_subgraph_errors": { "all": true } }))
        .unwrap()
        .schema(SCHEMA)
        .extra_private_plugin(response_cache.clone())
        .build_supergraph()
        .await
        .unwrap();

    let request = supergraph::Request::fake_builder()
        .query(query)
        .context(Context::new())
        .header(
            HeaderName::from_static(CACHE_DEBUG_HEADER_NAME),
            HeaderValue::from_static("true"),
        )
        .build()
        .unwrap();
    let mut response = service.oneshot(request).await.unwrap();

    assert!(
        response
            .response
            .headers()
            .get(CACHE_CONTROL)
            .and_then(|h| h.to_str().ok())
            .unwrap()
            .contains("max-age="),
    );
    assert!(
        response
            .response
            .headers()
            .get(CACHE_CONTROL)
            .and_then(|h| h.to_str().ok())
            .unwrap()
            .contains(",public"),
    );
    let mut cache_keys: CacheKeysContext = response
        .context
        .get(CONTEXT_DEBUG_CACHE_KEYS)
        .unwrap()
        .unwrap();
    cache_keys.iter_mut().for_each(|ck| {
        ck.invalidation_keys.sort();
        ck.cache_control.created = 0;
    });
    cache_keys.sort_by(|a, b| a.invalidation_keys.cmp(&b.invalidation_keys));
    insta::with_settings!({
        description => "Make sure everything is in status 'cached' and we have all the entities and root fields"
    }, {
        insta::assert_json_snapshot!(cache_keys);
    });

    let mut response = response.next_response().await.unwrap();
    assert!(
        response
            .extensions
            .remove(CACHE_DEBUG_EXTENSIONS_KEY)
            .is_some()
    );
    insta::assert_json_snapshot!(response, @r###"
    {
      "data": {
        "currentUser": {
          "activeOrganization": {
            "id": "1",
            "creatorUser": {
              "__typename": "User",
              "id": 2
            }
          }
        }
      }
    }
    "###);
}

#[tokio::test]
async fn insert_without_debug_header() {
    let valid_schema = Arc::new(Schema::parse_and_validate(SCHEMA, "test.graphql").unwrap());
    let query = "query { currentUser { activeOrganization { id creatorUser { __typename id } } } }";

    let subgraphs = serde_json::json!({
        "user": {
            "query": {
                "currentUser": {
                    "activeOrganization": {
                        "__typename": "Organization",
                        "id": "1",
                    }
                }
            },
            "headers": {"cache-control": "public"},
        },
        "orga": {
            "entities": [
                {
                    "__typename": "Organization",
                    "id": "1",
                    "creatorUser": {
                        "__typename": "User",
                        "id": 2
                    }
                }
            ],
            "headers": {"cache-control": "public"},
        },
    });

    let pg_cache = PostgresCacheStorage::new(&PostgresCacheConfig {
        url: "postgres://127.0.0.1".parse().unwrap(),
        username: None,
        password: None,
        timeout: Some(std::time::Duration::from_secs(5)),
        required_to_start: true,
        pool_size: default_pool_size(),
        batch_size: default_batch_size(),
        cleanup_interval: Duration::from_secs(60 * 60),
        namespace: Some(String::from("insert_without_debug_header")),
    })
    .await
    .unwrap();
    let map = [
        (
            "user".to_string(),
            Subgraph {
                postgres: None,
                private_id: Some("sub".to_string()),
                enabled: true.into(),
                ttl: None,
                ..Default::default()
            },
        ),
        (
            "orga".to_string(),
            Subgraph {
                postgres: None,
                private_id: Some("sub".to_string()),
                enabled: true.into(),
                ttl: None,
                ..Default::default()
            },
        ),
    ]
    .into_iter()
    .collect();
    let response_cache =
        ResponseCache::for_test(pg_cache.clone(), map, valid_schema.clone(), true, false)
            .await
            .unwrap();

    let service = TestHarness::builder()
        .configuration_json(serde_json::json!({
            "include_subgraph_errors": { "all": true },
            "experimental_mock_subgraphs": subgraphs,
        }))
        .unwrap()
        .schema(SCHEMA)
        .extra_private_plugin(response_cache.clone())
        .build_supergraph()
        .await
        .unwrap();

    let request = supergraph::Request::fake_builder()
        .query(query)
        .context(Context::new())
        .build()
        .unwrap();
    let mut response = service.oneshot(request).await.unwrap();
    assert!(
        response
            .context
            .get::<_, CacheKeysContext>(CONTEXT_DEBUG_CACHE_KEYS)
            .ok()
            .flatten()
            .is_none()
    );

    assert!(
        response
            .response
            .headers()
            .get(CACHE_CONTROL)
            .and_then(|h| h.to_str().ok())
            .unwrap()
            .contains("max-age="),
    );
    assert!(
        response
            .response
            .headers()
            .get(CACHE_CONTROL)
            .and_then(|h| h.to_str().ok())
            .unwrap()
            .contains(",public"),
    );
    let mut response = response.next_response().await.unwrap();
    assert!(
        response
            .extensions
            .remove(CACHE_DEBUG_EXTENSIONS_KEY)
            .is_none()
    );
    insta::assert_json_snapshot!(response, @r###"
    {
      "data": {
        "currentUser": {
          "activeOrganization": {
            "id": "1",
            "creatorUser": {
              "__typename": "User",
              "id": 2
            }
          }
        }
      }
    }
    "###);

    let service = TestHarness::builder()
        .configuration_json(serde_json::json!({"include_subgraph_errors": { "all": true } }))
        .unwrap()
        .schema(SCHEMA)
        .extra_private_plugin(response_cache.clone())
        .build_supergraph()
        .await
        .unwrap();

    let request = supergraph::Request::fake_builder()
        .query(query)
        .context(Context::new())
        .build()
        .unwrap();
    let mut response = service.oneshot(request).await.unwrap();

    assert!(
        response
            .response
            .headers()
            .get(CACHE_CONTROL)
            .and_then(|h| h.to_str().ok())
            .unwrap()
            .contains("max-age="),
    );
    assert!(
        response
            .response
            .headers()
            .get(CACHE_CONTROL)
            .and_then(|h| h.to_str().ok())
            .unwrap()
            .contains(",public"),
    );
    assert!(
        response
            .context
            .get::<_, CacheKeysContext>(CONTEXT_DEBUG_CACHE_KEYS)
            .ok()
            .flatten()
            .is_none()
    );

    let mut response = response.next_response().await.unwrap();
    assert!(
        response
            .extensions
            .remove(CACHE_DEBUG_EXTENSIONS_KEY)
            .is_none()
    );
    insta::assert_json_snapshot!(response, @r###"
    {
      "data": {
        "currentUser": {
          "activeOrganization": {
            "id": "1",
            "creatorUser": {
              "__typename": "User",
              "id": 2
            }
          }
        }
      }
    }
    "###);
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
                            "weight": 5,
                            "price": 150,
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

    let pg_cache = PostgresCacheStorage::new(&PostgresCacheConfig {
        cleanup_interval: default_cleanup_interval(),
        url: "postgres://127.0.0.1".parse().unwrap(),
        username: None,
        password: None,
        timeout: Some(std::time::Duration::from_secs(5)),
        required_to_start: true,
        pool_size: default_pool_size(),
        batch_size: default_batch_size(),
        namespace: Some(String::from("test_insert_with_requires")),
    })
    .await
    .unwrap();
    let map: HashMap<String, Subgraph> = [
        (
            "products".to_string(),
            Subgraph {
                postgres: None,
                private_id: Some("sub".to_string()),
                enabled: true.into(),
                ttl: None,
                ..Default::default()
            },
        ),
        (
            "inventory".to_string(),
            Subgraph {
                postgres: None,
                private_id: Some("sub".to_string()),
                enabled: true.into(),
                ttl: None,
                ..Default::default()
            },
        ),
    ]
    .into_iter()
    .collect();
    let response_cache = ResponseCache::for_test(
        pg_cache.clone(),
        map.clone(),
        valid_schema.clone(),
        true,
        false,
    )
    .await
    .unwrap();

    let service = TestHarness::builder()
        .configuration_json(serde_json::json!({"include_subgraph_errors": { "all": true } }))
        .unwrap()
        .schema(SCHEMA_REQUIRES)
        .extra_private_plugin(response_cache.clone())
        .extra_plugin(subgraphs.clone())
        .build_supergraph()
        .await
        .unwrap();

    let request = supergraph::Request::fake_builder()
        .query(query)
        .context(Context::new())
        .header(
            HeaderName::from_static(CACHE_DEBUG_HEADER_NAME),
            HeaderValue::from_static("true"),
        )
        .build()
        .unwrap();
    let mut response = service.oneshot(request).await.unwrap();
    let mut cache_keys: CacheKeysContext = response
        .context
        .get(CONTEXT_DEBUG_CACHE_KEYS)
        .unwrap()
        .unwrap();
    cache_keys.iter_mut().for_each(|ck| {
        ck.invalidation_keys.sort();
        ck.cache_control.created = 0;
    });
    cache_keys.sort_by(|a, b| a.invalidation_keys.cmp(&b.invalidation_keys));
    let mut entity_key = serde_json_bytes::Map::new();
    entity_key.insert(
        ByteString::from("upc"),
        serde_json_bytes::Value::String(ByteString::from("1")),
    );
    let hashed_entity_key = hash_representation(&entity_key);
    let prefix_key =
        format!("version:1.0:subgraph:inventory:type:Product:entity:{hashed_entity_key}");
    assert!(
        cache_keys
            .iter()
            .flat_map(|c| &c.invalidation_keys)
            .any(|cache_key| cache_key.starts_with(&prefix_key))
    );
    insta::with_settings!({
        description => "Make sure everything is in status 'new' and we have all the entities and root fields"
    }, {
        insta::assert_json_snapshot!(cache_keys);
    });

    assert!(
        response
            .response
            .headers()
            .get(CACHE_CONTROL)
            .and_then(|h| h.to_str().ok())
            .unwrap()
            .contains("max-age="),
    );
    assert!(
        response
            .response
            .headers()
            .get(CACHE_CONTROL)
            .and_then(|h| h.to_str().ok())
            .unwrap()
            .contains(",public"),
    );
    let mut response = response.next_response().await.unwrap();
    assert!(
        response
            .extensions
            .remove(CACHE_DEBUG_EXTENSIONS_KEY)
            .is_some()
    );

    insta::assert_json_snapshot!(response, @r###"
    {
      "data": {
        "topProducts": [
          {
            "name": "Test",
            "shippingEstimate": 15,
            "price": 150
          }
        ]
      }
    }
    "###);

    let service = TestHarness::builder()
        .configuration_json(serde_json::json!({"include_subgraph_errors": { "all": true } }))
        .unwrap()
        .schema(SCHEMA_REQUIRES)
        .extra_private_plugin(response_cache)
        .extra_plugin(subgraphs.clone())
        .build_supergraph()
        .await
        .unwrap();

    let request = supergraph::Request::fake_builder()
        .query(query)
        .context(Context::new())
        .header(
            HeaderName::from_static(CACHE_DEBUG_HEADER_NAME),
            HeaderValue::from_static("true"),
        )
        .build()
        .unwrap();
    let mut response = service.oneshot(request).await.unwrap();
    let mut cache_keys: CacheKeysContext = response
        .context
        .get(CONTEXT_DEBUG_CACHE_KEYS)
        .unwrap()
        .unwrap();
    cache_keys.iter_mut().for_each(|ck| {
        ck.invalidation_keys.sort();
        ck.cache_control.created = 0;
    });
    cache_keys.sort_by(|a, b| a.invalidation_keys.cmp(&b.invalidation_keys));
    insta::with_settings!({
        description => "Make sure everything is in status 'cached' and we have all the entities and root fields"
    }, {
        insta::assert_json_snapshot!(cache_keys);
    });
    assert!(
        response
            .response
            .headers()
            .get(CACHE_CONTROL)
            .and_then(|h| h.to_str().ok())
            .unwrap()
            .contains("max-age="),
    );
    assert!(
        response
            .response
            .headers()
            .get(CACHE_CONTROL)
            .and_then(|h| h.to_str().ok())
            .unwrap()
            .contains(",public"),
    );

    let mut response = response.next_response().await.unwrap();
    assert!(
        response
            .extensions
            .remove(CACHE_DEBUG_EXTENSIONS_KEY)
            .is_some()
    );

    insta::assert_json_snapshot!(response, @r###"
    {
      "data": {
        "topProducts": [
          {
            "name": "Test",
            "shippingEstimate": 15,
            "price": 150
          }
        ]
      }
    }
    "###);
}

#[tokio::test]
async fn insert_with_nested_field_set() {
    let valid_schema =
        Arc::new(Schema::parse_and_validate(SCHEMA_NESTED_KEYS, "test.graphql").unwrap());
    let query = "query { allProducts { name createdBy { name country { a } } } }";

    let subgraphs = serde_json::json!({
        "products": {
            "query": {"allProducts": [{
                "id": "1",
                "name": "Test",
                "sku": "150",
                "createdBy": { "__typename": "User", "email": "test@test.com", "country": {"a": "France"} }
            }]},
            "headers": {"cache-control": "public"},
        },
        "users": {
            "entities": [{
                "__typename": "User",
                "email": "test@test.com",
                "name": "test",
                "country": {
                    "a": "France"
                }
            }],
            "headers": {"cache-control": "public"},
        }
    });

    let pg_cache = PostgresCacheStorage::new(&PostgresCacheConfig {
        cleanup_interval: default_cleanup_interval(),
        url: "postgres://127.0.0.1".parse().unwrap(),
        username: None,
        password: None,
        timeout: Some(std::time::Duration::from_secs(5)),
        required_to_start: true,
        pool_size: default_pool_size(),
        batch_size: default_batch_size(),
        namespace: Some(String::from("test_insert_with_nested_field_set")),
    })
    .await
    .unwrap();
    let map = [
        (
            "products".to_string(),
            Subgraph {
                postgres: None,
                private_id: Some("sub".to_string()),
                enabled: true.into(),
                ttl: None,
                ..Default::default()
            },
        ),
        (
            "users".to_string(),
            Subgraph {
                postgres: None,
                private_id: Some("sub".to_string()),
                enabled: true.into(),
                ttl: None,
                ..Default::default()
            },
        ),
    ]
    .into_iter()
    .collect();
    let response_cache =
        ResponseCache::for_test(pg_cache.clone(), map, valid_schema.clone(), true, false)
            .await
            .unwrap();

    let service = TestHarness::builder()
        .configuration_json(serde_json::json!({"include_subgraph_errors": { "all": true }, "experimental_mock_subgraphs": subgraphs.clone() }))
        .unwrap()
        .schema(SCHEMA_NESTED_KEYS)
        .extra_private_plugin(response_cache.clone())
        .build_supergraph()
        .await
        .unwrap();

    let request = supergraph::Request::fake_builder()
        .query(query)
        .context(Context::new())
        .header(
            HeaderName::from_static(CACHE_DEBUG_HEADER_NAME),
            HeaderValue::from_static("true"),
        )
        .build()
        .unwrap();
    let mut response = service.oneshot(request).await.unwrap();
    let mut cache_keys: CacheKeysContext = response
        .context
        .get(CONTEXT_DEBUG_CACHE_KEYS)
        .unwrap()
        .unwrap();
    cache_keys.iter_mut().for_each(|ck| {
        ck.invalidation_keys.sort();
        ck.cache_control.created = 0;
    });
    cache_keys.sort_by(|a, b| a.invalidation_keys.cmp(&b.invalidation_keys));
    let mut entity_key = serde_json_bytes::Map::new();
    entity_key.insert(
        ByteString::from("email"),
        serde_json_bytes::Value::String(ByteString::from("test@test.com")),
    );
    entity_key.insert(
        ByteString::from("country"),
        serde_json_bytes::json!({"a": "France"}),
    );

    let hashed_entity_key = hash_representation(&entity_key);
    let prefix_key = format!("version:1.0:subgraph:users:type:User:entity:{hashed_entity_key}");
    assert!(
        cache_keys
            .iter()
            .flat_map(|c| &c.invalidation_keys)
            .any(|cache_key| cache_key.starts_with(&prefix_key))
    );
    insta::with_settings!({
        description => "Make sure everything is in status 'new' and we have all the entities and root fields"
    }, {
        insta::assert_json_snapshot!(cache_keys);
    });

    assert!(
        response
            .response
            .headers()
            .get(CACHE_CONTROL)
            .and_then(|h| h.to_str().ok())
            .unwrap()
            .contains("max-age="),
    );
    assert!(
        response
            .response
            .headers()
            .get(CACHE_CONTROL)
            .and_then(|h| h.to_str().ok())
            .unwrap()
            .contains(",public"),
    );

    let mut response = response.next_response().await.unwrap();
    assert!(
        response
            .extensions
            .remove(CACHE_DEBUG_EXTENSIONS_KEY)
            .is_some()
    );

    insta::assert_json_snapshot!(response, @r###"
    {
      "data": {
        "allProducts": [
          {
            "name": "Test",
            "createdBy": {
              "name": "test",
              "country": {
                "a": "France"
              }
            }
          }
        ]
      }
    }
    "###);

    let service = TestHarness::builder()
        .configuration_json(serde_json::json!({"include_subgraph_errors": { "all": true }, "experimental_mock_subgraphs": subgraphs.clone() }))
        .unwrap()
        .schema(SCHEMA_NESTED_KEYS)
        .extra_private_plugin(response_cache.clone())
        .build_supergraph()
        .await
        .unwrap();

    let request = supergraph::Request::fake_builder()
        .query(query)
        .context(Context::new())
        .header(
            HeaderName::from_static(CACHE_DEBUG_HEADER_NAME),
            HeaderValue::from_static("true"),
        )
        .build()
        .unwrap();
    let mut response = service.oneshot(request).await.unwrap();

    assert!(
        response
            .response
            .headers()
            .get(CACHE_CONTROL)
            .and_then(|h| h.to_str().ok())
            .unwrap()
            .contains("max-age="),
    );
    assert!(
        response
            .response
            .headers()
            .get(CACHE_CONTROL)
            .and_then(|h| h.to_str().ok())
            .unwrap()
            .contains(",public"),
    );
    let mut cache_keys: CacheKeysContext = response
        .context
        .get(CONTEXT_DEBUG_CACHE_KEYS)
        .unwrap()
        .unwrap();
    cache_keys.iter_mut().for_each(|ck| {
        ck.invalidation_keys.sort();
        ck.cache_control.created = 0;
    });
    cache_keys.sort_by(|a, b| a.invalidation_keys.cmp(&b.invalidation_keys));
    insta::with_settings!({
        description => "Make sure everything is in status 'cached' and we have all the entities and root fields"
    }, {
        insta::assert_json_snapshot!(cache_keys);
    });

    let mut response = response.next_response().await.unwrap();
    assert!(
        response
            .extensions
            .remove(CACHE_DEBUG_EXTENSIONS_KEY)
            .is_some()
    );

    insta::assert_json_snapshot!(response, @r###"
    {
      "data": {
        "allProducts": [
          {
            "name": "Test",
            "createdBy": {
              "name": "test",
              "country": {
                "a": "France"
              }
            }
          }
        ]
      }
    }
    "###);
}

#[tokio::test]
async fn no_cache_control() {
    let valid_schema = Arc::new(Schema::parse_and_validate(SCHEMA, "test.graphql").unwrap());
    let query = "query { currentUser { activeOrganization { id creatorUser { __typename id } } } }";

    let subgraphs = serde_json::json!({
        "user": {
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

    let pg_cache = PostgresCacheStorage::new(&PostgresCacheConfig {
        cleanup_interval: default_cleanup_interval(),
        url: "postgres://127.0.0.1".parse().unwrap(),
        username: None,
        password: None,
        timeout: Some(std::time::Duration::from_secs(5)),
        required_to_start: true,
        pool_size: default_pool_size(),
        batch_size: default_batch_size(),
        namespace: Some(String::from("test_no_cache_control")),
    })
    .await
    .unwrap();
    let response_cache = ResponseCache::for_test(
        pg_cache.clone(),
        HashMap::new(),
        valid_schema.clone(),
        false,
        false,
    )
    .await
    .unwrap();

    let service = TestHarness::builder()
        .configuration_json(serde_json::json!({"include_subgraph_errors": { "all": true }, "experimental_mock_subgraphs": subgraphs.clone() }))
        .unwrap()
        .schema(SCHEMA)
        .extra_private_plugin(response_cache.clone())
        .build_supergraph()
        .await
        .unwrap();

    let request = supergraph::Request::fake_builder()
        .query(query)
        .context(Context::new())
        .header(
            HeaderName::from_static(CACHE_DEBUG_HEADER_NAME),
            HeaderValue::from_static("true"),
        )
        .build()
        .unwrap();
    let mut response = service.oneshot(request).await.unwrap();

    assert_eq!(
        response
            .response
            .headers()
            .get(CACHE_CONTROL)
            .and_then(|h| h.to_str().ok())
            .unwrap(),
        "no-store"
    );
    let mut response = response.next_response().await.unwrap();
    assert!(
        response
            .extensions
            .remove(CACHE_DEBUG_EXTENSIONS_KEY)
            .is_some()
    );

    insta::assert_json_snapshot!(response, @r###"
    {
      "data": {
        "currentUser": {
          "activeOrganization": {
            "id": "1",
            "creatorUser": {
              "__typename": "User",
              "id": 2
            }
          }
        }
      }
    }
    "###);

    let service = TestHarness::builder()
        .configuration_json(serde_json::json!({"include_subgraph_errors": { "all": true }, "experimental_mock_subgraphs": subgraphs.clone() }))
        .unwrap()
        .schema(SCHEMA)
        .extra_private_plugin(response_cache.clone())
        .build_supergraph()
        .await
        .unwrap();

    let request = supergraph::Request::fake_builder()
        .query(query)
        .context(Context::new())
        .header(
            HeaderName::from_static(CACHE_DEBUG_HEADER_NAME),
            HeaderValue::from_static("true"),
        )
        .build()
        .unwrap();
    let mut response = service.oneshot(request).await.unwrap();

    assert_eq!(
        response
            .response
            .headers()
            .get(CACHE_CONTROL)
            .and_then(|h| h.to_str().ok())
            .unwrap(),
        "no-store"
    );
    let mut response = response.next_response().await.unwrap();
    assert!(
        response
            .extensions
            .remove(CACHE_DEBUG_EXTENSIONS_KEY)
            .is_some()
    );

    insta::assert_json_snapshot!(response, @r###"
    {
      "data": {
        "currentUser": {
          "activeOrganization": {
            "id": "1",
            "creatorUser": {
              "__typename": "User",
              "id": 2
            }
          }
        }
      }
    }
    "###);
}

#[tokio::test]
async fn private() {
    let query = "query { currentUser { activeOrganization { id creatorUser { __typename id } } } }";
    let valid_schema = Arc::new(Schema::parse_and_validate(SCHEMA, "test.graphql").unwrap());

    let subgraphs = serde_json::json!({
        "user": {
            "query": {
                "currentUser": {
                    "activeOrganization": {
                        "__typename": "Organization",
                        "id": "1",
                    }
                }
            },
            "headers": {"cache-control": "private"},
        },
        "orga": {
            "entities": [
                {
                    "__typename": "Organization",
                    "id": "1",
                    "creatorUser": {
                        "__typename": "User",
                        "id": 2
                    }
                }
            ],
            "headers": {"cache-control": "private"},
        },
    });

    let pg_cache = PostgresCacheStorage::new(&PostgresCacheConfig {
        cleanup_interval: default_cleanup_interval(),
        url: "postgres://127.0.0.1".parse().unwrap(),
        username: None,
        password: None,
        timeout: Some(std::time::Duration::from_secs(5)),
        required_to_start: true,
        pool_size: default_pool_size(),
        batch_size: default_batch_size(),
        namespace: Some(String::from("private")),
    })
    .await
    .unwrap();
    let map = [
        (
            "user".to_string(),
            Subgraph {
                postgres: None,
                private_id: Some("sub".to_string()),
                enabled: true.into(),
                ttl: None,
                ..Default::default()
            },
        ),
        (
            "orga".to_string(),
            Subgraph {
                postgres: None,
                private_id: Some("sub".to_string()),
                enabled: true.into(),
                ttl: None,
                ..Default::default()
            },
        ),
    ]
    .into_iter()
    .collect();
    let response_cache =
        ResponseCache::for_test(pg_cache.clone(), map, valid_schema.clone(), true, false)
            .await
            .unwrap();

    let mut service = TestHarness::builder()
        .configuration_json(serde_json::json!({"include_subgraph_errors": { "all": true }, "experimental_mock_subgraphs": subgraphs.clone() }))
        .unwrap()
        .schema(SCHEMA)
        .extra_private_plugin(response_cache.clone())
        .build_supergraph()
        .await
        .unwrap();

    let context = Context::new();
    context.insert_json_value("sub", "1234".into());

    let request = supergraph::Request::fake_builder()
        .query(query)
        .context(context)
        .header(
            HeaderName::from_static(CACHE_DEBUG_HEADER_NAME),
            HeaderValue::from_static("true"),
        )
        .build()
        .unwrap();
    let mut response = service.ready().await.unwrap().call(request).await.unwrap();
    let mut cache_keys: CacheKeysContext = response
        .context
        .get(CONTEXT_DEBUG_CACHE_KEYS)
        .unwrap()
        .unwrap();
    cache_keys.iter_mut().for_each(|ck| {
        ck.invalidation_keys.sort();
        ck.cache_control.created = 0;
    });
    cache_keys.sort_by(|a, b| a.invalidation_keys.cmp(&b.invalidation_keys));
    insta::assert_json_snapshot!(cache_keys);

    let mut response = response.next_response().await.unwrap();
    assert!(
        response
            .extensions
            .remove(CACHE_DEBUG_EXTENSIONS_KEY)
            .is_some()
    );
    insta::assert_json_snapshot!(response, @r###"
    {
      "data": {
        "currentUser": {
          "activeOrganization": {
            "id": "1",
            "creatorUser": {
              "__typename": "User",
              "id": 2
            }
          }
        }
      }
    }
    "###);
    // Now testing without any mock subgraphs, all the data should come from the cache
    let mut service = TestHarness::builder()
        .configuration_json(serde_json::json!({"include_subgraph_errors": { "all": true }, "experimental_mock_subgraphs": subgraphs.clone() }))
        .unwrap()
        .schema(SCHEMA)
        .extra_private_plugin(response_cache.clone())
        .build_supergraph()
        .await
        .unwrap();

    let context = Context::new();
    context.insert_json_value("sub", "1234".into());

    let request = supergraph::Request::fake_builder()
        .query(query)
        .context(context)
        .header(
            HeaderName::from_static(CACHE_DEBUG_HEADER_NAME),
            HeaderValue::from_static("true"),
        )
        .build()
        .unwrap();
    let mut response = service.ready().await.unwrap().call(request).await.unwrap();
    let mut cache_keys: CacheKeysContext = response
        .context
        .get(CONTEXT_DEBUG_CACHE_KEYS)
        .unwrap()
        .unwrap();
    cache_keys.iter_mut().for_each(|ck| {
        ck.invalidation_keys.sort();
        ck.cache_control.created = 0;
    });
    cache_keys.sort_by(|a, b| a.invalidation_keys.cmp(&b.invalidation_keys));
    insta::assert_json_snapshot!(cache_keys);

    let mut response = response.next_response().await.unwrap();
    assert!(
        response
            .extensions
            .remove(CACHE_DEBUG_EXTENSIONS_KEY)
            .is_some()
    );

    insta::assert_json_snapshot!(response, @r###"
    {
      "data": {
        "currentUser": {
          "activeOrganization": {
            "id": "1",
            "creatorUser": {
              "__typename": "User",
              "id": 2
            }
          }
        }
      }
    }
    "###);

    let context = Context::new();
    context.insert_json_value("sub", "5678".into());
    let request = supergraph::Request::fake_builder()
        .query(query)
        .context(context)
        .header(
            HeaderName::from_static(CACHE_DEBUG_HEADER_NAME),
            HeaderValue::from_static("true"),
        )
        .build()
        .unwrap();
    let mut response = service.ready().await.unwrap().call(request).await.unwrap();
    let mut cache_keys: CacheKeysContext = response
        .context
        .get(CONTEXT_DEBUG_CACHE_KEYS)
        .unwrap()
        .unwrap();
    cache_keys.iter_mut().for_each(|ck| {
        ck.invalidation_keys.sort();
        ck.cache_control.created = 0;
    });
    cache_keys.sort_by(|a, b| a.invalidation_keys.cmp(&b.invalidation_keys));
    insta::assert_json_snapshot!(cache_keys);

    let mut response = response.next_response().await.unwrap();
    assert!(
        response
            .extensions
            .remove(CACHE_DEBUG_EXTENSIONS_KEY)
            .is_some()
    );

    insta::assert_json_snapshot!(response, @r###"
    {
      "data": {
        "currentUser": {
          "activeOrganization": {
            "id": "1",
            "creatorUser": {
              "__typename": "User",
              "id": 2
            }
          }
        }
      }
    }
    "###);
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

    let pg_cache = PostgresCacheStorage::new(&PostgresCacheConfig {
        cleanup_interval: default_cleanup_interval(),
        url: "postgres://127.0.0.1".parse().unwrap(),
        username: None,
        password: None,
        timeout: Some(std::time::Duration::from_secs(5)),
        required_to_start: true,
        pool_size: default_pool_size(),
        batch_size: default_batch_size(),
        namespace: Some(String::from("no_data")),
    })
    .await
    .unwrap();
    let map = [
        (
            "user".to_string(),
            Subgraph {
                postgres: None,
                private_id: Some("sub".to_string()),
                enabled: true.into(),
                ttl: None,
                ..Default::default()
            },
        ),
        (
            "orga".to_string(),
            Subgraph {
                postgres: None,
                private_id: Some("sub".to_string()),
                enabled: true.into(),
                ttl: None,
                ..Default::default()
            },
        ),
    ]
    .into_iter()
    .collect();
    let response_cache =
        ResponseCache::for_test(pg_cache.clone(), map, valid_schema.clone(), true, false)
            .await
            .unwrap();

    let service = TestHarness::builder()
        .configuration_json(serde_json::json!({"include_subgraph_errors": { "all": true } }))
        .unwrap()
        .schema(SCHEMA)
        .extra_private_plugin(response_cache.clone())
        .extra_plugin(subgraphs)
        .build_supergraph()
        .await
        .unwrap();

    let request = supergraph::Request::fake_builder()
        .query(query)
        .context(Context::new())
        .header(
            HeaderName::from_static(CACHE_DEBUG_HEADER_NAME),
            HeaderValue::from_static("true"),
        )
        .build()
        .unwrap();
    let mut response = service.oneshot(request).await.unwrap();

    let mut cache_keys: CacheKeysContext = response
        .context
        .get(CONTEXT_DEBUG_CACHE_KEYS)
        .unwrap()
        .unwrap();
    cache_keys.iter_mut().for_each(|ck| {
        ck.invalidation_keys.sort();
        ck.cache_control.created = 0;
    });
    cache_keys.sort_by(|a, b| a.invalidation_keys.cmp(&b.invalidation_keys));
    insta::assert_json_snapshot!(cache_keys, {
        "[].cache_control" => insta::dynamic_redaction(|value, _path| {
            let cache_control = value.as_str().unwrap().to_string();
            assert!(cache_control.contains("max-age="));
            assert!(cache_control.contains("public"));
            "[REDACTED]"
        })
    });

    let mut response = response.next_response().await.unwrap();
    assert!(
        response
            .extensions
            .remove(CACHE_DEBUG_EXTENSIONS_KEY)
            .is_some()
    );

    insta::assert_json_snapshot!(response, @r###"
    {
      "data": {
        "currentUser": {
          "allOrganizations": [
            {
              "id": "1",
              "name": "Organization 1"
            },
            {
              "id": "3",
              "name": "Organization 3"
            }
          ]
        }
      }
    }
    "###);

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
        .extra_private_plugin(response_cache)
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
        .header(
            HeaderName::from_static(CACHE_DEBUG_HEADER_NAME),
            HeaderValue::from_static("true"),
        )
        .build()
        .unwrap();
    let mut response = service.oneshot(request).await.unwrap();

    let mut cache_keys: CacheKeysContext = response
        .context
        .get(CONTEXT_DEBUG_CACHE_KEYS)
        .unwrap()
        .unwrap();
    cache_keys.iter_mut().for_each(|ck| {
        ck.invalidation_keys.sort();
        ck.cache_control.created = 0;
    });
    cache_keys.sort_by(|a, b| a.invalidation_keys.cmp(&b.invalidation_keys));
    insta::assert_json_snapshot!(cache_keys);
    let mut response = response.next_response().await.unwrap();
    assert!(
        response
            .extensions
            .remove(CACHE_DEBUG_EXTENSIONS_KEY)
            .is_some()
    );

    insta::assert_json_snapshot!(response, @r###"
    {
      "data": {
        "currentUser": {
          "allOrganizations": [
            {
              "id": "1",
              "name": "Organization 1"
            },
            {
              "id": "2",
              "name": null
            },
            {
              "id": "3",
              "name": "Organization 3"
            }
          ]
        }
      },
      "errors": [
        {
          "message": "HTTP fetch failed from 'orga': orga not found",
          "path": [
            "currentUser",
            "allOrganizations",
            1
          ],
          "extensions": {
            "code": "SUBREQUEST_HTTP_ERROR",
            "service": "orga",
            "reason": "orga not found"
          }
        }
      ]
    }
    "###);
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

    let pg_cache = PostgresCacheStorage::new(&PostgresCacheConfig {
        cleanup_interval: default_cleanup_interval(),
        url: "postgres://127.0.0.1".parse().unwrap(),
        username: None,
        password: None,
        timeout: Some(std::time::Duration::from_secs(5)),
        required_to_start: true,
        pool_size: default_pool_size(),
        batch_size: default_batch_size(),
        namespace: Some(String::from("missing_entities")),
    })
    .await
    .unwrap();
    let map = [
        (
            "user".to_string(),
            Subgraph {
                postgres: None,
                private_id: Some("sub".to_string()),
                enabled: true.into(),
                ttl: None,
                ..Default::default()
            },
        ),
        (
            "orga".to_string(),
            Subgraph {
                postgres: None,
                private_id: Some("sub".to_string()),
                enabled: true.into(),
                ttl: None,
                ..Default::default()
            },
        ),
    ]
    .into_iter()
    .collect();
    let response_cache =
        ResponseCache::for_test(pg_cache.clone(), map, valid_schema.clone(), true, false)
            .await
            .unwrap();

    let service = TestHarness::builder()
        .configuration_json(serde_json::json!({"include_subgraph_errors": { "all": true } }))
        .unwrap()
        .schema(SCHEMA)
        .extra_private_plugin(response_cache.clone())
        .extra_plugin(subgraphs)
        .build_supergraph()
        .await
        .unwrap();

    let request = supergraph::Request::fake_builder()
        .query(query)
        .context(Context::new())
        .header(
            HeaderName::from_static(CACHE_DEBUG_HEADER_NAME),
            HeaderValue::from_static("true"),
        )
        .build()
        .unwrap();
    let mut response = service.oneshot(request).await.unwrap();
    let mut response = response.next_response().await.unwrap();
    assert!(
        response
            .extensions
            .remove(CACHE_DEBUG_EXTENSIONS_KEY)
            .is_some()
    );
    insta::assert_json_snapshot!(response);

    let response_cache = ResponseCache::for_test(
        pg_cache.clone(),
        HashMap::new(),
        valid_schema.clone(),
        false,
        false,
    )
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
        .extra_private_plugin(response_cache)
        .extra_plugin(subgraphs)
        .build_supergraph()
        .await
        .unwrap();

    let request = supergraph::Request::fake_builder()
        .query(query)
        .context(Context::new())
        .header(
            HeaderName::from_static(CACHE_DEBUG_HEADER_NAME),
            HeaderValue::from_static("true"),
        )
        .build()
        .unwrap();
    let mut response = service.oneshot(request).await.unwrap();
    let mut response = response.next_response().await.unwrap();
    assert!(
        response
            .extensions
            .remove(CACHE_DEBUG_EXTENSIONS_KEY)
            .is_some()
    );

    insta::assert_json_snapshot!(response);
}

#[tokio::test(flavor = "multi_thread")]
async fn invalidate() {
    let valid_schema = Arc::new(Schema::parse_and_validate(SCHEMA, "test.graphql").unwrap());
    let query = "query { currentUser { activeOrganization { id creatorUser { __typename id } } } }";
    let subgraphs = serde_json::json!({
        "user": {
            "query": {
                "currentUser": {
                    "activeOrganization": {
                        "__typename": "Organization",
                        "id": "1",
                    }
                }
            },
            "headers": {"cache-control": "public"},
        },
        "orga": {
            "entities": [
                {
                    "__typename": "Organization",
                    "id": "1",
                    "creatorUser": {
                        "__typename": "User",
                        "id": 2
                    }
                }
            ],
            "headers": {"cache-control": "public"},
        },
    });

    let pg_cache = PostgresCacheStorage::new(&PostgresCacheConfig {
        cleanup_interval: default_cleanup_interval(),
        url: "postgres://127.0.0.1".parse().unwrap(),
        username: None,
        password: None,
        timeout: Some(std::time::Duration::from_secs(5)),
        required_to_start: true,
        pool_size: default_pool_size(),
        batch_size: default_batch_size(),
        namespace: Some(String::from("test_invalidate")),
    })
    .await
    .unwrap();
    let map = [
        (
            "user".to_string(),
            Subgraph {
                postgres: None,
                private_id: Some("sub".to_string()),
                enabled: true.into(),
                ttl: None,
                ..Default::default()
            },
        ),
        (
            "orga".to_string(),
            Subgraph {
                postgres: None,
                private_id: Some("sub".to_string()),
                enabled: true.into(),
                ttl: None,
                ..Default::default()
            },
        ),
    ]
    .into_iter()
    .collect();
    let response_cache =
        ResponseCache::for_test(pg_cache.clone(), map, valid_schema.clone(), true, false)
            .await
            .unwrap();

    let invalidation = response_cache.invalidation.clone();

    let service = TestHarness::builder()
        .configuration_json(serde_json::json!({"include_subgraph_errors": { "all": true }, "experimental_mock_subgraphs": subgraphs.clone() }))
        .unwrap()
        .schema(SCHEMA)
        .extra_private_plugin(response_cache.clone())
        .build_supergraph()
        .await
        .unwrap();

    let request = supergraph::Request::fake_builder()
        .query(query)
        .context(Context::new())
        .header(
            HeaderName::from_static(CACHE_DEBUG_HEADER_NAME),
            HeaderValue::from_static("true"),
        )
        .build()
        .unwrap();
    let mut response = service.oneshot(request).await.unwrap();
    let mut cache_keys: CacheKeysContext = response
        .context
        .get(CONTEXT_DEBUG_CACHE_KEYS)
        .unwrap()
        .unwrap();
    cache_keys.iter_mut().for_each(|ck| {
        ck.invalidation_keys.sort();
        ck.cache_control.created = 0;
    });
    cache_keys.sort_by(|a, b| a.invalidation_keys.cmp(&b.invalidation_keys));
    insta::assert_json_snapshot!(cache_keys);
    assert!(
        response
            .response
            .headers()
            .get(CACHE_CONTROL)
            .and_then(|h| h.to_str().ok())
            .unwrap()
            .contains("max-age="),
    );
    assert!(
        response
            .response
            .headers()
            .get(CACHE_CONTROL)
            .and_then(|h| h.to_str().ok())
            .unwrap()
            .contains(",public"),
    );
    let mut response = response.next_response().await.unwrap();
    assert!(
        response
            .extensions
            .remove(CACHE_DEBUG_EXTENSIONS_KEY)
            .is_some()
    );

    insta::assert_json_snapshot!(response, @r###"
    {
      "data": {
        "currentUser": {
          "activeOrganization": {
            "id": "1",
            "creatorUser": {
              "__typename": "User",
              "id": 2
            }
          }
        }
      }
    }
    "###);

    // Now testing without any mock subgraphs, all the data should come from the cache
    let service = TestHarness::builder()
        .configuration_json(serde_json::json!({"include_subgraph_errors": { "all": true }, "experimental_mock_subgraphs": subgraphs.clone() }))
        .unwrap()
        .schema(SCHEMA)
        .extra_private_plugin(response_cache.clone())
        .build_supergraph()
        .await
        .unwrap();

    let request = supergraph::Request::fake_builder()
        .query(query)
        .context(Context::new())
        .header(
            HeaderName::from_static(CACHE_DEBUG_HEADER_NAME),
            HeaderValue::from_static("true"),
        )
        .build()
        .unwrap();
    let mut response = service.clone().oneshot(request).await.unwrap();
    let mut cache_keys: CacheKeysContext = response
        .context
        .get(CONTEXT_DEBUG_CACHE_KEYS)
        .unwrap()
        .unwrap();
    cache_keys.iter_mut().for_each(|ck| {
        ck.invalidation_keys.sort();
        ck.cache_control.created = 0;
    });
    cache_keys.sort_by(|a, b| a.invalidation_keys.cmp(&b.invalidation_keys));
    insta::assert_json_snapshot!(cache_keys);
    assert!(
        response
            .response
            .headers()
            .get(CACHE_CONTROL)
            .and_then(|h| h.to_str().ok())
            .unwrap()
            .contains("max-age="),
    );
    assert!(
        response
            .response
            .headers()
            .get(CACHE_CONTROL)
            .and_then(|h| h.to_str().ok())
            .unwrap()
            .contains(",public"),
    );
    let mut response = response.next_response().await.unwrap();
    assert!(
        response
            .extensions
            .remove(CACHE_DEBUG_EXTENSIONS_KEY)
            .is_some()
    );

    insta::assert_json_snapshot!(response, @r###"
    {
      "data": {
        "currentUser": {
          "activeOrganization": {
            "id": "1",
            "creatorUser": {
              "__typename": "User",
              "id": 2
            }
          }
        }
      }
    }
    "###);

    // now we invalidate data
    let res = invalidation
        .invalidate(vec![InvalidationRequest::CacheTag {
            subgraphs: vec!["orga".to_string()].into_iter().collect(),
            cache_tag: String::from("organization-1"),
        }])
        .await
        .unwrap();
    assert_eq!(res, 1);

    let service = TestHarness::builder()
        .configuration_json(serde_json::json!({"include_subgraph_errors": { "all": true }, "experimental_mock_subgraphs": subgraphs.clone() }))
        .unwrap()
        .schema(SCHEMA)
        .extra_private_plugin(response_cache)
        .build_supergraph()
        .await
        .unwrap();

    let request = supergraph::Request::fake_builder()
        .query(query)
        .context(Context::new())
        .header(
            HeaderName::from_static(CACHE_DEBUG_HEADER_NAME),
            HeaderValue::from_static("true"),
        )
        .build()
        .unwrap();
    let mut response = service.clone().oneshot(request).await.unwrap();
    let mut cache_keys: CacheKeysContext = response
        .context
        .get(CONTEXT_DEBUG_CACHE_KEYS)
        .unwrap()
        .unwrap();
    cache_keys.iter_mut().for_each(|ck| {
        ck.invalidation_keys.sort();
        ck.cache_control.created = 0;
    });
    cache_keys.sort_by(|a, b| a.invalidation_keys.cmp(&b.invalidation_keys));
    insta::assert_json_snapshot!(cache_keys);
    assert!(
        response
            .response
            .headers()
            .get(CACHE_CONTROL)
            .and_then(|h| h.to_str().ok())
            .unwrap()
            .contains("max-age="),
    );
    assert!(
        response
            .response
            .headers()
            .get(CACHE_CONTROL)
            .and_then(|h| h.to_str().ok())
            .unwrap()
            .contains(",public"),
    );
    let mut response = response.next_response().await.unwrap();
    assert!(
        response
            .extensions
            .remove(CACHE_DEBUG_EXTENSIONS_KEY)
            .is_some()
    );

    insta::assert_json_snapshot!(response, @r###"
    {
      "data": {
        "currentUser": {
          "activeOrganization": {
            "id": "1",
            "creatorUser": {
              "__typename": "User",
              "id": 2
            }
          }
        }
      }
    }
    "###);
}

#[tokio::test(flavor = "multi_thread")]
async fn interval_cleanup_config() {
    let valid_schema = Arc::new(Schema::parse_and_validate(SCHEMA, "test.graphql").unwrap());

    let pg_cache = PostgresCacheStorage::new(&PostgresCacheConfig {
        cleanup_interval: std::time::Duration::from_secs(60 * 7), // Every 7 minutes
        url: "postgres://127.0.0.1".parse().unwrap(),
        username: None,
        password: None,
        timeout: Some(std::time::Duration::from_secs(5)),
        required_to_start: true,
        pool_size: default_pool_size(),
        batch_size: default_batch_size(),
        namespace: Some(String::from("interval_cleanup_config_1")),
    })
    .await
    .unwrap();
    let _response_cache = ResponseCache::for_test(
        pg_cache.clone(),
        Default::default(),
        valid_schema.clone(),
        true,
        true,
    )
    .await
    .unwrap();

    let cron = pg_cache.get_cron().await.unwrap();
    assert_eq!(cron.0, String::from("*/7 * * * *"));

    let pg_cache = PostgresCacheStorage::new(&PostgresCacheConfig {
        cleanup_interval: std::time::Duration::from_secs(60 * 60 * 7), // Every 7 hours
        url: "postgres://127.0.0.1".parse().unwrap(),
        username: None,
        password: None,
        timeout: Some(std::time::Duration::from_secs(5)),
        required_to_start: true,
        pool_size: default_pool_size(),
        batch_size: default_batch_size(),
        namespace: Some(String::from("interval_cleanup_config_2")),
    })
    .await
    .unwrap();
    let _response_cache = ResponseCache::for_test(
        pg_cache.clone(),
        Default::default(),
        valid_schema.clone(),
        true,
        true,
    )
    .await
    .unwrap();

    let cron = pg_cache.get_cron().await.unwrap();
    assert_eq!(cron.0, String::from("0 */7 * * *"));

    let pg_cache = PostgresCacheStorage::new(&PostgresCacheConfig {
        cleanup_interval: std::time::Duration::from_secs(60 * 60 * 24 * 7), // Every 7 days
        url: "postgres://127.0.0.1".parse().unwrap(),
        username: None,
        password: None,
        timeout: Some(std::time::Duration::from_secs(5)),
        required_to_start: true,
        pool_size: default_pool_size(),
        batch_size: default_batch_size(),
        namespace: Some(String::from("interval_cleanup_config_2")),
    })
    .await
    .unwrap();
    let _response_cache = ResponseCache::for_test(
        pg_cache.clone(),
        Default::default(),
        valid_schema.clone(),
        true,
        true,
    )
    .await
    .unwrap();

    let cron = pg_cache.get_cron().await.unwrap();
    assert_eq!(cron.0, String::from("0 0 */7 * *"));
}
