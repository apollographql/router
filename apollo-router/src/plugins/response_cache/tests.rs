use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use std::time::Instant;

use apollo_compiler::Schema;
use http::HeaderName;
use http::HeaderValue;
use http::header::CACHE_CONTROL;
use tokio::time::sleep;
use tower::Service;
use tower::ServiceExt;

use super::plugin::ResponseCache;
use crate::Context;
use crate::MockedSubgraphs;
use crate::TestHarness;
use crate::graphql::Response;
use crate::metrics::FutureMetricsExt;
use crate::plugin::test::MockSubgraph;
use crate::plugin::test::MockSubgraphService;
use crate::plugins::response_cache::invalidation::InvalidationRequest;
use crate::plugins::response_cache::plugin::CACHE_DEBUG_EXTENSIONS_KEY;
use crate::plugins::response_cache::plugin::CACHE_DEBUG_HEADER_NAME;
use crate::plugins::response_cache::plugin::CONTEXT_DEBUG_CACHE_KEYS;
use crate::plugins::response_cache::plugin::CacheKeysContext;
use crate::plugins::response_cache::plugin::Subgraph;
use crate::plugins::response_cache::storage::CacheStorage;
use crate::plugins::response_cache::storage::redis::Config as RedisCacheConfig;
use crate::plugins::response_cache::storage::redis::Storage as RedisCacheStorage;
use crate::plugins::response_cache::storage::redis::default_redis_cache_config;
use crate::services::subgraph;
use crate::services::supergraph;

const SCHEMA: &str = include_str!("../../testdata/orga_supergraph_cache_key.graphql");
const SCHEMA_REQUIRES: &str = include_str!("../../testdata/supergraph_cache_key.graphql");
const SCHEMA_NESTED_KEYS: &str =
    include_str!("../../testdata/supergraph_nested_fields_cache_key.graphql");

/// Cache inserts happen asynchronously, so there's no way to wait for a cache insert based on the
/// `TestHarness` service return value.
///
/// Instead, we wait for up to 5 seconds for the keys we expected to be present in Redis.
/// TODO: it might be useful to panic here if the loop terminates?
async fn wait_for_cache(cache: &RedisCacheStorage, keys: Vec<String>) {
    if keys.is_empty() {
        return;
    }

    let keys_strs: Vec<&str> = keys.iter().map(|s| s.as_str()).collect();
    dbg!(&keys_strs);

    let now = Instant::now();
    while now.elapsed() < Duration::from_secs(5) {
        let fetch_result = cache.get_multiple(&keys_strs).await;
        dbg!(&fetch_result);
        if let Ok(values) = fetch_result
            && values.into_iter().all(|v| v.is_some())
        {
            return;
        }

        sleep(Duration::from_millis(10)).await;
    }

    dbg!("insert not complete");
}

/// Extracts a list of cache keys from `CacheKeysContext` that we expect to be cached. This is
/// mostly used in `wait_for_cache_population`.
///
/// NB: this is not always accurate! For example, a key might not be stored if it's private but
/// wasn't passed the private ID. But it's a good approximation for most test cases.
fn expected_cached_keys(cache_keys_context: &CacheKeysContext) -> Vec<String> {
    cache_keys_context
        .iter()
        .filter(|context| context.cache_control.max_age.is_some())
        .filter(|context| !context.cache_control.no_store)
        .map(|context| context.key.clone())
        .collect()
}

/// Extract `CacheKeysContext` from `supergraph::Response` and prepare it for a snapshot, sorting
/// the invalidation keys and setting `created` to zero.
fn get_cache_keys_context(response: &supergraph::Response) -> Option<CacheKeysContext> {
    let mut cache_keys: CacheKeysContext =
        response.context.get(CONTEXT_DEBUG_CACHE_KEYS).ok()??;
    cache_keys.iter_mut().for_each(|ck| {
        ck.invalidation_keys.sort();
        ck.cache_control.created = 0;
    });
    cache_keys.sort_by(|a, b| a.invalidation_keys.cmp(&b.invalidation_keys));
    Some(cache_keys)
}

fn get_cache_control_header(response: &supergraph::Response) -> Option<Vec<String>> {
    Some(
        response
            .response
            .headers()
            .get(CACHE_CONTROL)?
            .to_str()
            .ok()?
            .split(',')
            .map(ToString::to_string)
            .collect(),
    )
}

fn cache_control_contains_no_store(cache_control_header: &[String]) -> bool {
    cache_control_header.iter().any(|h| h == "no-store")
}

fn cache_control_contains_public(cache_control_header: &[String]) -> bool {
    cache_control_header.iter().any(|h| h == "public")
}

fn cache_control_contains_private(cache_control_header: &[String]) -> bool {
    cache_control_header.iter().any(|h| h == "private")
}

fn cache_control_contains_max_age(cache_control_header: &[String]) -> bool {
    cache_control_header
        .iter()
        .any(|h| h.starts_with("max-age="))
}

