#![cfg(test)]
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use apollo_compiler::Schema;
use futures::StreamExt;
use http::HeaderName;
use http::HeaderValue;
use http::header::CACHE_CONTROL;
use rstest::rstest;
use tokio_stream::wrappers::IntervalStream;
use tower::Service;
use tower::ServiceExt;
use uuid::Uuid;

use super::plugin::CacheSubgraph;
use super::plugin::ResponseCache;
use crate::Context;
use crate::MockedSubgraphs;
use crate::TestHarness;
use crate::configuration::subgraph::SubgraphConfiguration;
use crate::graphql;
use crate::metrics::FutureMetricsExt;
use crate::plugin::test::MockSubgraph;
use crate::plugin::test::MockSubgraphService;
use crate::plugins::response_cache::debugger::CacheEntryKind;
use crate::plugins::response_cache::debugger::CacheKeysContext;
use crate::plugins::response_cache::invalidation::InvalidationRequest;
use crate::plugins::response_cache::invalidation_endpoint::SubgraphInvalidationConfig;
use crate::plugins::response_cache::metrics::CacheMetricContextKey;
use crate::plugins::response_cache::plugin::CACHE_DEBUG_HEADER_NAME;
use crate::plugins::response_cache::plugin::CONTEXT_CACHE_KEY;
use crate::plugins::response_cache::plugin::INVALIDATION_SHARED_KEY;
use crate::plugins::response_cache::plugin::Subgraph;
use crate::plugins::response_cache::storage::CacheStorage;
use crate::plugins::response_cache::storage::redis::Config;
use crate::plugins::response_cache::storage::redis::Storage;
use crate::services::subgraph;
use crate::services::supergraph;

const SCHEMA: &str = include_str!("../../testdata/orga_supergraph_cache_key.graphql");
const SCHEMA_CACHE_TAG: &str =
    include_str!("../../testdata/orga_supergraph_cache_key_cache_tag.graphql");
const SCHEMA_REQUIRES: &str = include_str!("../../testdata/supergraph_cache_key.graphql");
const SCHEMA_NESTED_KEYS: &str =
    include_str!("../../testdata/supergraph_nested_fields_cache_key.graphql");

/// Cache inserts happen asynchronously, so there's no way to wait for a cache insert based on the
/// `TestHarness` service return value.
///
/// Instead, we wait for up to 5 seconds for the keys we expected to be present in the cache storage.
async fn wait_for_cache(storage: &Storage, keys: Vec<String>) {
    if keys.is_empty() {
        return;
    }

    let keys_strs: Vec<&str> = keys.iter().map(|s| s.as_str()).collect();
    let mut interval_stream =
        IntervalStream::new(tokio::time::interval(Duration::from_millis(100))).take(50);

    while interval_stream.next().await.is_some() {
        if let Ok(values) = storage.fetch_multiple(&keys_strs, "").await
            && values.iter().all(Option::is_some)
        {
            return;
        }
    }

    panic!("insert not complete");
}

pub(super) fn create_subgraph_conf(
    subgraphs: HashMap<String, Subgraph>,
) -> SubgraphConfiguration<Subgraph> {
    SubgraphConfiguration {
        all: Subgraph {
            invalidation: Some(SubgraphInvalidationConfig {
                enabled: true,
                shared_key: INVALIDATION_SHARED_KEY.to_string(),
                ..Default::default()
            }),
            ..Default::default()
        },
        subgraphs,
    }
}

/// Extracts a list of cache keys from `CacheKeysContext` that we expect to be cached. This is
/// mostly used in `wait_for_cache`.
///
/// NB: this is not always accurate! For example, a key might not be stored if it's private but
/// wasn't passed the private ID. But it's a good approximation for most test cases.
fn expected_cached_keys(cache_keys_context: &CacheKeysContext) -> Vec<String> {
    cache_keys_context
        .iter()
        .filter(|context| context.cache_control.should_store())
        .map(|context| context.key.clone())
        .collect()
}

/// Extract `CacheKeysContext` from `supergraph::Response` and prepare it for a snapshot, sorting
/// the invalidation keys and setting `created` to zero.
fn get_cache_keys_context(response: &supergraph::Response) -> Option<CacheKeysContext> {
    let mut cache_keys: CacheKeysContext = response
        .context
        .get(super::plugin::CONTEXT_DEBUG_CACHE_KEYS)
        .ok()??;
    cache_keys.iter_mut().for_each(|ck| {
        ck.invalidation_keys.sort();
        ck.cache_control.zero_out_created();
    });
    cache_keys.sort_by(|a, b| a.invalidation_keys.cmp(&b.invalidation_keys));
    Some(cache_keys)
}