/// Removes `CACHE_DEBUG_EXTENSIONS_KEY` to avoid messing up snapshots. Returns true to indicate
/// that the key was present.
fn remove_debug_extensions_key(response: &mut Response) -> bool {
    response
        .extensions
        .remove(CACHE_DEBUG_EXTENSIONS_KEY)
        .is_some()
}

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

    let config = RedisCacheConfig {
        namespace: Some(String::from("insert")),
        ..default_redis_cache_config()
    };
    let cache = RedisCacheStorage::new(&config).await.unwrap();

    let map = [
        (
            "user".to_string(),
            Subgraph {
                redis: None,
                private_id: Some("sub".to_string()),
                enabled: true.into(),
                ttl: None,
                ..Default::default()
            },
        ),
        (
            "orga".to_string(),
            Subgraph {
                redis: None,
                private_id: Some("sub".to_string()),
                enabled: true.into(),
                ttl: None,
                ..Default::default()
            },
        ),
    ]
    .into_iter()
    .collect();
    let response_cache = ResponseCache::for_test(cache.clone(), map, valid_schema.clone(), true)
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
    let cache_keys_context = get_cache_keys_context(&response).unwrap();
    insta::with_settings!({
        description => "Make sure everything is in status 'new' and we have all the entities and root fields"
    }, {
        insta::assert_json_snapshot!(cache_keys_context);
    });
    wait_for_cache(&cache, expected_cached_keys(&cache_keys_context)).await;

    let cache_control_header = get_cache_control_header(&response).unwrap();
    assert!(cache_control_contains_max_age(&cache_control_header));
    assert!(cache_control_contains_public(&cache_control_header));

    let mut response = response.next_response().await.unwrap();
    assert!(remove_debug_extensions_key(&mut response));
    insta::assert_json_snapshot!(response, @r#"
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
    "#);

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

    let cache_control_header = get_cache_control_header(&response).unwrap();
    assert!(cache_control_contains_max_age(&cache_control_header));
    assert!(cache_control_contains_public(&cache_control_header));

    let cache_keys_context = get_cache_keys_context(&response).unwrap();
    insta::with_settings!({
        description => "Make sure everything is in status 'cached' and we have all the entities and root fields"
    }, {
        insta::assert_json_snapshot!(cache_keys_context);
    });

    let mut response = response.next_response().await.unwrap();
    assert!(remove_debug_extensions_key(&mut response));
    insta::assert_json_snapshot!(response, @r#"
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
    "#);
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

    let namespace = Some(String::from("insert-without-debug-header"));
    let config = RedisCacheConfig {
        namespace,
        ..default_redis_cache_config()
    };
    let cache = RedisCacheStorage::new(&config).await.unwrap();

    let map = [
        (
            "user".to_string(),
            Subgraph {
                redis: None,
                private_id: Some("sub".to_string()),
                enabled: true.into(),
                ttl: None,
                ..Default::default()
            },
        ),
        (
            "orga".to_string(),
            Subgraph {
                redis: None,
                private_id: Some("sub".to_string()),
                enabled: true.into(),
                ttl: None,
                ..Default::default()
            },
        ),
    ]
    .into_iter()
    .collect();
    let response_cache = ResponseCache::for_test(cache, map, valid_schema.clone(), true)
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
    assert!(get_cache_keys_context(&response).is_none());

    let cache_control_header = get_cache_control_header(&response).unwrap();
    assert!(cache_control_contains_max_age(&cache_control_header));
    assert!(cache_control_contains_public(&cache_control_header));

    let mut response = response.next_response().await.unwrap();
    assert!(!remove_debug_extensions_key(&mut response));
    insta::assert_json_snapshot!(response, @r#"
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
    "#);

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

    let cache_control_header = get_cache_control_header(&response).unwrap();
    assert!(cache_control_contains_max_age(&cache_control_header));
    assert!(cache_control_contains_public(&cache_control_header));

    assert!(get_cache_keys_context(&response).is_none());

    let mut response = response.next_response().await.unwrap();
    assert!(!remove_debug_extensions_key(&mut response));
    insta::assert_json_snapshot!(response, @r#"
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
    "#);
}

#[tokio::test]
async fn insert_with_requires() {
    let valid_schema =
        Arc::new(Schema::parse_and_validate(SCHEMA_REQUIRES, "test.graphql").unwrap());
    let query = "query { topProducts { name shippingEstimate price } }";

    let subgraphs = MockedSubgraphs([
        ("products", MockSubgraph::builder().with_json(
            serde_json::json! {{"query":"{ topProducts { __typename upc name price weight } }"}},
            serde_json::json! {{"data": {"topProducts": [{
                    "__typename": "Product",
                    "upc": "1",
                    "name": "Test",
                    "price": 150,
                    "weight": 5
                }]}}},
        ).with_header(CACHE_CONTROL, HeaderValue::from_static("public")).build()),
        ("inventory", MockSubgraph::builder().with_json(
            serde_json::json! {{
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
            serde_json::json! {{"data": {
                "_entities": [{
                    "shippingEstimate": 15
                }]
            }}},
        ).with_header(CACHE_CONTROL, HeaderValue::from_static("public")).build())
    ].into_iter().collect());

    let namespace = Some(String::from("insert-with-requires"));
    let config = RedisCacheConfig {
        namespace,
        ..default_redis_cache_config()
    };
    let cache = RedisCacheStorage::new(&config).await.unwrap();

    let map: HashMap<String, Subgraph> = [
        (
            "products".to_string(),
            Subgraph {
                redis: None,
                private_id: Some("sub".to_string()),
                enabled: true.into(),
                ttl: None,
                ..Default::default()
            },
        ),
        (
            "inventory".to_string(),
            Subgraph {
                redis: None,
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
        ResponseCache::for_test(cache.clone(), map.clone(), valid_schema.clone(), true)
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
    let cache_keys_context = get_cache_keys_context(&response).unwrap();
    insta::with_settings!({
        description => "Make sure everything is in status 'new' and we have all the entities and root fields"
    }, {
        insta::assert_json_snapshot!(cache_keys_context);
    });
    wait_for_cache(&cache, expected_cached_keys(&cache_keys_context)).await;

    let cache_control_header = get_cache_control_header(&response).unwrap();
    assert!(cache_control_contains_max_age(&cache_control_header));
    assert!(cache_control_contains_public(&cache_control_header));

    let mut response = response.next_response().await.unwrap();
    assert!(remove_debug_extensions_key(&mut response));

    insta::assert_json_snapshot!(response, @r#"
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
    "#);

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
    let cache_keys_context = get_cache_keys_context(&response).unwrap();
    insta::with_settings!({
        description => "Make sure everything is in status 'cached' and we have all the entities and root fields"
    }, {
        insta::assert_json_snapshot!(cache_keys_context);
    });

    let cache_control_header = get_cache_control_header(&response).unwrap();
    assert!(cache_control_contains_max_age(&cache_control_header));
    assert!(cache_control_contains_public(&cache_control_header));

    let mut response = response.next_response().await.unwrap();
    assert!(remove_debug_extensions_key(&mut response));

    insta::assert_json_snapshot!(response, @r#"
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
    "#);
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

    let namespace = Some(String::from("insert-with-nested-field-set"));
    let config = RedisCacheConfig {
        namespace,
        ..default_redis_cache_config()
    };
    let cache = RedisCacheStorage::new(&config).await.unwrap();

    let map = [
        (
            "products".to_string(),
            Subgraph {
                redis: None,
                private_id: Some("sub".to_string()),
                enabled: true.into(),
                ttl: None,
                ..Default::default()
            },
        ),
        (
            "users".to_string(),
            Subgraph {
                redis: None,
                private_id: Some("sub".to_string()),
                enabled: true.into(),
                ttl: None,
                ..Default::default()
            },
        ),
    ]
    .into_iter()
    .collect();
    let response_cache = ResponseCache::for_test(cache.clone(), map, valid_schema.clone(), true)
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
    let cache_keys_context = get_cache_keys_context(&response).unwrap();
    insta::with_settings!({
        description => "Make sure everything is in status 'new' and we have all the entities and root fields"
    }, {
        insta::assert_json_snapshot!(cache_keys_context);
    });
    wait_for_cache(&cache, expected_cached_keys(&cache_keys_context)).await;

    let cache_control_header = get_cache_control_header(&response).unwrap();
    assert!(cache_control_contains_max_age(&cache_control_header));
    assert!(cache_control_contains_public(&cache_control_header));

    let mut response = response.next_response().await.unwrap();
    assert!(remove_debug_extensions_key(&mut response));

    insta::assert_json_snapshot!(response, @r#"
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
    "#);

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

    let cache_control_header = get_cache_control_header(&response).unwrap();
    assert!(cache_control_contains_max_age(&cache_control_header));
    assert!(cache_control_contains_public(&cache_control_header));

    let cache_keys_context = get_cache_keys_context(&response).unwrap();
    insta::with_settings!({
        description => "Make sure everything is in status 'cached' and we have all the entities and root fields"
    }, {
        insta::assert_json_snapshot!(cache_keys_context);
    });

    let mut response = response.next_response().await.unwrap();
    assert!(remove_debug_extensions_key(&mut response));

    insta::assert_json_snapshot!(response, @r#"
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
    "#);
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

    let namespace = Some(String::from("no-cache-control"));
    let config = RedisCacheConfig {
        namespace,
        ..default_redis_cache_config()
    };
    let cache = RedisCacheStorage::new(&config).await.unwrap();

    let response_cache =
        ResponseCache::for_test(cache.clone(), HashMap::new(), valid_schema.clone(), true)
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

    let cache_control_header = get_cache_control_header(&response).unwrap();
    assert!(cache_control_contains_no_store(&cache_control_header));

    let mut response = response.next_response().await.unwrap();
    assert!(remove_debug_extensions_key(&mut response));

    insta::assert_json_snapshot!(response, @r#"
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
    "#);

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

    let cache_control_header = get_cache_control_header(&response).unwrap();
    assert!(cache_control_contains_no_store(&cache_control_header));

    let mut response = response.next_response().await.unwrap();
    assert!(remove_debug_extensions_key(&mut response));

    insta::assert_json_snapshot!(response, @r#"
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
    "#);
}

#[tokio::test]
async fn no_store_from_request() {
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

    let namespace = Some(String::from("no-store-from-request"));
    let config = RedisCacheConfig {
        namespace,
        ..default_redis_cache_config()
    };
    let cache = RedisCacheStorage::new(&config).await.unwrap();

    let response_cache =
        ResponseCache::for_test(cache.clone(), HashMap::new(), valid_schema.clone(), true)
            .await
            .unwrap();

    let service = TestHarness::builder()
        .configuration_json(serde_json::json!({"include_subgraph_errors": { "all": true }, "experimental_mock_subgraphs": subgraphs.clone(), "headers": {
            "all": {
                "request": [{
                    "propagate": {
                        "named": "cache-control"
                    }
                }]
            }
        } }))
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
        .header(CACHE_CONTROL, HeaderValue::from_static("no-store"))
        .build()
        .unwrap();
    let mut response = service.oneshot(request).await.unwrap();

    let cache_control_header = get_cache_control_header(&response).unwrap();
    assert!(cache_control_contains_no_store(&cache_control_header));

    let response = response.next_response().await.unwrap();

    insta::assert_json_snapshot!(response, @r#"
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
    "#);

    // Just to make sure it doesn't invalidate anything, which means nothing has been stored
    let invalidation_result: HashMap<String, u64> = cache
        .invalidate(
            vec![
                "user".to_string(),
                "organization".to_string(),
                "currentUser".to_string(),
            ],
            vec!["orga".to_string(), "user".to_string()],
        )
        .await
        .unwrap();
    let entries_invalidated: u64 = invalidation_result.into_values().sum();
    assert_eq!(entries_invalidated, 0);

    let service = TestHarness::builder()
        .configuration_json(serde_json::json!({"include_subgraph_errors": { "all": true }, "experimental_mock_subgraphs": subgraphs.clone(), "headers": {
            "all": {
                "request": [{
                    "propagate": {
                        "named": "cache-control"
                    }
                }]
            }
        } }))
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
        .header(CACHE_CONTROL, HeaderValue::from_static("no-store"))
        .build()
        .unwrap();
    let mut response = service.oneshot(request).await.unwrap();

    let cache_control_header = get_cache_control_header(&response).unwrap();
    assert!(cache_control_contains_no_store(&cache_control_header));

    let response = response.next_response().await.unwrap();

    insta::assert_json_snapshot!(response, @r#"
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
    "#);

    // Just to make sure it doesn't invalidate anything, which means nothing has been stored
    let invalidation_result: HashMap<String, u64> = cache
        .invalidate(
            vec![
                "user".to_string(),
                "organization".to_string(),
                "currentUser".to_string(),
            ],
            vec!["orga".to_string(), "user".to_string()],
        )
        .await
        .unwrap();
    let entries_invalidated: u64 = invalidation_result.into_values().sum();
    assert_eq!(entries_invalidated, 0);
}

#[tokio::test]
async fn private_only() {
    async {
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

        let namespace = Some(String::from("private-only"));
        let config = RedisCacheConfig {
            namespace,
            ..default_redis_cache_config()
        };
        let cache = RedisCacheStorage::new(&config).await.unwrap();

        let map = [
            (
                "user".to_string(),
                Subgraph {
                    redis: None,
                    private_id: Some("sub".to_string()),
                    enabled: true.into(),
                    ttl: None,
                    ..Default::default()
                },
            ),
            (
                "orga".to_string(),
                Subgraph {
                    redis: None,
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
            ResponseCache::for_test(cache, map, valid_schema.clone(), true)
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
        let cache_keys_context = get_cache_keys_context(&response).unwrap();
        insta::assert_json_snapshot!(cache_keys_context);

        assert_gauge!("apollo.router.response_cache.private_queries.lru.size", 1);

        let mut response = response.next_response().await.unwrap();
        assert!(remove_debug_extensions_key(&mut response));
        insta::assert_json_snapshot!(response, @r#"
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
        "#);

        // wait for key to be in the cache
        let cache = RedisCacheStorage::new(&config).await.unwrap();
        wait_for_cache(&cache, expected_cached_keys(&cache_keys_context)).await;

        // First request with only private response cache-control
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
        let cache_control_header = get_cache_control_header(&response).unwrap();
        assert!(cache_control_contains_private(&cache_control_header));

        let cache_keys_context = get_cache_keys_context(&response).unwrap();
        insta::assert_json_snapshot!(cache_keys_context);

        let mut response = response.next_response().await.unwrap();
        assert!(remove_debug_extensions_key(&mut response));

        insta::assert_json_snapshot!(response, @r#"
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
        "#);
        wait_for_cache(&cache, expected_cached_keys(&cache_keys_context)).await;

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
        let cache_control_header = get_cache_control_header(&response).unwrap();
        assert!(cache_control_contains_private(&cache_control_header));

        let cache_keys_context = get_cache_keys_context(&response).unwrap();
        insta::assert_json_snapshot!(cache_keys_context);
        wait_for_cache(&cache, expected_cached_keys(&cache_keys_context)).await;

        let mut response = response.next_response().await.unwrap();
        assert!(remove_debug_extensions_key(&mut response));

        insta::assert_json_snapshot!(response, @r#"
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
        "#);
    }.with_metrics().await;
}

// In this test we want to make sure when we have 2 root fields with both public and private data it still returns private
#[tokio::test]
async fn private_and_public() {
    let query = "query { currentUser { activeOrganization { id creatorUser { __typename id } } } orga(id: \"2\") { name } }";
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
            "headers": {"cache-control": "public"},
        },
        "orga": {
            "query": {
              "orga": {
                  "__typename": "Organization",
                  "id": "2",
                  "name": "test_orga"
              }
            },
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

    let namespace = Some(String::from("private-and-public"));
    let config = RedisCacheConfig {
        namespace,
        ..default_redis_cache_config()
    };
    let cache = RedisCacheStorage::new(&config).await.unwrap();
    let map = [
        (
            "user".to_string(),
            Subgraph {
                redis: None,
                private_id: Some("sub".to_string()),
                enabled: true.into(),
                ttl: None,
                ..Default::default()
            },
        ),
        (
            "orga".to_string(),
            Subgraph {
                redis: None,
                private_id: Some("sub".to_string()),
                enabled: true.into(),
                ttl: None,
                ..Default::default()
            },
        ),
    ]
    .into_iter()
    .collect();
    let response_cache = ResponseCache::for_test(cache.clone(), map, valid_schema.clone(), true)
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
    let cache_keys_context = get_cache_keys_context(&response).unwrap();
    insta::assert_json_snapshot!(cache_keys_context);

    let mut response = response.next_response().await.unwrap();
    assert!(remove_debug_extensions_key(&mut response));
    insta::assert_json_snapshot!(response, @r#"
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
        },
        "orga": {
          "name": "test_orga"
        }
      }
    }
    "#);

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
    let cache_control_header = get_cache_control_header(&response).unwrap();
    assert!(cache_control_contains_private(&cache_control_header));

    let cache_keys_context = get_cache_keys_context(&response).unwrap();
    insta::assert_json_snapshot!(cache_keys_context);
    wait_for_cache(&cache, expected_cached_keys(&cache_keys_context)).await;

    let mut response = response.next_response().await.unwrap();
    assert!(remove_debug_extensions_key(&mut response));

    insta::assert_json_snapshot!(response, @r#"
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
        },
        "orga": {
          "name": "test_orga"
        }
      }
    }
    "#);

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
    let cache_control_header = get_cache_control_header(&response).unwrap();
    assert!(cache_control_contains_private(&cache_control_header));

    let cache_keys_context = get_cache_keys_context(&response).unwrap();
    insta::assert_json_snapshot!(cache_keys_context);

    let mut response = response.next_response().await.unwrap();
    assert!(remove_debug_extensions_key(&mut response));

    insta::assert_json_snapshot!(response, @r#"
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
        },
        "orga": {
          "name": "test_orga"
        }
      }
    }
    "#);
}

// In this test we want to make sure when we have a subgraph query that could be either public or private depending of private_id it still works
#[tokio::test]
async fn polymorphic_private_and_public() {
    async {
        let query = "query { currentUser { activeOrganization { id creatorUser { __typename id } } } orga(id: \"2\") { name } }";
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
                "headers": {"cache-control": "public"},
            },
            "orga": {
                "query": {
                "orga": {
                    "__typename": "Organization",
                    "id": "2",
                    "name": "test_orga"
                }
                },
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

        let namespace = Some(String::from("polymorphic-private-and-public"));
        let config = RedisCacheConfig {
            namespace,
            ..default_redis_cache_config()
        };
        let cache = RedisCacheStorage::new(&config).await.unwrap();

        let map = [
            (
                "user".to_string(),
                Subgraph {
                    redis: None,
                    private_id: Some("sub".to_string()),
                    enabled: true.into(),
                    ttl: None,
                    ..Default::default()
                },
            ),
            (
                "orga".to_string(),
                Subgraph {
                    redis: None,
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
            ResponseCache::for_test(cache.clone(), map, valid_schema.clone(), true)
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
        let cache_keys_context = get_cache_keys_context(&response).unwrap();
        insta::with_settings!({
            description => "Make sure everything is in status 'new' and we have all the entities and root fields"
        }, {
            insta::assert_json_snapshot!(cache_keys_context);
        });
        wait_for_cache(&cache, expected_cached_keys(&cache_keys_context)).await;

        let mut response = response.next_response().await.unwrap();
        assert!(remove_debug_extensions_key(&mut response));
        insta::assert_json_snapshot!(response, @r#"
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
            },
            "orga": {
              "name": "test_orga"
            }
          }
        }
        "#);

        let subgraphs_public = serde_json::json!({
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
                "query": {
                "orga": {
                    "__typename": "Organization",
                    "id": "2",
                    "name": "test_orga_public"
                }
                },
                "entities": [
                    {
                        "__typename": "Organization",
                        "id": "1",
                        "creatorUser": {
                            "__typename": "User",
                            "id": 3
                        }
                    }
                ],
                "headers": {"cache-control": "public"},
            },
        });

        let mut service = TestHarness::builder()
            .configuration_json(serde_json::json!({"include_subgraph_errors": { "all": true }, "experimental_mock_subgraphs": subgraphs_public.clone() }))
            .unwrap()
            .schema(SCHEMA)
            .extra_private_plugin(response_cache.clone())
            .build_supergraph()
            .await
            .unwrap();

        let context = Context::new();

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
        let cache_control_header = get_cache_control_header(&response).unwrap();
        assert!(cache_control_contains_public(&cache_control_header));

        let cache_keys_context = get_cache_keys_context(&response).unwrap();
        insta::assert_json_snapshot!(cache_keys_context);
        wait_for_cache(&cache, expected_cached_keys(&cache_keys_context)).await;

        let mut response = response.next_response().await.unwrap();
        assert!(remove_debug_extensions_key(&mut response));

        insta::assert_json_snapshot!(response, @r#"
        {
          "data": {
            "currentUser": {
              "activeOrganization": {
                "id": "1",
                "creatorUser": {
                  "__typename": "User",
                  "id": 3
                }
              }
            },
            "orga": {
              "name": "test_orga_public"
            }
          }
        }
        "#);

        // Put back private cache-control to check it's still in cache
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
        let cache_control_header = get_cache_control_header(&response).unwrap();
        assert!(cache_control_contains_private(&cache_control_header));
        let cache_keys_context = get_cache_keys_context(&response).unwrap();
        insta::assert_json_snapshot!(cache_keys_context);
        wait_for_cache(&cache, expected_cached_keys(&cache_keys_context)).await;

        let mut response = response.next_response().await.unwrap();
        assert!(remove_debug_extensions_key(&mut response));

        insta::assert_json_snapshot!(response, @r#"
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
            },
            "orga": {
              "name": "test_orga"
            }
          }
        }
        "#);

        // Test again with subgraph public to make sure it's still cached
        let mut service = TestHarness::builder()
            .configuration_json(serde_json::json!({"include_subgraph_errors": { "all": true }, "experimental_mock_subgraphs": subgraphs_public.clone() }))
            .unwrap()
            .schema(SCHEMA)
            .extra_private_plugin(response_cache.clone())
            .build_supergraph()
            .await
            .unwrap();

        let context = Context::new();
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
        let cache_control_header = get_cache_control_header(&response).unwrap();
        assert!(cache_control_contains_public(&cache_control_header));
        let cache_keys_context = get_cache_keys_context(&response).unwrap();
        insta::assert_json_snapshot!(cache_keys_context);
        wait_for_cache(&cache, expected_cached_keys(&cache_keys_context)).await;

        let mut response = response.next_response().await.unwrap();
        assert!(remove_debug_extensions_key(&mut response));

        insta::assert_json_snapshot!(response, @r#"
        {
          "data": {
            "currentUser": {
              "activeOrganization": {
                "id": "1",
                "creatorUser": {
                  "__typename": "User",
                  "id": 3
                }
              }
            },
            "orga": {
              "name": "test_orga_public"
            }
          }
        }
        "#);
        assert_gauge!("apollo.router.response_cache.private_queries.lru.size", 1);

        // Test again with public subgraph but with a private_id set, it should be private because this query is private once we have private_id set, even if the subgraph is public, it's coming from the cache
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
        let cache_control_header = get_cache_control_header(&response).unwrap();
        assert!(cache_control_contains_private(&cache_control_header));
        let cache_keys_context = get_cache_keys_context(&response).unwrap();
        insta::assert_json_snapshot!(cache_keys_context);
        wait_for_cache(&cache, expected_cached_keys(&cache_keys_context)).await;

        let mut response = response.next_response().await.unwrap();
        assert!(remove_debug_extensions_key(&mut response));

        insta::assert_json_snapshot!(response, @r#"
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
            },
            "orga": {
              "name": "test_orga"
            }
          }
        }
        "#);
        assert_gauge!("apollo.router.response_cache.private_queries.lru.size", 1);

        // Test again with private subgraph but without private_id set, it should give the public values because it's cached and it knows even if the subgraphs are private it was public without private_id
        let mut service = TestHarness::builder()
            .configuration_json(serde_json::json!({"include_subgraph_errors": { "all": true }, "experimental_mock_subgraphs": subgraphs.clone() }))
            .unwrap()
            .schema(SCHEMA)
            .extra_private_plugin(response_cache.clone())
            .build_supergraph()
            .await
            .unwrap();
        let context = Context::new();
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
        let cache_control_header = get_cache_control_header(&response).unwrap();
        assert!(cache_control_contains_public(&cache_control_header));
        let cache_keys_context = get_cache_keys_context(&response).unwrap();
        insta::assert_json_snapshot!(cache_keys_context);
        wait_for_cache(&cache, expected_cached_keys(&cache_keys_context)).await;

        let mut response = response.next_response().await.unwrap();
        assert!(remove_debug_extensions_key(&mut response));

        insta::assert_json_snapshot!(response, @r#"
        {
          "data": {
            "currentUser": {
              "activeOrganization": {
                "id": "1",
                "creatorUser": {
                  "__typename": "User",
                  "id": 3
                }
              }
            },
            "orga": {
              "name": "test_orga_public"
            }
          }
        }
        "#);
        assert_gauge!("apollo.router.response_cache.private_queries.lru.size", 1);
    }.with_metrics().await;
}

#[tokio::test]
async fn private_without_private_id() {
    async {
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

        let namespace = Some(String::from("private-without-private-id"));
        let config = RedisCacheConfig {
            namespace,
            ..default_redis_cache_config()
        };
        let cache = RedisCacheStorage::new(&config).await.unwrap();

        let map = [
            (
                "user".to_string(),
                Subgraph {
                    redis: None,
                    enabled: true.into(),
                    ttl: None,
                    ..Default::default()
                },
            ),
            (
                "orga".to_string(),
                Subgraph {
                    redis: None,
                    enabled: true.into(),
                    ttl: None,
                    ..Default::default()
                },
            ),
        ]
            .into_iter()
            .collect();
        let response_cache =
            ResponseCache::for_test(cache.clone(), map, valid_schema.clone(), true)
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
        let cache_control_header = get_cache_control_header(&response).unwrap();
        assert!(cache_control_contains_private(&cache_control_header));
        let cache_keys_context = get_cache_keys_context(&response).unwrap();
        insta::assert_json_snapshot!(cache_keys_context);

        assert_gauge!("apollo.router.response_cache.private_queries.lru.size", 1);

        let mut response = response.next_response().await.unwrap();
        assert!(remove_debug_extensions_key(&mut response));
        insta::assert_json_snapshot!(response, @r#"
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
        "#);
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
        let cache_control_header = get_cache_control_header(&response).unwrap();
        assert!(cache_control_contains_private(&cache_control_header));
        let cache_keys_context = get_cache_keys_context(&response).unwrap();
        insta::assert_json_snapshot!(cache_keys_context);
        wait_for_cache(&cache, expected_cached_keys(&cache_keys_context)).await;

        let mut response = response.next_response().await.unwrap();
        assert!(remove_debug_extensions_key(&mut response));

        insta::assert_json_snapshot!(response, @r#"
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
        "#);
    }.with_metrics().await;
}

#[tokio::test]
async fn no_data() {
    let query = "query { currentUser { allOrganizations { id name } } }";
    let valid_schema = Arc::new(Schema::parse_and_validate(SCHEMA, "test.graphql").unwrap());

    let subgraphs = MockedSubgraphs([
        ("user", MockSubgraph::builder().with_json(
            serde_json::json! {{"query":"{currentUser{allOrganizations{__typename id}}}"}},
            serde_json::json! {{"data": {"currentUser": { "allOrganizations": [
                    {
                        "__typename": "Organization",
                        "id": "1"
                    },
                    {
                        "__typename": "Organization",
                        "id": "3"
                    }
                ] }}}},
        ).with_header(CACHE_CONTROL, HeaderValue::from_static("no-store")).build()),
        ("orga", MockSubgraph::builder().with_json(
            serde_json::json! {{
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
            serde_json::json! {{
                "data": {
                    "_entities": [{
                    "name": "Organization 1",
                },
                {
                    "name": "Organization 3"
                }]
            }
            }},
        ).with_header(CACHE_CONTROL, HeaderValue::from_static("public, max-age=3600")).build())
    ].into_iter().collect());

    let namespace = Some(String::from("no-data"));
    let config = RedisCacheConfig {
        namespace,
        ..default_redis_cache_config()
    };
    let cache = RedisCacheStorage::new(&config).await.unwrap();

    let map = [
        (
            "user".to_string(),
            Subgraph {
                redis: None,
                private_id: Some("sub".to_string()),
                enabled: true.into(),
                ttl: None,
                ..Default::default()
            },
        ),
        (
            "orga".to_string(),
            Subgraph {
                redis: None,
                private_id: Some("sub".to_string()),
                enabled: true.into(),
                ttl: None,
                ..Default::default()
            },
        ),
    ]
    .into_iter()
    .collect();
    let response_cache = ResponseCache::for_test(cache.clone(), map, valid_schema.clone(), true)
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

    let cache_keys_context = get_cache_keys_context(&response).unwrap();
    insta::assert_json_snapshot!(cache_keys_context, {
        "[].cache_control" => insta::dynamic_redaction(|value, _path| {
            let cache_control = value.as_str().unwrap().to_string();
            assert!(cache_control.contains("max-age="));
            assert!(cache_control.contains("public"));
            "[REDACTED]"
        })
    });
    wait_for_cache(&cache, expected_cached_keys(&cache_keys_context)).await;

    let mut response = response.next_response().await.unwrap();
    assert!(remove_debug_extensions_key(&mut response));

    insta::assert_json_snapshot!(response, @r#"
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
    "#);

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

    let cache_keys_context = get_cache_keys_context(&response).unwrap();
    insta::assert_json_snapshot!(cache_keys_context);
    wait_for_cache(&cache, expected_cached_keys(&cache_keys_context)).await;

    let mut response = response.next_response().await.unwrap();
    assert!(remove_debug_extensions_key(&mut response));

    insta::assert_json_snapshot!(response, @r#"
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
    "#);
}

#[tokio::test]
async fn missing_entities() {
    let query = "query { currentUser { allOrganizations { id name } } }";
    let valid_schema = Arc::new(Schema::parse_and_validate(SCHEMA, "test.graphql").unwrap());
    let subgraphs = MockedSubgraphs([
        ("user", MockSubgraph::builder().with_json(
            serde_json::json! {{"query":"{currentUser{allOrganizations{__typename id}}}"}},
            serde_json::json! {{"data": {"currentUser": { "allOrganizations": [
                    {
                        "__typename": "Organization",
                        "id": "1"
                    },
                    {
                        "__typename": "Organization",
                        "id": "2"
                    }
                ] }}}},
        ).with_header(CACHE_CONTROL, HeaderValue::from_static("no-store")).build()),
        ("orga", MockSubgraph::builder().with_json(
            serde_json::json! {{
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
            serde_json::json! {{
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
            }},
        ).with_header(CACHE_CONTROL, HeaderValue::from_static("public, max-age=3600")).build())
    ].into_iter().collect());

    let namespace = Some(String::from("missing-entities"));
    let config = RedisCacheConfig {
        namespace,
        ..default_redis_cache_config()
    };
    let cache = RedisCacheStorage::new(&config).await.unwrap();

    let map = [
        (
            "user".to_string(),
            Subgraph {
                redis: None,
                private_id: Some("sub".to_string()),
                enabled: true.into(),
                ttl: None,
                ..Default::default()
            },
        ),
        (
            "orga".to_string(),
            Subgraph {
                redis: None,
                private_id: Some("sub".to_string()),
                enabled: true.into(),
                ttl: None,
                ..Default::default()
            },
        ),
    ]
    .into_iter()
    .collect();
    let response_cache = ResponseCache::for_test(cache.clone(), map, valid_schema.clone(), true)
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
    let cache_keys_context = get_cache_keys_context(&response).unwrap();
    wait_for_cache(&cache, expected_cached_keys(&cache_keys_context)).await;

    let mut response = response.next_response().await.unwrap();
    assert!(remove_debug_extensions_key(&mut response));
    insta::assert_json_snapshot!(response);

    let response_cache =
        ResponseCache::for_test(cache.clone(), HashMap::new(), valid_schema.clone(), false)
            .await
            .unwrap();

    let subgraphs = MockedSubgraphs([
        ("user", MockSubgraph::builder().with_json(
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
        ).with_header(CACHE_CONTROL, HeaderValue::from_static("no-store")).build()),
        ("orga", MockSubgraph::builder().with_json(
            serde_json::json! {{
                    "query": "query($representations:[_Any!]!){_entities(representations:$representations){...on Organization{name}}}",
                "variables": {
                    "representations": [
                        {
                            "id": "3",
                            "__typename": "Organization",
                        }
                    ]
                }}},
            serde_json::json! {{
                    "data": null,
                    "errors": [{
                        "message": "Organization not found",
                    }]
                }},
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
    assert!(remove_debug_extensions_key(&mut response));

    insta::assert_json_snapshot!(response);
}

#[tokio::test(flavor = "multi_thread")]
async fn invalidate_by_cache_tag() {
    async move {
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

        let namespace = Some(String::from("invalidate-by-cache-tag"));
        let config = RedisCacheConfig {
            namespace,
            ..default_redis_cache_config()
        };
        let cache = RedisCacheStorage::new(&config).await.unwrap();

        let map = [
            (
                "user".to_string(),
                Subgraph {
                    redis: None,
                    private_id: Some("sub".to_string()),
                    enabled: true.into(),
                    ttl: None,
                    ..Default::default()
                },
            ),
            (
                "orga".to_string(),
                Subgraph {
                    redis: None,
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
            ResponseCache::for_test(cache.clone(), map, valid_schema.clone(), true)
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
        let cache_keys_context = get_cache_keys_context(&response).unwrap();
        insta::assert_json_snapshot!(cache_keys_context);
        wait_for_cache(&cache, expected_cached_keys(&cache_keys_context)).await;

        let cache_control_header = get_cache_control_header(&response).unwrap();
        assert!(cache_control_contains_max_age(&cache_control_header));
        assert!(cache_control_contains_public(&cache_control_header));

        let mut response = response.next_response().await.unwrap();
        assert!(remove_debug_extensions_key(&mut response));

        insta::assert_json_snapshot!(response, @r#"
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
        "#);
        assert_histogram_sum!("apollo.router.operations.response_cache.fetch.entity", 1u64, "subgraph.name" = "orga", "graphql.type" = "Organization");


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
        let cache_keys_context = get_cache_keys_context(&response).unwrap();
        insta::assert_json_snapshot!(cache_keys_context);
        wait_for_cache(&cache, expected_cached_keys(&cache_keys_context)).await;

        let cache_control_header = get_cache_control_header(&response).unwrap();
        assert!(cache_control_contains_max_age(&cache_control_header));
        assert!(cache_control_contains_public(&cache_control_header));

        let mut response = response.next_response().await.unwrap();
        assert!(remove_debug_extensions_key(&mut response));
        assert_histogram_sum!("apollo.router.operations.response_cache.fetch.entity", 2u64, "subgraph.name" = "orga", "graphql.type" = "Organization");

        insta::assert_json_snapshot!(response, @r#"
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
        "#);

        // now we invalidate data
        let res = invalidation
            .invalidate(vec![InvalidationRequest::CacheTag {
                subgraphs: vec!["orga".to_string()].into_iter().collect(),
                cache_tag: String::from("organization-1"),
            }])
            .await
            .unwrap();
        assert_eq!(res, 1);

        assert_counter!("apollo.router.operations.response_cache.invalidation.entry", 1u64, "subgraph.name" = "orga", "kind" = "cache_tag", "cache.tag" = "organization-1");

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
        let cache_keys_context = get_cache_keys_context(&response).unwrap();
        insta::assert_json_snapshot!(cache_keys_context);
        wait_for_cache(&cache, expected_cached_keys(&cache_keys_context)).await;

        let cache_control_header = get_cache_control_header(&response).unwrap();
        assert!(cache_control_contains_max_age(&cache_control_header));
        assert!(cache_control_contains_public(&cache_control_header));

        let mut response = response.next_response().await.unwrap();
        assert!(remove_debug_extensions_key(&mut response));

        insta::assert_json_snapshot!(response, @r#"
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
        "#);
        assert_histogram_sum!("apollo.router.operations.response_cache.fetch.entity", 3u64, "subgraph.name" = "orga", "graphql.type" = "Organization");
    }.with_metrics().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn invalidate_by_type() {
    async move {
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

        let namespace = Some(String::from("invalidate-by-type"));
        let config = RedisCacheConfig {
            namespace,
            ..default_redis_cache_config()
        };
        let cache = RedisCacheStorage::new(&config).await.unwrap();

        let map = [
            (
                "user".to_string(),
                Subgraph {
                    redis: None,
                    private_id: Some("sub".to_string()),
                    enabled: true.into(),
                    ttl: None,
                    ..Default::default()
                },
            ),
            (
                "orga".to_string(),
                Subgraph {
                    redis: None,
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
            ResponseCache::for_test(cache.clone(), map, valid_schema.clone(), true)
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
        let cache_keys_context = get_cache_keys_context(&response).unwrap();
        insta::assert_json_snapshot!(cache_keys_context);
        wait_for_cache(&cache, expected_cached_keys(&cache_keys_context)).await;

        let cache_control_header = get_cache_control_header(&response).unwrap();
        assert!(cache_control_contains_max_age(&cache_control_header));
        assert!(cache_control_contains_public(&cache_control_header));

        let mut response = response.next_response().await.unwrap();
        assert!(remove_debug_extensions_key(&mut response));

        insta::assert_json_snapshot!(response, @r#"
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
        "#);

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
        let cache_keys_context = get_cache_keys_context(&response).unwrap();
        insta::assert_json_snapshot!(cache_keys_context);
        wait_for_cache(&cache, expected_cached_keys(&cache_keys_context)).await;

        let cache_control_header = get_cache_control_header(&response).unwrap();
        assert!(cache_control_contains_max_age(&cache_control_header));
        assert!(cache_control_contains_public(&cache_control_header));

        let mut response = response.next_response().await.unwrap();
        assert!(remove_debug_extensions_key(&mut response));

        insta::assert_json_snapshot!(response, @r#"
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
        "#);

        // now we invalidate data
        let res = invalidation
            .invalidate(vec![InvalidationRequest::Type { subgraph: "orga".to_string(), r#type: "Organization".to_string() }])
            .await
            .unwrap();
        assert_eq!(res, 1);

        assert_counter!("apollo.router.operations.response_cache.invalidation.entry", 1u64, "subgraph.name" = "orga", "graphql.type" = "Organization", "kind" = "type");

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
        let cache_keys_context = get_cache_keys_context(&response).unwrap();
        insta::assert_json_snapshot!(cache_keys_context);
        wait_for_cache(&cache, expected_cached_keys(&cache_keys_context)).await;

        let cache_control_header = get_cache_control_header(&response).unwrap();
        assert!(cache_control_contains_max_age(&cache_control_header));
        assert!(cache_control_contains_public(&cache_control_header));

        let mut response = response.next_response().await.unwrap();
        assert!(remove_debug_extensions_key(&mut response));

        insta::assert_json_snapshot!(response, @r#"
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
        "#);
    }.with_metrics().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn interval_cleanup_config() {
    let valid_schema = Arc::new(Schema::parse_and_validate(SCHEMA, "test.graphql").unwrap());

    let namespace = Some(String::from("interval-cleanup-config"));
    let config = RedisCacheConfig {
        namespace,
        ..default_redis_cache_config()
    };
    let cache = RedisCacheStorage::new(&config).await.unwrap();

    let _response_cache = ResponseCache::for_test(
        cache.clone(),
        Default::default(),
        valid_schema.clone(),
        true,
    )
    .await
    .unwrap();

    let namespace = Some(String::from("interval-cleanup-config-2"));
    let config = RedisCacheConfig {
        namespace: namespace.clone(),
        ..default_redis_cache_config()
    };
    let cache = RedisCacheStorage::new(&config).await.unwrap();
    let _response_cache = ResponseCache::for_test(
        cache.clone(),
        Default::default(),
        valid_schema.clone(),
        true,
    )
    .await
    .unwrap();

    // TODO: should this be interval_cleanup_config_3? prev code was _2
    let config = RedisCacheConfig {
        namespace,
        ..default_redis_cache_config()
    };
    let cache = RedisCacheStorage::new(&config).await.unwrap();
    let _response_cache = ResponseCache::for_test(
        cache.clone(),
        Default::default(),
        valid_schema.clone(),
        true,
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn failure_mode() {
    async {
        let valid_schema = Arc::new(Schema::parse_and_validate(SCHEMA, "test.graphql").unwrap());
        let query =
            "query { currentUser { activeOrganization { id creatorUser { __typename id } } } }";

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

        let map = [
            (
                "user".to_string(),
                Subgraph {
                    redis: None,
                    private_id: Some("sub".to_string()),
                    enabled: true.into(),
                    ttl: None,
                    ..Default::default()
                },
            ),
            (
                "orga".to_string(),
                Subgraph {
                    redis: None,
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
            ResponseCache::without_storage_for_failure_mode(map, valid_schema.clone())
                .await
                .unwrap();

        let service = TestHarness::builder()
            .configuration_json(serde_json::json!({
                "include_subgraph_errors": { "all": true },
                "experimental_mock_subgraphs": subgraphs.clone(),
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
        let response = response.next_response().await.unwrap();
        insta::assert_json_snapshot!(response, @r#"
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
        "#);

        assert_counter!(
            "apollo.router.operations.response_cache.fetch.error",
            1,
            "subgraph.name" = "orga",
            "code" = "NO_STORAGE"
        );
        assert_counter!(
            "apollo.router.operations.response_cache.fetch.error",
            1,
            "subgraph.name" = "user",
            "code" = "NO_STORAGE"
        );

        let service = TestHarness::builder()
            .configuration_json(
                serde_json::json!({"include_subgraph_errors": { "all": true },
                    "experimental_mock_subgraphs": subgraphs.clone(),
                }),
            )
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

        let response = response.next_response().await.unwrap();
        insta::assert_json_snapshot!(response, @r#"
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
        "#);

        assert_counter!(
            "apollo.router.operations.response_cache.fetch.error",
            2,
            "subgraph.name" = "orga",
            "code" = "NO_STORAGE"
        );
        assert_counter!(
            "apollo.router.operations.response_cache.fetch.error",
            2,
            "subgraph.name" = "user",
            "code" = "NO_STORAGE"
        );
    }
    .with_metrics()
    .await;
}

#[tokio::test]
async fn failure_mode_reconnect() {
    async {
        let valid_schema = Arc::new(Schema::parse_and_validate(SCHEMA, "test.graphql").unwrap());
        let query =
            "query { currentUser { activeOrganization { id creatorUser { __typename id } } } }";

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

        let map = [
            (
                "user".to_string(),
                Subgraph {
                    redis: None,
                    private_id: Some("sub".to_string()),
                    enabled: true.into(),
                    ttl: None,
                    ..Default::default()
                },
            ),
            (
                "orga".to_string(),
                Subgraph {
                    redis: None,
                    private_id: Some("sub".to_string()),
                    enabled: true.into(),
                    ttl: None,
                    ..Default::default()
                },
            ),
        ]
            .into_iter()
            .collect();
        let namespace = Some(String::from("failure-mode-reconnect"));
        let config = RedisCacheConfig {
            namespace,
            ..default_redis_cache_config()
        };
        let cache = RedisCacheStorage::new(&config).await.unwrap();

        cache.truncate_namespace().await.unwrap();

        let response_cache =
            ResponseCache::without_storage_for_failure_mode(map, valid_schema.clone())
                .await
                .unwrap();

        let service = TestHarness::builder()
            .configuration_json(serde_json::json!({
                "include_subgraph_errors": { "all": true },
                "experimental_mock_subgraphs": subgraphs.clone(),
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
        let response = response.next_response().await.unwrap();
        insta::assert_json_snapshot!(response, @r#"
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
        "#);

        assert_counter!(
            "apollo.router.operations.response_cache.fetch.error",
            1,
            "subgraph.name" = "orga",
            "code" = "NO_STORAGE"
        );
        assert_counter!(
            "apollo.router.operations.response_cache.fetch.error",
            1,
            "subgraph.name" = "user",
            "code" = "NO_STORAGE"
        );


        let service = TestHarness::builder()
            .configuration_json(
                serde_json::json!({"include_subgraph_errors": { "all": true },
                    "experimental_mock_subgraphs": subgraphs.clone(),
                }),
            )
            .unwrap()
            .schema(SCHEMA)
            .extra_private_plugin(response_cache.clone())
            .build_supergraph()
            .await
            .unwrap();

        response_cache
            .storage
            .all
            .as_ref()
            .expect("the database all should already be Some")
            .set(cache)
            .map_err(|_| "this should not be already set")
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
        let cache_keys_context = get_cache_keys_context(&response).unwrap();
        insta::with_settings!({
            description => "Make sure everything is in status 'new' and we have all the entities and root fields"
        }, {
            insta::assert_json_snapshot!(cache_keys_context);
        });

        let mut response = response.next_response().await.unwrap();
        assert!(remove_debug_extensions_key(&mut response));
        insta::assert_json_snapshot!(response, @r#"
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
        "#);

        assert_counter!(
            "apollo.router.operations.response_cache.fetch.error",
            1,
            "subgraph.name" = "orga",
            "code" = "NO_STORAGE"
        );
        assert_counter!(
            "apollo.router.operations.response_cache.fetch.error",
            1,
            "subgraph.name" = "user",
            "code" = "NO_STORAGE"
        );

        let service = TestHarness::builder()
            .configuration_json(
                serde_json::json!({"include_subgraph_errors": { "all": true },
                    "experimental_mock_subgraphs": subgraphs.clone(),
                }),
            )
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
        let cache_keys_context = get_cache_keys_context(&response).unwrap();
        insta::with_settings!({
            description => "Make sure everything is in status 'cached' and we have all the entities and root fields"
        }, {
            insta::assert_json_snapshot!(cache_keys_context);
        });

        let mut response = response.next_response().await.unwrap();
        assert!(remove_debug_extensions_key(&mut response));
        insta::assert_json_snapshot!(response, @r#"
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
        "#);

        assert_counter!(
            "apollo.router.operations.response_cache.fetch.error",
            1,
            "subgraph.name" = "orga",
            "code" = "NO_STORAGE"
        );
        assert_counter!(
            "apollo.router.operations.response_cache.fetch.error",
            1,
            "subgraph.name" = "user",
            "code" = "NO_STORAGE"
        );
    }
        .with_metrics()
        .await;
}