fn get_cache_control_header(response: &supergraph::Response) -> Option<Vec<String>> {
    let cache_control_headers: Vec<String> = response
        .response
        .headers()
        .get_all(CACHE_CONTROL)
        .iter()
        .flat_map(|header| header.to_str().unwrap().split(','))
        .map(ToString::to_string)
        .collect();

    if cache_control_headers.is_empty() {
        return None;
    }

    Some(cache_control_headers)
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
fn remove_debug_extensions_key(response: &mut graphql::Response) -> bool {
    response
        .extensions
        .remove(super::plugin::CACHE_DEBUG_EXTENSIONS_KEY)
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

    let (drop_tx, drop_rx) = tokio::sync::broadcast::channel(2);
    let storage = Storage::new(&Config::test(false, &Uuid::new_v4().to_string()), drop_rx)
        .await
        .unwrap();
    let subgraphs_conf = create_subgraph_conf(
        [
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
        .collect(),
    );
    let response_cache = ResponseCache::for_test(
        storage.clone(),
        subgraphs_conf,
        valid_schema.clone(),
        true,
        drop_tx,
        true,
    )
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
    let cache_keys = get_cache_keys_context(&response).expect("missing cache keys");
    insta::with_settings!({
        description => "Make sure everything is in status 'new' and we have all the entities and root fields"
    }, {
        insta::assert_json_snapshot!(cache_keys);
    });

    let cache_control_header = get_cache_control_header(&response).expect("missing header");
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

    wait_for_cache(&storage, expected_cached_keys(&cache_keys)).await;
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

    let cache_control_header = get_cache_control_header(&response).expect("missing header");
    assert!(cache_control_contains_max_age(&cache_control_header));
    assert!(cache_control_contains_public(&cache_control_header));

    let cache_keys = get_cache_keys_context(&response).expect("missing cache keys");
    insta::with_settings!({
        description => "Make sure everything is in status 'cached' and we have all the entities and root fields"
    }, {
        insta::assert_json_snapshot!(cache_keys);
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
async fn insert_with_custom_key() {
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

    let (drop_tx, drop_rx) = tokio::sync::broadcast::channel(2);
    let storage = Storage::new(&Config::test(false, &Uuid::new_v4().to_string()), drop_rx)
        .await
        .unwrap();
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
    let subgraphs_conf = create_subgraph_conf(map);
    let response_cache = ResponseCache::for_test(
        storage.clone(),
        subgraphs_conf,
        valid_schema.clone(),
        true,
        drop_tx,
        true,
    )
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
    let context = Context::new();
    context.insert_json_value(
        CONTEXT_CACHE_KEY,
        serde_json_bytes::json!({
            "all": {
              "locale": "be"
            },
            "subgraphs": {
                "user": {
                    "foo": "bar"
                }
            }
        }),
    );
    let request = supergraph::Request::fake_builder()
        .query(query)
        .context(context.clone())
        .header(
            HeaderName::from_static(CACHE_DEBUG_HEADER_NAME),
            HeaderValue::from_static("true"),
        )
        .build()
        .unwrap();
    let mut response = service.oneshot(request).await.unwrap();
    let cache_keys = get_cache_keys_context(&response).expect("missing cache keys");
    insta::with_settings!({
        description => "Make sure everything is with source 'subgraph' and we have all the entities and root fields"
    }, {
        insta::assert_json_snapshot!(cache_keys);
    });

    let cache_control_header = get_cache_control_header(&response).expect("missing header");
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

    wait_for_cache(&storage, expected_cached_keys(&cache_keys)).await;
    let service = TestHarness::builder()
        .configuration_json(serde_json::json!({"include_subgraph_errors": { "all": true }, "experimental_mock_subgraphs": subgraphs, }))
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

    let cache_control_header = get_cache_control_header(&response).expect("missing header");
    assert!(cache_control_contains_max_age(&cache_control_header));
    assert!(cache_control_contains_public(&cache_control_header));

    let cache_keys = get_cache_keys_context(&response).expect("missing cache keys");
    insta::with_settings!({
        description => "Make sure everything is with source 'subgraph' because we didn't pass the context and we have all the entities and root fields"
    }, {
        insta::assert_json_snapshot!(cache_keys);
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
async fn already_expired_cache_control() {
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
            "headers": {"cache-control": "public", "age": "5"},
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
            "headers": {"cache-control": "public", "age": "1000000"},
        },
    });

    let (drop_tx, drop_rx) = tokio::sync::broadcast::channel(2);
    let storage = Storage::new(&Config::test(false, &Uuid::new_v4().to_string()), drop_rx)
        .await
        .unwrap();
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
    let subgraphs_conf = create_subgraph_conf(map);
    let response_cache = ResponseCache::for_test(
        storage.clone(),
        subgraphs_conf,
        valid_schema.clone(),
        true,
        drop_tx,
        true,
    )
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
    let cache_keys = get_cache_keys_context(&response).expect("missing cache keys");
    insta::with_settings!({
        description => "Make sure everything is in status 'new' and we have all the entities and root fields"
    }, {
        insta::assert_json_snapshot!(cache_keys);
    });

    let cache_control_header = get_cache_control_header(&response).expect("missing header");
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

    wait_for_cache(&storage, expected_cached_keys(&cache_keys)).await;
    let service = TestHarness::builder()
        .configuration_json(serde_json::json!({"include_subgraph_errors": { "all": true }, "experimental_mock_subgraphs": subgraphs }))
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

    let cache_keys = get_cache_keys_context(&response).expect("missing cache keys");
    insta::with_settings!({
        description => "Make sure only root field query is in status 'cached' and entities are not cached"
    }, {
        insta::assert_json_snapshot!(cache_keys);
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

    let (drop_tx, drop_rx) = tokio::sync::broadcast::channel(2);
    let storage = Storage::new(&Config::test(false, &Uuid::new_v4().to_string()), drop_rx)
        .await
        .unwrap();
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
    let subgraphs_conf = create_subgraph_conf(map);
    let response_cache = ResponseCache::for_test(
        storage.clone(),
        subgraphs_conf,
        valid_schema.clone(),
        true,
        drop_tx,
        true,
    )
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

    let cache_control_header = get_cache_control_header(&response).expect("missing header");
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

    let cache_control_header = get_cache_control_header(&response).expect("missing header");
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
                            "upc": "1",
                            "price": 150,
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

    let (drop_tx, drop_rx) = tokio::sync::broadcast::channel(2);
    let storage = Storage::new(&Config::test(false, &Uuid::new_v4().to_string()), drop_rx)
        .await
        .unwrap();
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
    let subgraphs_conf = create_subgraph_conf(map);
    let response_cache = ResponseCache::for_test(
        storage.clone(),
        subgraphs_conf,
        valid_schema.clone(),
        true,
        drop_tx,
        true,
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
    let cache_keys = get_cache_keys_context(&response).expect("missing cache keys");
    insta::with_settings!({
        description => "Make sure everything is in status 'new' and we have all the entities and root fields"
    }, {
        insta::assert_json_snapshot!(cache_keys);
    });

    let cache_control_header = get_cache_control_header(&response).expect("missing header");
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

    wait_for_cache(&storage, expected_cached_keys(&cache_keys)).await;
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
    let cache_keys = get_cache_keys_context(&response).expect("missing cache keys");
    insta::with_settings!({
        description => "Make sure everything is in status 'cached' and we have all the entities and root fields"
    }, {
        insta::assert_json_snapshot!(cache_keys);
    });
    let cache_control_header = get_cache_control_header(&response).expect("missing header");
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

    let (drop_tx, drop_rx) = tokio::sync::broadcast::channel(2);
    let storage = Storage::new(&Config::test(false, &Uuid::new_v4().to_string()), drop_rx)
        .await
        .unwrap();
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
    let subgraphs_conf = create_subgraph_conf(map);
    let response_cache = ResponseCache::for_test(
        storage.clone(),
        subgraphs_conf,
        valid_schema.clone(),
        true,
        drop_tx,
        true,
    )
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
    let cache_keys = get_cache_keys_context(&response).expect("missing cache keys");
    insta::with_settings!({
        description => "Make sure everything is in status 'new' and we have all the entities and root fields"
    }, {
        insta::assert_json_snapshot!(cache_keys);
    });

    let cache_control_header = get_cache_control_header(&response).expect("missing header");
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

    wait_for_cache(&storage, expected_cached_keys(&cache_keys)).await;
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

    let cache_control_header = get_cache_control_header(&response).expect("missing header");
    assert!(cache_control_contains_max_age(&cache_control_header));
    assert!(cache_control_contains_public(&cache_control_header));

    let cache_keys = get_cache_keys_context(&response).expect("missing cache keys");
    insta::with_settings!({
        description => "Make sure everything is in status 'cached' and we have all the entities and root fields"
    }, {
        insta::assert_json_snapshot!(cache_keys);
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

    let (drop_tx, drop_rx) = tokio::sync::broadcast::channel(2);
    let storage = Storage::new(&Config::test(false, &Uuid::new_v4().to_string()), drop_rx)
        .await
        .unwrap();
    let response_cache = ResponseCache::for_test(
        storage.clone(),
        Default::default(),
        valid_schema.clone(),
        false,
        drop_tx,
        true,
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

    let cache_control_header = get_cache_control_header(&response).expect("missing header");
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

    let cache_control_header = get_cache_control_header(&response).expect("missing header");
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

    let (drop_tx, drop_rx) = tokio::sync::broadcast::channel(2);
    let storage = Storage::new(&Config::test(false, &Uuid::new_v4().to_string()), drop_rx)
        .await
        .unwrap();
    let response_cache = ResponseCache::for_test(
        storage.clone(),
        Default::default(),
        valid_schema.clone(),
        false,
        drop_tx,
        true,
    )
    .await
    .unwrap();

    let service = TestHarness::builder()
        .configuration_json(serde_json::json!({"include_subgraph_errors": { "all": true }, "experimental_mock_subgraphs": subgraphs.clone(), "headers": {
            "all": {
                "request": { "operations": [{
                    "propagate": {
                        "named": "cache-control"
                    }
                }]}
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

    let cache_control_header = get_cache_control_header(&response).expect("missing header");
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

    // Just to make sure it doesn't invalidate anything, which means nothing has been stored
    let invalidations_by_subgraph = storage
        .invalidate(
            vec![
                "user".to_string(),
                "organization".to_string(),
                "currentUser".to_string(),
            ],
            vec!["orga".to_string(), "user".to_string()],
            "test_bulk_invalidation",
        )
        .await
        .unwrap();
    assert_eq!(invalidations_by_subgraph.into_values().sum::<u64>(), 0);

    let service = TestHarness::builder()
        .configuration_json(serde_json::json!({"include_subgraph_errors": { "all": true }, "experimental_mock_subgraphs": subgraphs.clone(), "headers": {
            "all": {
                "request": { "operations": [{
                    "propagate": {
                        "named": "cache-control"
                    }
                }]}
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

    let cache_control_header = get_cache_control_header(&response).expect("missing header");
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

    // Just to make sure it doesn't invalidate anything, which means nothing has been stored
    let invalidations_by_subgraph = storage
        .invalidate(
            vec![
                "user".to_string(),
                "organization".to_string(),
                "currentUser".to_string(),
            ],
            vec!["orga".to_string(), "user".to_string()],
            "test_bulk_invalidate",
        )
        .await
        .unwrap();
    assert_eq!(invalidations_by_subgraph.into_values().sum::<u64>(), 0);
}

// Regression test for ROUTER-1689:
// When `cache-control: no-cache` is sent by the client and response_cache is enabled,
// entity fields resolved via `_entities` queries must not be discarded.
// Previously, the no-cache fast-path returned an empty IntermediateResult list,
// causing insert_entities_in_result to produce `_entities: []` and entity fields to be null.
#[tokio::test]
async fn no_cache_from_request() {
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

    let (drop_tx, drop_rx) = tokio::sync::broadcast::channel(2);
    let storage = Storage::new(&Config::test(false, &Uuid::new_v4().to_string()), drop_rx)
        .await
        .unwrap();
    let response_cache = ResponseCache::for_test(
        storage.clone(),
        Default::default(),
        valid_schema.clone(),
        false,
        drop_tx,
        true,
    )
    .await
    .unwrap();

    // Phase 1: Warm up the cache with a normal request (no cache-control header)
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
        .build()
        .unwrap();
    let mut response = service.oneshot(request).await.unwrap();
    let response = response.next_response().await.unwrap();

    // Sanity-check: normal request returns entity data
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

    // Phase 2: Request with `no-cache` — cache must be bypassed for lookup but entity data
    // from the subgraph must still be returned correctly (regression for ROUTER-1689).
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

    let no_cache_context = Context::new();
    let request = supergraph::Request::fake_builder()
        .query(query)
        .context(no_cache_context.clone())
        .header(CACHE_CONTROL, HeaderValue::from_static("no-cache"))
        .build()
        .unwrap();
    let mut response = service.oneshot(request).await.unwrap();
    let response = response.next_response().await.unwrap();

    // Entity fields must NOT be null — this was the regression
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

    // Metrics must NOT be recorded for no-cache requests (no misleading cache hit/miss counters)
    let orga_metric = no_cache_context
        .get::<_, CacheSubgraph>(CacheMetricContextKey::new("orga".to_string()))
        .ok()
        .flatten();
    assert!(
        orga_metric.is_none(),
        "no-cache requests should not record cache hit/miss metrics"
    );
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

        let (drop_tx, drop_rx) = tokio::sync::broadcast::channel(2);
        let storage = Storage::new(&Config::test(false,"private_only"), drop_rx)
            .await
            .unwrap();
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
        let subgraphs_conf = create_subgraph_conf(map);

        let response_cache =
            ResponseCache::for_test(storage.clone(), subgraphs_conf, valid_schema.clone(), true, drop_tx, true)
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
        let cache_keys = get_cache_keys_context(&response).expect("missing cache keys");
        insta::assert_json_snapshot!(cache_keys);

        assert_gauge!("apollo.router.response_cache.private_queries.lru.size", 2); // root-field and entity queries

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
        let cache_control_header = get_cache_control_header(&response).expect("missing header");
        assert!(cache_control_contains_private(&cache_control_header));
        let cache_keys = get_cache_keys_context(&response).expect("missing cache keys");
        insta::assert_json_snapshot!(cache_keys);

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
        let cache_control_header = get_cache_control_header(&response).expect("missing header");
        assert!(cache_control_contains_private(&cache_control_header));
        let cache_keys = get_cache_keys_context(&response).expect("missing cache keys");
        insta::assert_json_snapshot!(cache_keys);

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

    let (drop_tx, drop_rx) = tokio::sync::broadcast::channel(2);
    let storage = Storage::new(&Config::test(false, &Uuid::new_v4().to_string()), drop_rx)
        .await
        .unwrap();
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
    let subgraphs_conf = create_subgraph_conf(map);
    let response_cache = ResponseCache::for_test(
        storage.clone(),
        subgraphs_conf,
        valid_schema.clone(),
        true,
        drop_tx,
        true,
    )
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
    let cache_keys = get_cache_keys_context(&response).expect("missing cache keys");
    insta::assert_json_snapshot!(cache_keys);

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
    let cache_control_header = get_cache_control_header(&response).expect("missing header");
    assert!(cache_control_contains_private(&cache_control_header));
    let cache_keys = get_cache_keys_context(&response).expect("missing cache keys");
    insta::assert_json_snapshot!(cache_keys);

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
    let cache_control_header = get_cache_control_header(&response).expect("missing header");
    assert!(cache_control_contains_private(&cache_control_header));
    let cache_keys = get_cache_keys_context(&response).expect("missing cache keys");
    insta::assert_json_snapshot!(cache_keys);

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

        let (drop_tx, drop_rx) = tokio::sync::broadcast::channel(2);
        let storage = Storage::new(&Config::test(false,"polymorphic_private_and_public"), drop_rx)
            .await
            .unwrap();
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
        let subgraphs_conf = create_subgraph_conf(map);
        let response_cache =
            ResponseCache::for_test(storage.clone(), subgraphs_conf, valid_schema.clone(), true, drop_tx, true)
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
        let cache_keys = get_cache_keys_context(&response).expect("missing cache keys");
        insta::with_settings!({
            description => "Make sure everything is in status 'new' and we have all the entities and root fields"
        }, {
            insta::assert_json_snapshot!(cache_keys);
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
        let cache_control_header = get_cache_control_header(&response).expect("missing header");
        assert!(cache_control_contains_public(&cache_control_header));
        let cache_keys = get_cache_keys_context(&response).expect("missing cache keys");
        insta::assert_json_snapshot!(cache_keys);

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
        let cache_control_header = get_cache_control_header(&response).expect("missing header");
        assert!(cache_control_contains_private(&cache_control_header));
        let cache_keys = get_cache_keys_context(&response).expect("missing cache keys");
        insta::assert_json_snapshot!(cache_keys);

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
        let cache_control_header = get_cache_control_header(&response).expect("missing header");
        assert!(cache_control_contains_public(&cache_control_header));
        let cache_keys = get_cache_keys_context(&response).expect("missing cache keys");
        insta::assert_json_snapshot!(cache_keys);

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
        assert_gauge!("apollo.router.response_cache.private_queries.lru.size", 2); // root-field and entity queries

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
        let cache_control_header = get_cache_control_header(&response).expect("missing header");
        assert!(cache_control_contains_private(&cache_control_header));
        let cache_keys = get_cache_keys_context(&response).expect("missing cache keys");
        insta::assert_json_snapshot!(cache_keys);

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
        assert_gauge!("apollo.router.response_cache.private_queries.lru.size", 2); // root-field and entity queries

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
        let cache_control_header = get_cache_control_header(&response).expect("missing header");
        assert!(cache_control_contains_public(&cache_control_header));
        let cache_keys = get_cache_keys_context(&response).expect("missing cache keys");
        insta::assert_json_snapshot!(cache_keys);

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
        assert_gauge!("apollo.router.response_cache.private_queries.lru.size", 2); // root-field and entity queries
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

        let (drop_tx, drop_rx) = tokio::sync::broadcast::channel(2);
        let storage = Storage::new(&Config::test(false,"private_without_private_id"), drop_rx)
            .await
            .unwrap();
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

        let subgraphs_conf = create_subgraph_conf(map);
        let response_cache =
            ResponseCache::for_test(storage.clone(), subgraphs_conf, valid_schema.clone(), true, drop_tx, true)
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
        let cache_control_header = get_cache_control_header(&response).expect("missing header");
        assert!(cache_control_contains_private(&cache_control_header));
        let cache_keys = get_cache_keys_context(&response).expect("missing cache keys");
        insta::assert_json_snapshot!(cache_keys);

        assert_gauge!("apollo.router.response_cache.private_queries.lru.size", 2); // root-field and entity queries

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
        let cache_control_header = get_cache_control_header(&response).expect("missing header");
        assert!(cache_control_contains_private(&cache_control_header));
        let cache_keys = get_cache_keys_context(&response).expect("missing cache keys");
        insta::assert_json_snapshot!(cache_keys);

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

    let (drop_tx, drop_rx) = tokio::sync::broadcast::channel(2);
    let storage = Storage::new(&Config::test(false, &Uuid::new_v4().to_string()), drop_rx)
        .await
        .unwrap();
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
    let subgraphs_conf = create_subgraph_conf(map);
    let response_cache = ResponseCache::for_test(
        storage.clone(),
        subgraphs_conf,
        valid_schema.clone(),
        true,
        drop_tx,
        true,
    )
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

    let cache_keys = get_cache_keys_context(&response).expect("missing cache keys");
    insta::assert_json_snapshot!(cache_keys, {
        "[].cache_control" => insta::dynamic_redaction(|value, _path| {
            let cache_control = value.as_str().unwrap().to_string();
            assert!(cache_control.contains("max-age="));
            assert!(cache_control.contains("public"));
            "[REDACTED]"
        })
    });

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

    let cache_keys = get_cache_keys_context(&response).expect("missing cache keys");
    insta::assert_json_snapshot!(cache_keys);
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
    let subgraphs_conf = create_subgraph_conf(map);

    // Use a shared namespace so the second storage can access cached data from the first
    let namespace = Uuid::new_v4().to_string();

    let (drop_tx, drop_rx) = tokio::sync::broadcast::channel(2);
    let storage = Storage::new(&Config::test(false, &namespace), drop_rx)
        .await
        .unwrap();
    let response_cache = ResponseCache::for_test(
        storage.clone(),
        subgraphs_conf,
        valid_schema.clone(),
        true,
        drop_tx,
        true,
    )
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
    assert!(remove_debug_extensions_key(&mut response));
    insta::assert_json_snapshot!(response);

    // Reuse the same namespace so cached entities from the first request are accessible
    let (drop_tx, drop_rx) = tokio::sync::broadcast::channel(2);
    let storage = Storage::new(&Config::test(false, &namespace), drop_rx)
        .await
        .unwrap();
    let response_cache = ResponseCache::for_test(
        storage.clone(),
        Default::default(),
        valid_schema.clone(),
        false,
        drop_tx,
        true,
    )
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

        let (drop_tx, drop_rx) = tokio::sync::broadcast::channel(2);
        let storage = Storage::new(&Config::test(false,"test_invalidate_by_cache_tag"), drop_rx)
            .await
            .unwrap();
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
        let subgraphs_conf = create_subgraph_conf(map);
        let response_cache =
            ResponseCache::for_test(storage.clone(), subgraphs_conf, valid_schema.clone(), true, drop_tx, true)
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
        let cache_keys = get_cache_keys_context(&response).expect("missing cache keys");
        insta::assert_json_snapshot!(cache_keys);
        let cache_control_header = get_cache_control_header(&response).expect("missing header");
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
        assert_histogram_sum!("apollo.router.operations.response_cache.fetch.entity", 1u64, "subgraph.name" = "orga");

        // Now testing without any mock subgraphs, all the data should come from the cache
        wait_for_cache(&storage, expected_cached_keys(&cache_keys)).await;
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
        let cache_keys = get_cache_keys_context(&response).expect("missing cache keys");
        insta::assert_json_snapshot!(cache_keys);
        let cache_control_header = get_cache_control_header(&response).expect("missing header");
        assert!(cache_control_contains_max_age(&cache_control_header));
        assert!(cache_control_contains_public(&cache_control_header));
        let mut response = response.next_response().await.unwrap();
        assert!(remove_debug_extensions_key(&mut response));
        assert_histogram_sum!("apollo.router.operations.response_cache.fetch.entity", 2u64, "subgraph.name" = "orga");

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
        let cache_keys = get_cache_keys_context(&response).expect("missing cache keys");
        insta::assert_json_snapshot!(cache_keys);
        let cache_control_header = get_cache_control_header(&response).expect("missing header");
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
        assert_histogram_sum!("apollo.router.operations.response_cache.fetch.entity", 3u64, "subgraph.name" = "orga");
    }.with_metrics().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn complex_cache_tag() {
    async move {
        let valid_schema = Arc::new(Schema::parse_and_validate(SCHEMA_CACHE_TAG, "test.graphql").unwrap());
        let query = "query { currentUser { activeOrganization { ... on Organization { id creatorUser { __typename id } } } } }";
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

        let (drop_tx, drop_rx) = tokio::sync::broadcast::channel(2);
        let storage = Storage::new(&Config::test(false,"test_complex_cache_tag"), drop_rx)
            .await
            .unwrap();
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
        let subgraphs_conf = create_subgraph_conf(map);
        let response_cache =
            ResponseCache::for_test(storage.clone(), subgraphs_conf, valid_schema.clone(), true, drop_tx, true)
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
        let cache_keys = get_cache_keys_context(&response).expect("missing cache keys");
        insta::assert_json_snapshot!(cache_keys);
        let cache_control_header = get_cache_control_header(&response).expect("missing header");
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

        let (drop_tx, drop_rx) = tokio::sync::broadcast::channel(2);
        let storage = Storage::new(&Config::test(false,"test_invalidate_by_subgraph"), drop_rx)
            .await
            .unwrap();
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
        let subgraphs_conf = create_subgraph_conf(map);
        let response_cache =
            ResponseCache::for_test(storage.clone(), subgraphs_conf, valid_schema.clone(), true, drop_tx, true)
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
        let cache_keys = get_cache_keys_context(&response).expect("missing cache keys");
        insta::assert_json_snapshot!(cache_keys);
        let cache_control_header = get_cache_control_header(&response).expect("missing header");
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
        wait_for_cache(&storage, expected_cached_keys(&cache_keys)).await;
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
        let cache_keys = get_cache_keys_context(&response).expect("missing cache keys");
        insta::assert_json_snapshot!(cache_keys);

        let cache_control_header = get_cache_control_header(&response).expect("missing header");
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
        let cache_keys = get_cache_keys_context(&response).expect("missing cache keys");
        insta::assert_json_snapshot!(cache_keys);

        let cache_control_header = get_cache_control_header(&response).expect("missing header");
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
        let (_drop_tx, drop_rx) = tokio::sync::broadcast::channel(2);
        let storage = Storage::new(&Config::test(false,"failure_mode_reconnect"), drop_rx)
            .await
            .unwrap();
        storage.truncate_namespace().await.unwrap();

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
            .storage.replace_storage(storage).expect("must be able to replace");

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
        let cache_keys = get_cache_keys_context(&response).expect("missing cache keys");
        insta::with_settings!({
            description => "Make sure everything is in status 'new' and we have all the entities and root fields"
        }, {
            insta::assert_json_snapshot!(cache_keys);
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
        let cache_keys = get_cache_keys_context(&response).expect("missing cache keys");
        insta::with_settings!({
            description => "Make sure everything is in status 'cached' and we have all the entities and root fields"
        }, {
            insta::assert_json_snapshot!(cache_keys);
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

/// When one subgraph returns data with a `Cache-Control: max-age=N, public` header and another
/// subgraph times out via the traffic shaping layer, the final HTTP response must carry
/// `Cache-Control: no-store` to prevent intermediate caches from caching a partial/error response.
#[tokio::test(flavor = "multi_thread")]
async fn no_store_on_subgraph_timeout() {
    let valid_schema = Arc::new(Schema::parse_and_validate(SCHEMA, "test.graphql").unwrap());
    // This query spans two subgraphs: `user` (returns data) and `orga` (entity lookup).
    let query = "query { currentUser { activeOrganization { id creatorUser { __typename id } } } }";

    // `user` returns data with a cacheable header; `orga` is configured to sleep so it times out.
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
            "headers": {"cache-control": "max-age=1800, public"},
        },
        "orga": {
            "entities": [],
        },
    });

    let (drop_tx, drop_rx) = tokio::sync::broadcast::channel(2);
    let storage = Storage::new(&Config::test(false, &Uuid::new_v4().to_string()), drop_rx)
        .await
        .unwrap();
    let subgraphs_conf = create_subgraph_conf(HashMap::from([
        ("user".to_string(), Subgraph::default()),
        ("orga".to_string(), Subgraph::default()),
    ]));
    let response_cache = ResponseCache::for_test(
        storage.clone(),
        subgraphs_conf,
        valid_schema.clone(),
        true,
        drop_tx,
        true,
    )
    .await
    .unwrap();

    let service = TestHarness::builder()
        .configuration_json(serde_json::json!({
            "include_subgraph_errors": { "all": true },
            "experimental_mock_subgraphs": subgraphs,
            // Force a 1ms timeout on the `orga` subgraph so it always times out.
            "traffic_shaping": {
                "subgraphs": {
                    "orga": {
                        "timeout": "1ms"
                    }
                }
            }
        }))
        .unwrap()
        .schema(SCHEMA)
        .extra_private_plugin(response_cache.clone())
        // Override the `orga` subgraph service to sleep long enough to trigger the timeout.
        .subgraph_hook(|name, service| {
            if name == "orga" {
                tower::service_fn(|_req: subgraph::Request| async move {
                    tokio::time::sleep(Duration::from_millis(500)).await;
                    // Unreachable in practice — the traffic shaping timeout fires first.
                    Err::<subgraph::Response, tower::BoxError>("orga sleep exceeded".into())
                })
                .boxed()
            } else {
                service
            }
        })
        .build_supergraph()
        .await
        .unwrap();

    let request = supergraph::Request::fake_builder()
        .query(query)
        .context(Context::new())
        .build()
        .unwrap();
    let mut response = service.oneshot(request).await.unwrap();

    // The response must contain `no-store` because the `orga` subgraph timed out.
    let cache_control_header =
        get_cache_control_header(&response).expect("missing cache-control header");
    assert!(
        cache_control_contains_no_store(&cache_control_header),
        "expected Cache-Control: no-store when a subgraph times out, got: {:?}",
        cache_control_header
    );
    assert!(
        !cache_control_contains_public(&cache_control_header),
        "Cache-Control must not contain 'public' when a subgraph timed out, got: {:?}",
        cache_control_header
    );
    assert!(
        !cache_control_contains_max_age(&cache_control_header),
        "Cache-Control must not contain max-age when a subgraph timed out, got: {:?}",
        cache_control_header
    );

    // The response body should contain errors from the timed-out subgraph.
    let body = response.next_response().await.unwrap();
    assert!(
        !body.errors.is_empty(),
        "expected errors in response body due to subgraph timeout"
    );
}

/// When one subgraph returns data with a `Cache-Control: max-age=N, public` header and another
/// subgraph returns errors (simulating a partial failure), the final HTTP response must carry
/// `Cache-Control: no-store` to prevent intermediate caches (CDNs, reverse proxies) from caching an
/// incomplete or error response.
#[tokio::test]
async fn no_store_on_partial_subgraph_failure() {
    let valid_schema = Arc::new(Schema::parse_and_validate(SCHEMA, "test.graphql").unwrap());
    // This query spans two subgraphs: `user` (returns data) and `orga` (entity lookup).
    let query = "query { currentUser { activeOrganization { id creatorUser { __typename id } } } }";

    // Configure only `user` subgraph — `orga` is intentionally omitted so it returns an error.
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
            "headers": {"cache-control": "max-age=1800, public"},
        },
        // `orga` is intentionally not configured — the mock plugin will return a GraphQL error.
    });

    let (drop_tx, drop_rx) = tokio::sync::broadcast::channel(2);
    let storage = Storage::new(&Config::test(false, &Uuid::new_v4().to_string()), drop_rx)
        .await
        .unwrap();
    let subgraphs_conf = create_subgraph_conf(HashMap::from([
        ("user".to_string(), Subgraph::default()),
        ("orga".to_string(), Subgraph::default()),
    ]));
    let response_cache = ResponseCache::for_test(
        storage.clone(),
        subgraphs_conf,
        valid_schema.clone(),
        true,
        drop_tx,
        true,
    )
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

    // The response must contain `no-store` — not `max-age` or `public` — because one subgraph
    // returned an error. Caching a partial response would be incorrect.
    let cache_control_header =
        get_cache_control_header(&response).expect("missing cache-control header");
    assert!(
        cache_control_contains_no_store(&cache_control_header),
        "expected Cache-Control: no-store on partial failure, got: {:?}",
        cache_control_header
    );
    assert!(
        !cache_control_contains_public(&cache_control_header),
        "Cache-Control must not contain 'public' when a subgraph failed, got: {:?}",
        cache_control_header
    );
    assert!(
        !cache_control_contains_max_age(&cache_control_header),
        "Cache-Control must not contain max-age when a subgraph failed, got: {:?}",
        cache_control_header
    );

    // The response body should contain errors from the failing subgraph.
    let body = response.next_response().await.unwrap();
    assert!(
        !body.errors.is_empty(),
        "expected errors in response body due to failing subgraph"
    );
}

/// Shared setup for include_cache_control_header_on_router_response integration tests.
/// Returns (storage, response_cache, subgraph_mock_config).
async fn setup_send_cache_control_test(
    include_cache_control_header_on_router_response: bool,
) -> (Storage, ResponseCache, serde_json::Value) {
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

    let (drop_tx, drop_rx) = tokio::sync::broadcast::channel(2);
    let storage = Storage::new(&Config::test(false, &Uuid::new_v4().to_string()), drop_rx)
        .await
        .unwrap();
    let subgraphs_conf = create_subgraph_conf(
        [
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
        .collect(),
    );
    let valid_schema = Arc::new(Schema::parse_and_validate(SCHEMA, "test.graphql").unwrap());
    let response_cache = ResponseCache::for_test(
        storage.clone(),
        subgraphs_conf,
        valid_schema,
        true,
        drop_tx,
        include_cache_control_header_on_router_response,
    )
    .await
    .unwrap();

    (storage, response_cache, subgraphs)
}

#[tokio::test]
async fn include_cache_control_header_on_router_response_false_suppresses_headers() {
    let query = "query { currentUser { activeOrganization { id creatorUser { __typename id } } } }";
    let (storage, response_cache, subgraphs) = setup_send_cache_control_test(false).await;

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
    let response = service.oneshot(request).await.unwrap();

    // Cache-Control header should NOT be present
    assert!(
        get_cache_control_header(&response).is_none(),
        "Cache-Control header should be suppressed when include_cache_control_header_on_router_response is false"
    );

    // But data should still be cached (internal caching still works)
    let cache_keys = get_cache_keys_context(&response).expect("missing cache keys");
    wait_for_cache(&storage, expected_cached_keys(&cache_keys)).await;

    // Second request — verify cache hit and still no Cache-Control header
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
    let response = service.oneshot(request).await.unwrap();

    // Still no Cache-Control header on cache hit
    assert!(
        get_cache_control_header(&response).is_none(),
        "Cache-Control header should remain suppressed on cache hit"
    );

    // Verify we got a cache hit
    let cache_keys = get_cache_keys_context(&response).expect("missing cache keys");
    assert!(
        cache_keys
            .iter()
            .any(|ck| matches!(ck.source, super::debugger::CacheKeySource::Cache)),
        "second request should produce a cache hit"
    );
}

#[tokio::test]
async fn include_cache_control_header_on_router_response_true_sends_headers() {
    let query = "query { currentUser { activeOrganization { id creatorUser { __typename id } } } }";
    let (_storage, response_cache, subgraphs) = setup_send_cache_control_test(true).await;

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
    let response = service.oneshot(request).await.unwrap();

    // Cache-Control header SHOULD be present (regression test for default behavior)
    let cache_control_header = get_cache_control_header(&response).expect("missing header");
    assert!(cache_control_contains_max_age(&cache_control_header));
    assert!(cache_control_contains_public(&cache_control_header));
}

/// What a toggleable mock subgraph returns for the next request. `Healthy` is a cacheable
/// `public, max-age=600` response. The failure shapes are what a subgraph framework can emit on
/// its error path (Plug, for example, defaults to `max-age=0, private, must-revalidate`).
#[derive(Clone, Copy, Debug)]
enum ToggledSubgraphResponse {
    Healthy,
    PrivateStorable,
    PrivateStorableNoData,
    PrivateDataWithErrors,
    PrivateStorableWithErrors,
    PrivateNullDataWithErrors,
    PrivateHttp500,
    PrivateMaxAgeZero,
    PublicDataWithErrors,
    NoStoreDataWithErrors,
}

/// A subgraph service that answers with the status, `Cache-Control` header, data and errors
/// selected in `mode`, and counts every call that reaches it.
fn toggled_subgraph_service(
    mode: Arc<std::sync::Mutex<ToggledSubgraphResponse>>,
    calls: Arc<std::sync::atomic::AtomicUsize>,
    healthy_data: serde_json_bytes::Value,
) -> subgraph::BoxService {
    tower::service_fn(move |req: subgraph::Request| {
        let mode = *mode.lock().unwrap();
        calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let healthy_data = healthy_data.clone();
        async move {
            let error = graphql::Error::builder()
                .message("subgraph failure")
                .extension_code("SUBGRAPH_FAILURE")
                .build();
            let (status, cache_control, data, errors) = match mode {
                ToggledSubgraphResponse::Healthy => (
                    http::StatusCode::OK,
                    "public, max-age=600",
                    Some(healthy_data),
                    vec![],
                ),
                ToggledSubgraphResponse::PrivateStorable => (
                    http::StatusCode::OK,
                    "private, max-age=600",
                    Some(healthy_data),
                    vec![],
                ),
                ToggledSubgraphResponse::PrivateStorableNoData => {
                    (http::StatusCode::OK, "private, max-age=600", None, vec![])
                }
                ToggledSubgraphResponse::PrivateDataWithErrors => (
                    http::StatusCode::OK,
                    "max-age=0, private, must-revalidate",
                    Some(healthy_data),
                    vec![error],
                ),
                ToggledSubgraphResponse::PrivateStorableWithErrors => (
                    http::StatusCode::OK,
                    "private, max-age=600",
                    Some(healthy_data),
                    vec![error],
                ),
                ToggledSubgraphResponse::PrivateNullDataWithErrors => (
                    http::StatusCode::OK,
                    "max-age=0, private, must-revalidate",
                    Some(serde_json_bytes::Value::Null),
                    vec![error],
                ),
                // Mirrors what the HTTP subgraph service hands to plugins for a non-2xx
                // response: the original status and headers, and a `SUBREQUEST_HTTP_ERROR`.
                ToggledSubgraphResponse::PrivateHttp500 => (
                    http::StatusCode::INTERNAL_SERVER_ERROR,
                    "max-age=0, private, must-revalidate",
                    None,
                    vec![
                        crate::error::FetchError::SubrequestHttpError {
                            service: req.subgraph_name.clone(),
                            status_code: Some(500),
                            reason: "500: Internal Server Error".to_string(),
                        }
                        .to_graphql_error(None),
                    ],
                ),
                ToggledSubgraphResponse::PrivateMaxAgeZero => (
                    http::StatusCode::OK,
                    "private, max-age=0",
                    Some(healthy_data),
                    vec![],
                ),
                ToggledSubgraphResponse::PublicDataWithErrors => (
                    http::StatusCode::OK,
                    "public, max-age=600",
                    Some(healthy_data),
                    vec![error],
                ),
                ToggledSubgraphResponse::NoStoreDataWithErrors => (
                    http::StatusCode::OK,
                    "no-store",
                    Some(healthy_data),
                    vec![error],
                ),
            };
            // Built like the HTTP subgraph service does, because `subgraph::Response::builder()`
            // turns missing `data` into `null`.
            let response = http::Response::builder()
                .status(status)
                .header(CACHE_CONTROL, cache_control)
                .body(
                    graphql::Response::builder()
                        .and_data(data)
                        .errors(errors)
                        .build(),
                )
                .unwrap();
            Ok(subgraph::Response::new_from_response(
                response,
                req.context,
                req.subgraph_name,
                req.id,
            ))
        }
    })
    .boxed()
}

/// A supergraph with the response cache in front of one toggleable subgraph: `user` for a
/// root-field fetch, or `orga` for an entity fetch (with `user` then returning a public root
/// response).
struct ToggledSubgraphHarness {
    service: supergraph::BoxCloneService,
    storage: Storage,
    mode: Arc<std::sync::Mutex<ToggledSubgraphResponse>>,
    calls: Arc<std::sync::atomic::AtomicUsize>,
    subgraph_name: &'static str,
    query: &'static str,
    _drop_tx: tokio::sync::broadcast::Sender<()>,
}

impl ToggledSubgraphHarness {
    async fn root_fields(with_private_id: bool) -> Self {
        Self::new(
            "user",
            "query { currentUser { activeOrganization { id } } }",
            serde_json_bytes::json!({
                "currentUser": {
                    "activeOrganization": { "__typename": "Organization", "id": "1" }
                }
            }),
            with_private_id,
        )
        .await
    }

    async fn entities(with_private_id: bool) -> Self {
        Self::new(
            "orga",
            "query { currentUser { activeOrganization { id creatorUser { __typename id } } } }",
            serde_json_bytes::json!({
                "_entities": [{
                    "__typename": "Organization",
                    "id": "1",
                    "creatorUser": { "__typename": "User", "id": 2 }
                }]
            }),
            with_private_id,
        )
        .await
    }

    async fn new(
        subgraph_name: &'static str,
        query: &'static str,
        healthy_data: serde_json_bytes::Value,
        with_private_id: bool,
    ) -> Self {
        let valid_schema = Arc::new(Schema::parse_and_validate(SCHEMA, "test.graphql").unwrap());
        let mode = Arc::new(std::sync::Mutex::new(ToggledSubgraphResponse::Healthy));
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));

        let (drop_tx, drop_rx) = tokio::sync::broadcast::channel(2);
        let storage = Storage::new(&Config::test(false, &Uuid::new_v4().to_string()), drop_rx)
            .await
            .unwrap();
        let subgraph = Subgraph {
            private_id: with_private_id.then(|| "sub".to_string()),
            ..Default::default()
        };
        let subgraphs_conf = create_subgraph_conf(HashMap::from([
            ("user".to_string(), subgraph.clone()),
            ("orga".to_string(), subgraph),
        ]));
        let response_cache = ResponseCache::for_test(
            storage.clone(),
            subgraphs_conf,
            valid_schema,
            true,
            drop_tx.clone(),
            true,
        )
        .await
        .unwrap();

        let hook_mode = mode.clone();
        let hook_calls = calls.clone();
        let service = TestHarness::builder()
            .configuration_json(serde_json::json!({
                "include_subgraph_errors": { "all": true },
                "experimental_mock_subgraphs": {
                    "user": {
                        "query": {
                            "currentUser": {
                                "activeOrganization": { "__typename": "Organization", "id": "1" }
                            }
                        },
                        "headers": {"cache-control": "public, max-age=600"},
                    },
                },
            }))
            .unwrap()
            .schema(SCHEMA)
            .extra_private_plugin(response_cache)
            .subgraph_hook(move |name, service| {
                if name == subgraph_name {
                    toggled_subgraph_service(
                        hook_mode.clone(),
                        hook_calls.clone(),
                        healthy_data.clone(),
                    )
                } else {
                    service
                }
            })
            .build_supergraph()
            .await
            .unwrap();

        Self {
            service,
            storage,
            mode,
            calls,
            subgraph_name,
            query,
            _drop_tx: drop_tx,
        }
    }

    fn respond_with(&self, response: ToggledSubgraphResponse) {
        *self.mode.lock().unwrap() = response;
    }

    fn subgraph_calls(&self) -> usize {
        self.calls.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Sends the query with cache debugging on and returns the debugger entries recorded for the
    /// toggleable subgraph.
    async fn call(&mut self, private_id: Option<&str>) -> CacheKeysContext {
        let context = Context::new();
        if let Some(private_id) = private_id {
            context.insert_json_value("sub", private_id.into());
        }
        let request = supergraph::Request::fake_builder()
            .query(self.query)
            .context(context)
            .header(
                HeaderName::from_static(CACHE_DEBUG_HEADER_NAME),
                HeaderValue::from_static("true"),
            )
            .build()
            .unwrap();
        let response = self
            .service
            .ready()
            .await
            .unwrap()
            .call(request)
            .await
            .unwrap();
        get_cache_keys_context(&response)
            .unwrap_or_default()
            .into_iter()
            .filter(|entry| entry.subgraph_name == self.subgraph_name)
            .collect()
    }

    /// Checks that the next healthy public response is stored and the request after it is served
    /// from the cache.
    async fn assert_healthy_response_is_cached(&mut self, private_id: Option<&str>) {
        self.respond_with(ToggledSubgraphResponse::Healthy);
        let calls_before = self.subgraph_calls();

        let entries = self.call(private_id).await;
        assert_eq!(self.subgraph_calls(), calls_before + 1);
        assert!(
            !entries.is_empty() && entries.iter().all(|entry| entry.should_store),
            "the healthy response should be stored, got: {entries:?}"
        );
        wait_for_cache(&self.storage, expected_cached_keys(&entries)).await;

        self.call(private_id).await;
        assert_eq!(
            self.subgraph_calls(),
            calls_before + 1,
            "the request after a stored healthy response should be served from the cache"
        );
    }
}

/// A private response that the cache could not store does not mark the query as private. The
/// next healthy public response is stored and the request after it is served from the cache.
#[rstest]
#[tokio::test]
async fn unstorable_response_does_not_mark_root_field_query_private(
    #[values(
        ToggledSubgraphResponse::PrivateStorableNoData,
        ToggledSubgraphResponse::PrivateDataWithErrors,
        ToggledSubgraphResponse::PrivateStorableWithErrors,
        ToggledSubgraphResponse::PrivateNullDataWithErrors,
        ToggledSubgraphResponse::PrivateHttp500,
        ToggledSubgraphResponse::PrivateMaxAgeZero,
        ToggledSubgraphResponse::PublicDataWithErrors,
        ToggledSubgraphResponse::NoStoreDataWithErrors
    )]
    failure: ToggledSubgraphResponse,
    #[values(false, true)] with_private_id: bool,
) {
    async move {
        let private_id = with_private_id.then_some("1234");
        let mut harness = ToggledSubgraphHarness::root_fields(with_private_id).await;

        harness.respond_with(failure);
        let entries = harness.call(private_id).await;
        assert_eq!(harness.subgraph_calls(), 1);
        assert!(
            entries.iter().all(|entry| !entry.should_store),
            "nothing was stored for a {failure:?} response, got: {entries:?}"
        );
        assert_gauge!("apollo.router.response_cache.private_queries.lru.size", 0);

        harness.assert_healthy_response_is_cached(private_id).await;
    }
    .with_metrics()
    .await;
}

/// Entity variant of `unstorable_response_does_not_mark_root_field_query_private`. A public
/// response with errors is left out: an error without an entity path does not stop the entity
/// from being stored.
#[rstest]
#[tokio::test]
async fn unstorable_response_does_not_mark_entity_query_private(
    #[values(
        ToggledSubgraphResponse::PrivateDataWithErrors,
        ToggledSubgraphResponse::PrivateStorableWithErrors,
        ToggledSubgraphResponse::PrivateNullDataWithErrors,
        ToggledSubgraphResponse::PrivateHttp500,
        ToggledSubgraphResponse::PrivateMaxAgeZero,
        ToggledSubgraphResponse::NoStoreDataWithErrors
    )]
    failure: ToggledSubgraphResponse,
) {
    async move {
        let mut harness = ToggledSubgraphHarness::entities(false).await;

        harness.respond_with(failure);
        let entries = harness.call(None).await;
        assert_eq!(harness.subgraph_calls(), 1);
        assert!(
            entries.iter().all(|entry| !entry.should_store),
            "nothing was stored for a {failure:?} response, got: {entries:?}"
        );
        assert_gauge!("apollo.router.response_cache.private_queries.lru.size", 0);

        harness.assert_healthy_response_is_cached(None).await;
    }
    .with_metrics()
    .await;
}

/// A storable private response marks the query as private. Without `private_id`, later
/// requests skip the cache, and the debugger reports that nothing was stored.
#[tokio::test]
async fn storable_private_response_marks_root_field_query_private() {
    async {
        let mut harness = ToggledSubgraphHarness::root_fields(false).await;

        harness.respond_with(ToggledSubgraphResponse::PrivateStorable);
        harness.call(None).await;
        assert_gauge!("apollo.router.response_cache.private_queries.lru.size", 1);

        harness.respond_with(ToggledSubgraphResponse::Healthy);
        for expected_calls in 2..=3 {
            let entries = harness.call(None).await;
            assert_eq!(harness.subgraph_calls(), expected_calls);
            assert_eq!(entries.len(), 1);
            assert_eq!(entries[0].key, "-", "the request should skip the cache");
            assert!(!entries[0].should_store);
        }
    }
    .with_metrics()
    .await;
}

/// Entity variant of `storable_private_response_marks_root_field_query_private`, which also
/// checks that the entity path counts its entry in the known-private queries gauge.
#[tokio::test]
async fn storable_private_response_marks_entity_query_private() {
    async {
        let mut harness = ToggledSubgraphHarness::entities(false).await;

        harness.respond_with(ToggledSubgraphResponse::PrivateStorable);
        harness.call(None).await;
        assert_gauge!("apollo.router.response_cache.private_queries.lru.size", 1);

        harness.respond_with(ToggledSubgraphResponse::Healthy);
        for expected_calls in 2..=3 {
            let entries = harness.call(None).await;
            assert_eq!(harness.subgraph_calls(), expected_calls);
            assert_eq!(entries.len(), 1);
            assert_eq!(entries[0].key, "-", "the request should skip the cache");
            assert!(!entries[0].should_store);
        }
    }
    .with_metrics()
    .await;
}

/// A response with errors is not stored, and the debugger says so even though its
/// `Cache-Control` would allow storing.
#[tokio::test]
async fn response_with_errors_is_not_reported_as_stored() {
    let mut harness = ToggledSubgraphHarness::root_fields(false).await;

    harness.respond_with(ToggledSubgraphResponse::PublicDataWithErrors);
    let entries = harness.call(None).await;
    assert_eq!(entries.len(), 1);
    assert!(!entries[0].should_store);

    harness.assert_healthy_response_is_cached(None).await;
}

/// In an entity batch, only the entity an error points at is reported as not stored.
#[tokio::test]
async fn entity_with_error_is_not_reported_as_stored() {
    let valid_schema = Arc::new(Schema::parse_and_validate(SCHEMA, "test.graphql").unwrap());
    let subgraphs = MockedSubgraphs([
        ("user", MockSubgraph::builder().with_json(
            serde_json::json! {{"query":"{currentUser{allOrganizations{__typename id}}}"}},
            serde_json::json! {{"data": {"currentUser": { "allOrganizations": [
                { "__typename": "Organization", "id": "1" },
                { "__typename": "Organization", "id": "3" }
            ] }}}},
        ).with_header(CACHE_CONTROL, HeaderValue::from_static("public, max-age=600")).build()),
        ("orga", MockSubgraph::builder().with_json(
            serde_json::json! {{
                "query": "query($representations:[_Any!]!){_entities(representations:$representations){...on Organization{name}}}",
                "variables": { "representations": [
                    { "id": "1", "__typename": "Organization" },
                    { "id": "3", "__typename": "Organization" }
                ]}
            }},
            serde_json::json! {{
                "data": { "_entities": [{ "name": null }, { "name": "Organization 3" }] },
                "errors": [{ "message": "subgraph failure", "path": ["_entities", 0] }]
            }},
        ).with_header(CACHE_CONTROL, HeaderValue::from_static("public, max-age=600")).build()),
    ].into_iter().collect());

    let (drop_tx, drop_rx) = tokio::sync::broadcast::channel(2);
    let storage = Storage::new(&Config::test(false, &Uuid::new_v4().to_string()), drop_rx)
        .await
        .unwrap();
    let subgraphs_conf = create_subgraph_conf(HashMap::from([
        ("user".to_string(), Subgraph::default()),
        ("orga".to_string(), Subgraph::default()),
    ]));
    let response_cache =
        ResponseCache::for_test(storage, subgraphs_conf, valid_schema, true, drop_tx, true)
            .await
            .unwrap();
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
        .query("query { currentUser { allOrganizations { id name } } }")
        .header(
            HeaderName::from_static(CACHE_DEBUG_HEADER_NAME),
            HeaderValue::from_static("true"),
        )
        .build()
        .unwrap();
    let response = service.oneshot(request).await.unwrap();

    let should_store_by_id: HashMap<String, bool> = get_cache_keys_context(&response)
        .expect("missing cache keys")
        .into_iter()
        .filter_map(|entry| match entry.kind {
            CacheEntryKind::Entity { entity_key, .. } => Some((
                entity_key.get("id")?.as_str()?.to_string(),
                entry.should_store,
            )),
            CacheEntryKind::RootFields { .. } => None,
        })
        .collect();
    assert_eq!(
        should_store_by_id,
        HashMap::from([("1".to_string(), false), ("3".to_string(), true)])
    );
}

/// With `private_id`, a storable private response is cached for that user only.
#[rstest]
#[case::root_fields(false)]
#[case::entities(true)]
#[tokio::test]
async fn storable_private_response_is_cached_per_user_with_private_id(#[case] entity_fetch: bool) {
    async move {
        let mut harness = if entity_fetch {
            ToggledSubgraphHarness::entities(true).await
        } else {
            ToggledSubgraphHarness::root_fields(true).await
        };

        harness.respond_with(ToggledSubgraphResponse::PrivateStorable);
        let entries = harness.call(Some("1234")).await;
        assert_gauge!("apollo.router.response_cache.private_queries.lru.size", 1);
        assert!(entries[0].should_store);
        wait_for_cache(&harness.storage, expected_cached_keys(&entries)).await;

        harness.call(Some("1234")).await;
        assert_eq!(
            harness.subgraph_calls(),
            1,
            "the same user is served from the cache"
        );

        harness.call(Some("5678")).await;
        assert_eq!(harness.subgraph_calls(), 2, "another user is not");
    }
    .with_metrics()
    .await;
}
