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
use crate::plugins::response_cache::debugger::CacheKeysContext;
use crate::plugins::response_cache::debugger::CdnInvalidationDebug;
use crate::plugins::response_cache::invalidation::InvalidationRequest;
use crate::plugins::response_cache::invalidation_endpoint::InvalidationIndexes;
use crate::plugins::response_cache::invalidation_endpoint::SubgraphInvalidationConfig;
use crate::plugins::response_cache::invalidation_labels::InvalidationLabels;
use crate::plugins::response_cache::metrics::CacheMetricContextKey;
use crate::plugins::response_cache::plugin::CACHE_DEBUG_HEADER_NAME;
use crate::plugins::response_cache::plugin::CONTEXT_CACHE_KEY;
use crate::plugins::response_cache::plugin::CdnInvalidationConfig;
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

/// Reads the CDN invalidation debug info from the *actual response body extension* a real
/// client receives (`apolloCacheDebugging.cdnInvalidation`), not just from context, so this
/// exercises the same serialization path as production. Consumes `response`'s body stream.
async fn get_cdn_invalidation_debug(
    mut response: supergraph::Response,
) -> Option<CdnInvalidationDebug> {
    let body = response.next_response().await?;
    let debugging = body
        .extensions
        .get(super::plugin::CACHE_DEBUG_EXTENSIONS_KEY)?;
    let cdn_invalidation = debugging.get("cdnInvalidation")?;
    serde_json_bytes::from_value(cdn_invalidation.clone()).ok()
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

fn get_cache_tag_header(response: &supergraph::Response) -> Option<Vec<String>> {
    let header = response.response.headers().get("Cache-Tag")?;
    Some(
        header
            .to_str()
            .unwrap()
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

    let drain_drivers = std::sync::Arc::new(std::sync::Mutex::new(Vec::<
        tokio::task::JoinHandle<()>,
    >::new()));
    let drain_drivers_clone = drain_drivers.clone();
    let service = TestHarness::builder()
        .configuration_json(serde_json::json!({"include_subgraph_errors": { "all": true } }))
        .unwrap()
        .schema(SCHEMA)
        .extra_private_plugin(response_cache)
        .subgraph_hook(move |name, service| {
            if name == "orga" {
                let (mock, mut handle) =
                    tower_test::mock::pair::<subgraph::Request, subgraph::Response>();
                let driver = tokio::spawn(async move {
                    while let Some((_req, responder)) = handle.next_request().await {
                        responder.send_error("orga not found");
                    }
                });
                drain_drivers_clone.lock().unwrap().push(driver);
                mock.boxed_clone()
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
          "message": "HTTP fetch failed: orga not found",
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
    for driver in std::sync::Arc::try_unwrap(drain_drivers)
        .unwrap()
        .into_inner()
        .unwrap()
    {
        crate::plugin::test::await_mock_driver(driver).await;
    }
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

/// Shared setup for the `cdn_invalidation_enabled` vs. `IndexMode::CacheTag` decoupling tests:
/// one query that touches both a root field with a schema-derived `@cacheTag` (`currentUser` on
/// the `user` subgraph, format `"currentUser"`) and an entity type with schema-derived
/// `@cacheTag`s (`Organization` on the `orga` subgraph, formats `"organization"` and
/// `"organizationid-id--{$key.id}"`). Both subgraphs share the same `cache_tag` index setting,
/// since the point here is proving the CDN header doesn't depend on that setting at all, not
/// exercising per-subgraph config resolution (already covered elsewhere).
///
/// `extension_tag`, when `Some`, adds a `__cacheTags` key to the `orga` mock's entity — the mock
/// harness's special-cased key (`mock_subgraphs/mod.rs:323`) that populates the subgraph
/// response's `apolloEntityCacheTags` extension, exercising `insert_entities_in_result`'s
/// extension-tag path rather than `extract_cache_keys`'s schema-directive path.
async fn setup_cdn_invalidation_gating_test(
    cache_tag_index_enabled: bool,
    cdn_invalidation_enabled: bool,
    extension_tag: Option<&str>,
) -> supergraph::Response {
    let valid_schema =
        Arc::new(Schema::parse_and_validate(SCHEMA_CACHE_TAG, "test.graphql").unwrap());
    // `creatorUser` is `@join__field(graph: ORGA)` only, so requesting it forces an actual
    // `_entities` fetch to `orga` — requesting only `id` (a key field) lets `user` answer the
    // whole query itself, with no entity fetch at all.
    let query = "query { currentUser { activeOrganization { ... on Organization { id creatorUser { __typename id } } } } }";
    let mut orga_entity = serde_json::json!({
        "__typename": "Organization",
        "id": "1",
        "creatorUser": {
            "__typename": "User",
            "id": 2
        }
    });
    if let Some(tag) = extension_tag {
        orga_entity
            .as_object_mut()
            .unwrap()
            .insert("__cacheTags".to_string(), serde_json::json!([tag]));
    }
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
            "entities": [orga_entity],
            "headers": {"cache-control": "public"},
        },
    });

    let (drop_tx, drop_rx) = tokio::sync::broadcast::channel(2);
    let storage = Storage::new(&Config::test(false, &Uuid::new_v4().to_string()), drop_rx)
        .await
        .unwrap();
    let subgraph_config = || Subgraph {
        redis: None,
        private_id: Some("sub".to_string()),
        enabled: true.into(),
        ttl: None,
        invalidation: Some(SubgraphInvalidationConfig {
            indexes: InvalidationIndexes {
                cache_tag: cache_tag_index_enabled,
                ..Default::default()
            },
            ..Default::default()
        }),
    };
    let map = [
        ("user".to_string(), subgraph_config()),
        ("orga".to_string(), subgraph_config()),
    ]
    .into_iter()
    .collect();
    let subgraphs_conf = create_subgraph_conf(map);
    let response_cache = ResponseCache::for_test_with_cdn_invalidation(
        storage.clone(),
        subgraphs_conf,
        valid_schema.clone(),
        true,
        drop_tx,
        true,
        CdnInvalidationConfig {
            enabled: cdn_invalidation_enabled,
            ..Default::default()
        },
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
        .extra_private_plugin(response_cache)
        .build_supergraph()
        .await
        .unwrap();

    let request = supergraph::Request::fake_builder()
        .query(query)
        .context(Context::new())
        .build()
        .unwrap();
    service.oneshot(request).await.unwrap()
}

#[rstest]
#[case::cdn_only_still_emits_header(false, true, true)]
#[case::redis_index_alone_does_not_emit_header(true, false, false)]
#[case::both_enabled(true, true, true)]
#[tokio::test(flavor = "multi_thread")]
async fn cdn_invalidation_header_gates_independently_of_cache_tag_index(
    #[case] cache_tag_index_enabled: bool,
    #[case] cdn_invalidation_enabled: bool,
    #[case] expect_header_present: bool,
) {
    async move {
        let response = setup_cdn_invalidation_gating_test(
            cache_tag_index_enabled,
            cdn_invalidation_enabled,
            None,
        )
        .await;
        let header = get_cache_tag_header(&response);

        assert_eq!(
            header.is_some(),
            expect_header_present,
            "cache_tag_index_enabled={cache_tag_index_enabled}, cdn_invalidation_enabled={cdn_invalidation_enabled}: got header {header:?}"
        );

        if let Some(header) = header {
            // Root's schema-derived tag (`user` subgraph) and the entity's schema-derived tags
            // (`orga` subgraph) should both be present regardless of `cache_tag_index_enabled` —
            // the CDN header only depends on `cdn_invalidation_enabled`.
            assert!(
                header.contains(&"currentUser".to_string()),
                "missing root tag in {header:?}"
            );
            assert!(
                header.contains(&"organization".to_string()),
                "missing entity tag in {header:?}"
            );
        }
    }
    .with_metrics()
    .await;
}

#[rstest]
#[case::cdn_only_still_emits_header(false, true, true)]
#[case::redis_index_alone_does_not_emit_header(true, false, false)]
#[tokio::test(flavor = "multi_thread")]
async fn cdn_invalidation_header_for_extension_tags_gates_independently_of_cache_tag_index(
    #[case] cache_tag_index_enabled: bool,
    #[case] cdn_invalidation_enabled: bool,
    #[case] expect_header_present: bool,
) {
    async move {
        // Exercises `insert_entities_in_result`'s `apolloEntityCacheTags`-extension path
        // specifically, as opposed to `extract_cache_keys`'s schema-directive path covered by
        // `cdn_invalidation_header_gates_independently_of_cache_tag_index` above.
        let response = setup_cdn_invalidation_gating_test(
            cache_tag_index_enabled,
            cdn_invalidation_enabled,
            Some("extension-tag"),
        )
        .await;
        let header = get_cache_tag_header(&response);

        assert_eq!(
            header.is_some(),
            expect_header_present,
            "cache_tag_index_enabled={cache_tag_index_enabled}, cdn_invalidation_enabled={cdn_invalidation_enabled}: got header {header:?}"
        );

        if let Some(header) = header {
            assert!(
                header.contains(&"extension-tag".to_string()),
                "missing apolloEntityCacheTags-derived tag in {header:?}"
            );
        }
    }
    .with_metrics()
    .await;
}

/// Regression test: fine-grained tags used to leak onto the live `InvalidationLabels` context
/// aggregator whenever the Redis `cache_tag` index was on, even with `cdn_invalidation.enabled:
/// false` — contradicting `CdnInvalidationConfig`'s documented contract that fine-grained tag
/// values only ever aggregate onto context when `cdn_invalidation.enabled` is `true`. Exercises
/// both the schema-directive path (root field, entity) and the extension-tag path.
#[tokio::test(flavor = "multi_thread")]
async fn fine_grained_tags_stay_off_context_when_cdn_invalidation_disabled() {
    async move {
        let response = setup_cdn_invalidation_gating_test(true, false, Some("extension-tag")).await;

        let invalidation_labels = InvalidationLabels::get_or_create(&response.context);
        assert!(
            invalidation_labels.tags.is_empty(),
            "expected no fine-grained tags on context when cdn_invalidation is disabled, got {:?}",
            invalidation_labels.tags
        );
        // The coarse subgraph/type fallback tiers are unconditional, regardless of
        // `cdn_invalidation.enabled` — only the fine-grained `tags` tier is gated.
        assert!(!invalidation_labels.subgraphs.is_empty());
        assert!(!invalidation_labels.types.is_empty());
    }
    .with_metrics()
    .await;
}

/// Verifies, end to end, that the router threads `maybe_emit_header`'s result into a
/// `CdnInvalidationDebug` on the request context for the cache debugger to read. This is the
/// only place truncation info is visible: it depends on the combined label set across every
/// entry a response touches, not any single one, so no per-entry `CacheKeyContext` could show it.
#[tokio::test(flavor = "multi_thread")]
async fn cdn_invalidation_debug_reflects_outcome_and_truncation() {
    async move {
        let valid_schema =
            Arc::new(Schema::parse_and_validate(SCHEMA_CACHE_TAG, "test.graphql").unwrap());
        let query = "query { currentUser { activeOrganization { ... on Organization { id creatorUser { __typename id } } } } }";
        let orga_entity = serde_json::json!({
            "__typename": "Organization",
            "id": "1",
            "creatorUser": {
                "__typename": "User",
                "id": 2
            }
        });
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
                "entities": [orga_entity],
                "headers": {"cache-control": "public"},
            },
        });
        let map = || {
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
            .collect()
        };
        let build_request = |send_debug_header: bool| {
            let builder = supergraph::Request::fake_builder()
                .query(query)
                .context(Context::new());
            if send_debug_header {
                builder
                    .header(
                        HeaderName::from_static(CACHE_DEBUG_HEADER_NAME),
                        HeaderValue::from_static("true"),
                    )
                    .build()
                    .unwrap()
            } else {
                builder.build().unwrap()
            }
        };
        let run = |cdn_invalidation: CdnInvalidationConfig, send_debug_header: bool| {
            let valid_schema = valid_schema.clone();
            let subgraphs = subgraphs.clone();
            async move {
                let (drop_tx, drop_rx) = tokio::sync::broadcast::channel(2);
                let storage = Storage::new(&Config::test(false, &Uuid::new_v4().to_string()), drop_rx)
                    .await
                    .unwrap();
                let response_cache = ResponseCache::for_test_with_cdn_invalidation(
                    storage,
                    create_subgraph_conf(map()),
                    valid_schema,
                    true,
                    drop_tx,
                    true,
                    cdn_invalidation,
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
                    .extra_private_plugin(response_cache)
                    .build_supergraph()
                    .await
                    .unwrap();
                service.oneshot(build_request(send_debug_header)).await.unwrap()
            }
        };

        // Generous max_bytes: nothing truncates.
        let response = run(
            CdnInvalidationConfig {
                enabled: true,
                ..Default::default()
            },
            true,
        )
        .await;
        let debug = get_cdn_invalidation_debug(response)
            .await
            .expect("expected CDN invalidation debug info");
        assert_eq!(debug.outcome, "complete_without_truncation");
        assert!(debug.emitted, "{debug:?}");
        assert!(debug.header_value.is_some(), "{debug:?}");
        assert!(debug.untruncated_size_bytes.is_some(), "{debug:?}");
        assert_eq!(debug.header_name, "Cache-Tag");

        // Tiny max_bytes: forces truncation down to nothing fitting, so the header is
        // suppressed even though the response touched real labels — outcome is
        // `complete_with_truncation` (not `empty`, since there was something to report), but
        // `emitted` is false since an empty-value header carries no purge capability and would
        // be indistinguishable from no header at all. The debug info still shows what the
        // (unsent) header value would have been, for troubleshooting.
        let response = run(
            CdnInvalidationConfig {
                enabled: true,
                max_bytes: 10,
                ..Default::default()
            },
            true,
        )
        .await;
        let debug = get_cdn_invalidation_debug(response)
            .await
            .expect("expected CDN invalidation debug info");
        assert_eq!(debug.outcome, "complete_with_truncation");
        assert!(
            !debug.emitted,
            "an empty-value header shouldn't be reported as emitted: {debug:?}"
        );
        let header_value = debug.header_value.clone().expect("expected a header value");
        assert_eq!(
            header_value, "",
            "expected nothing to have survived truncation at max_bytes=10: {debug:?}"
        );
        let untruncated = debug
            .untruncated_size_bytes
            .expect("expected an untruncated size");
        assert!(
            untruncated > 0,
            "expected a nonzero untruncated size: {debug:?}"
        );
        assert_eq!(debug.max_bytes, 10);

        // Disabled entirely: no debug info should be recorded at all.
        let response = run(
            CdnInvalidationConfig {
                enabled: false,
                ..Default::default()
            },
            true,
        )
        .await;
        assert!(
            get_cdn_invalidation_debug(response).await.is_none(),
            "expected no CDN invalidation debug info when the feature is disabled"
        );

        // Enabled, but this particular request didn't send the debug header: no debug info,
        // even though `response_cache.debug` is on at the config level (the test harness always
        // sets it). Debug info should only ever appear for requests that actually asked for it.
        let response = run(
            CdnInvalidationConfig {
                enabled: true,
                ..Default::default()
            },
            false,
        )
        .await;
        assert!(
            get_cdn_invalidation_debug(response).await.is_none(),
            "expected no CDN invalidation debug info when the request didn't ask for it"
        );
    }
    .with_metrics()
    .await;
}

/// Characterizes a documented limitation: the `Cache-Tag` header is built from whatever's
/// resolved by the time the router is ready to send the initial HTTP response, without waiting
/// on `@defer`-deferred payloads. `activeOrganization` is deferred here specifically because it
/// reads `currentUser`'s resolved reference to build its entity representation — that data
/// dependency is what reliably serializes the `orga` subgraph fetch behind the primary payload
/// finishing, which is why this test's outcome (the deferred `organization`/
/// `organizationid-id--*` tags absent from the header) isn't just incidental mock-latency luck.
/// A deferred fragment with no such dependency on the primary payload isn't covered by this test
/// and has no such ordering guarantee.
#[tokio::test(flavor = "multi_thread")]
async fn cdn_invalidation_header_excludes_deferred_payload_tags() {
    async move {
        let valid_schema =
            Arc::new(Schema::parse_and_validate(SCHEMA_CACHE_TAG, "test.graphql").unwrap());
        let query = "query { currentUser { id ... @defer { activeOrganization { ... on Organization { id creatorUser { __typename id } } } } } }";
        let subgraphs = serde_json::json!({
            "user": {
                "query": {
                    "currentUser": {
                        "id": "1",
                        "activeOrganization": {
                            "__typename": "Organization",
                            "id": "1",
                        }
                    }
                },
                "headers": {"cache-control": "public"},
            },
            "orga": {
                "entities": [serde_json::json!({
                    "__typename": "Organization",
                    "id": "1",
                    "creatorUser": {
                        "__typename": "User",
                        "id": 2
                    }
                })],
                "headers": {"cache-control": "public"},
            },
        });

        let (drop_tx, drop_rx) = tokio::sync::broadcast::channel(2);
        let storage = Storage::new(&Config::test(false, &Uuid::new_v4().to_string()), drop_rx)
            .await
            .unwrap();
        let subgraph_config = || Subgraph {
            redis: None,
            private_id: Some("sub".to_string()),
            enabled: true.into(),
            ttl: None,
            invalidation: None,
        };
        let map = [
            ("user".to_string(), subgraph_config()),
            ("orga".to_string(), subgraph_config()),
        ]
        .into_iter()
        .collect();
        let response_cache = ResponseCache::for_test_with_cdn_invalidation(
            storage,
            create_subgraph_conf(map),
            valid_schema,
            true,
            drop_tx,
            false,
            CdnInvalidationConfig {
                enabled: true,
                ..Default::default()
            },
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
            .extra_private_plugin(response_cache)
            .build_supergraph()
            .await
            .unwrap();

        let request = supergraph::Request::fake_builder()
            .query(query)
            .context(defer_context())
            .build()
            .unwrap();

        let mut response = service.oneshot(request).await.unwrap();
        let header = get_cache_tag_header(&response).expect("expected a Cache-Tag header");

        // The deferred fragment's own tags must never appear on the primary response's header.
        assert!(
            !header.contains(&"organization".to_string()),
            "deferred-only tag leaked into the primary response's header: {header:?}"
        );

        // Confirm the query really did defer: a second chunk with the deferred data follows.
        let primary = response.next_response().await.unwrap();
        assert_eq!(
            primary.has_next,
            Some(true),
            "expected the primary chunk to signal more chunks are coming"
        );
        let deferred = response
            .next_response()
            .await
            .expect("expected a second, deferred chunk");
        assert!(
            !deferred.incremental.is_empty(),
            "expected the deferred chunk to carry incremental data"
        );

        // The primary chunk's own tag (from `currentUser`'s root-field `@cacheTag`) should still
        // be present — only the deferred fragment's tags are excluded.
        assert!(
            header.contains(&"currentUser".to_string()),
            "missing primary-payload tag in {header:?}"
        );
    }
    .with_metrics()
    .await;
}

fn defer_context() -> Context {
    let context = Context::new();
    context.extensions().with_lock(|lock| {
        lock.insert(crate::services::router::ClientRequestAccepts {
            multipart_defer: true,
            ..Default::default()
        })
    });
    context
}

/// Like `setup_cdn_invalidation_gating_test`, but performs the request twice against the same
/// underlying storage: once to populate the cache (miss) and once more to hit it. The second
/// `TestHarness` has no mock subgraphs wired in at all (mirrors `insert()`'s miss-then-hit
/// pattern) — if the hit path doesn't actually serve from cache, the request has nothing to
/// answer it and fails outright, rather than silently emitting a header built from a live
/// subgraph call.
///
/// `cache_tag_index_enabled` controls the Redis `cache_tag` index independently of CDN
/// invalidation. Fine-grained CDN tags are persisted for a later cache hit based on their own
/// `cdn_invalidation_enabled` gate, not on Redis's `cache_tag` index — so passing `false` here
/// exercises CDN invalidation running entirely on its own, with Redis per-tag indexing off.
async fn setup_cdn_invalidation_hit_test(
    cache_tag_index_enabled: bool,
    extension_tag: Option<&str>,
) -> (supergraph::Response, supergraph::Response) {
    let valid_schema =
        Arc::new(Schema::parse_and_validate(SCHEMA_CACHE_TAG, "test.graphql").unwrap());
    let query = "query { currentUser { activeOrganization { ... on Organization { id creatorUser { __typename id } } } } }";
    let mut orga_entity = serde_json::json!({
        "__typename": "Organization",
        "id": "1",
        "creatorUser": {
            "__typename": "User",
            "id": 2
        }
    });
    if let Some(tag) = extension_tag {
        orga_entity
            .as_object_mut()
            .unwrap()
            .insert("__cacheTags".to_string(), serde_json::json!([tag]));
    }
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
            "entities": [orga_entity],
            "headers": {"cache-control": "public"},
        },
    });

    let (drop_tx, drop_rx) = tokio::sync::broadcast::channel(2);
    let storage = Storage::new(&Config::test(false, &Uuid::new_v4().to_string()), drop_rx)
        .await
        .unwrap();
    let subgraph_config = || Subgraph {
        redis: None,
        private_id: Some("sub".to_string()),
        enabled: true.into(),
        ttl: None,
        invalidation: Some(SubgraphInvalidationConfig {
            indexes: InvalidationIndexes {
                cache_tag: cache_tag_index_enabled,
                ..Default::default()
            },
            ..Default::default()
        }),
    };
    let map = [
        ("user".to_string(), subgraph_config()),
        ("orga".to_string(), subgraph_config()),
    ]
    .into_iter()
    .collect();
    let subgraphs_conf = create_subgraph_conf(map);
    let response_cache = ResponseCache::for_test_with_cdn_invalidation(
        storage.clone(),
        subgraphs_conf,
        valid_schema.clone(),
        true,
        drop_tx,
        true,
        CdnInvalidationConfig {
            enabled: true,
            ..Default::default()
        },
    )
    .await
    .unwrap();

    let build_request = || {
        supergraph::Request::fake_builder()
            .query(query)
            .context(Context::new())
            .header(
                HeaderName::from_static(CACHE_DEBUG_HEADER_NAME),
                HeaderValue::from_static("true"),
            )
            .build()
            .unwrap()
    };

    let miss_service = TestHarness::builder()
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

    let miss_response = miss_service.oneshot(build_request()).await.unwrap();
    let cache_keys = get_cache_keys_context(&miss_response).expect("missing cache keys");
    wait_for_cache(&storage, expected_cached_keys(&cache_keys)).await;

    // No `experimental_mock_subgraphs` here: if the hit path fell through to a live subgraph
    // call, there'd be nothing configured to answer it and the request would fail.
    let hit_service = TestHarness::builder()
        .configuration_json(serde_json::json!({"include_subgraph_errors": { "all": true } }))
        .unwrap()
        .schema(SCHEMA)
        .extra_private_plugin(response_cache)
        .build_supergraph()
        .await
        .unwrap();

    let hit_response = hit_service.oneshot(build_request()).await.unwrap();

    (miss_response, hit_response)
}

#[rstest]
#[case::redis_index_off(false)]
#[case::redis_index_on(true)]
#[tokio::test(flavor = "multi_thread")]
async fn cdn_invalidation_header_persists_schema_tags_across_cache_hit(
    #[case] cache_tag_index_enabled: bool,
) {
    async move {
        let (miss_response, hit_response) =
            setup_cdn_invalidation_hit_test(cache_tag_index_enabled, None).await;

        for (label, response) in [("miss", &miss_response), ("hit", &hit_response)] {
            let header = get_cache_tag_header(response).unwrap_or_else(|| {
                panic!(
                    "cache_tag_index_enabled={cache_tag_index_enabled}: expected a Cache-Tag header on the {label} response"
                )
            });

            // Root-field tag: recorded by `call_service_for_root_fields_operation`'s
            // cache-hit branch (`merge`) on the hit response, and `cache_lookup_root`'s miss
            // branch on the miss response.
            assert!(
                header.contains(&"currentUser".to_string()),
                "cache_tag_index_enabled={cache_tag_index_enabled}: {label} response missing root tag in {header:?}"
            );
            // Entity tags: recorded by the `cache_result.iter()` loop's `merge`/`add_tags`
            // backfill on hit (via `cdn_invalidation_tags`, independent of the Redis
            // `cache_tag` index), and `extract_cache_keys`'s schema-directive path on miss.
            assert!(
                header.contains(&"organization".to_string()),
                "cache_tag_index_enabled={cache_tag_index_enabled}: {label} response missing entity tag in {header:?}"
            );
            assert!(
                header.contains(&"organizationid-id--1".to_string()),
                "cache_tag_index_enabled={cache_tag_index_enabled}: {label} response missing interpolated entity tag in {header:?}"
            );
        }
    }
    .with_metrics()
    .await;
}

#[rstest]
#[case::redis_index_off(false)]
#[case::redis_index_on(true)]
#[tokio::test(flavor = "multi_thread")]
async fn cdn_invalidation_header_persists_extension_tag_across_cache_hit(
    #[case] cache_tag_index_enabled: bool,
) {
    async move {
        let (miss_response, hit_response) =
            setup_cdn_invalidation_hit_test(cache_tag_index_enabled, Some("extension-tag")).await;

        for (label, response) in [("miss", &miss_response), ("hit", &hit_response)] {
            let header = get_cache_tag_header(response).unwrap_or_else(|| {
                panic!(
                    "cache_tag_index_enabled={cache_tag_index_enabled}: expected a Cache-Tag header on the {label} response"
                )
            });
            assert!(
                header.contains(&"extension-tag".to_string()),
                "cache_tag_index_enabled={cache_tag_index_enabled}: {label} response missing apolloEntityCacheTags-derived tag in {header:?}"
            );
        }
    }
    .with_metrics()
    .await;
}

/// Regression coverage for a single request that mixes a cache hit and a cache miss for
/// entities of the *same* type: the `cache_result.iter()` loop in `insert_entities_in_result`
/// runs `merge`/`add_tags` for hits and the schema-directive path for misses, both writing
/// through the same per-request `InvalidationLabels` handle. Proves the header unions both
/// rather than one clobbering the other.
#[tokio::test(flavor = "multi_thread")]
async fn cdn_invalidation_header_unions_hit_and_miss_entities_in_the_same_request() {
    async move {
        let valid_schema =
            Arc::new(Schema::parse_and_validate(SCHEMA_CACHE_TAG, "test.graphql").unwrap());
        let query = "query { currentUser { allOrganizations { id name } } }";

        // Shared namespace so the second storage/service can see what the first cached —
        // mirrors `missing_entities`'s pre-warm-then-partially-miss setup.
        let namespace = Uuid::new_v4().to_string();
        let map = || {
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
            .collect()
        };

        // First request: warm the cache for orgs 1 and 2 only.
        {
            let subgraphs = MockedSubgraphs(
                [
                    (
                        "user",
                        MockSubgraph::builder()
                            .with_json(
                                serde_json::json! {{"query":"{currentUser{allOrganizations{__typename id}}}"}},
                                serde_json::json! {{"data": {"currentUser": { "allOrganizations": [
                                    {"__typename": "Organization", "id": "1"},
                                    {"__typename": "Organization", "id": "2"}
                                ] }}}},
                            )
                            .with_header(CACHE_CONTROL, HeaderValue::from_static("no-store"))
                            .build(),
                    ),
                    (
                        "orga",
                        MockSubgraph::builder()
                            .with_json(
                                serde_json::json! {{
                                    "query": "query($representations:[_Any!]!){_entities(representations:$representations){...on Organization{name}}}",
                                    "variables": {
                                        "representations": [
                                            {"id": "1", "__typename": "Organization"},
                                            {"id": "2", "__typename": "Organization"}
                                        ]
                                    }
                                }},
                                serde_json::json! {{
                                    "data": {
                                        "_entities": [
                                            {"name": "Organization 1"},
                                            {"name": "Organization 2"}
                                        ]
                                    }
                                }},
                            )
                            .with_header(CACHE_CONTROL, HeaderValue::from_static("public, max-age=3600"))
                            .build(),
                    ),
                ]
                .into_iter()
                .collect(),
            );

            let (drop_tx, drop_rx) = tokio::sync::broadcast::channel(2);
            let storage = Storage::new(&Config::test(false, &namespace), drop_rx)
                .await
                .unwrap();
            let response_cache = ResponseCache::for_test_with_cdn_invalidation(
                storage.clone(),
                create_subgraph_conf(map()),
                valid_schema.clone(),
                true,
                drop_tx,
                true,
                CdnInvalidationConfig {
                    enabled: true,
                    ..Default::default()
                },
            )
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
                .query(query)
                .context(Context::new())
                .header(
                    HeaderName::from_static(CACHE_DEBUG_HEADER_NAME),
                    HeaderValue::from_static("true"),
                )
                .build()
                .unwrap();
            let response = service.oneshot(request).await.unwrap();
            let cache_keys = get_cache_keys_context(&response).expect("missing cache keys");
            wait_for_cache(&storage, expected_cached_keys(&cache_keys)).await;
        }

        // Second request: orgs 1, 2, and 3. The `orga` mock below only has a registered response
        // for representation "3" — if the router doesn't actually serve 1 and 2 from cache, the
        // mock harness has no matching response for the unexpected extra representations and the
        // request fails outright, rather than silently succeeding via a live re-fetch.
        let subgraphs = MockedSubgraphs(
            [
                (
                    "user",
                    MockSubgraph::builder()
                        .with_json(
                            serde_json::json! {{"query":"{currentUser{allOrganizations{__typename id}}}"}},
                            serde_json::json! {{"data": {"currentUser": { "allOrganizations": [
                                {"__typename": "Organization", "id": "1"},
                                {"__typename": "Organization", "id": "2"},
                                {"__typename": "Organization", "id": "3"}
                            ] }}}},
                        )
                        .with_header(CACHE_CONTROL, HeaderValue::from_static("no-store"))
                        .build(),
                ),
                (
                    "orga",
                    MockSubgraph::builder()
                        .with_json(
                            serde_json::json! {{
                                "query": "query($representations:[_Any!]!){_entities(representations:$representations){...on Organization{name}}}",
                                "variables": {
                                    "representations": [
                                        {"id": "3", "__typename": "Organization"}
                                    ]
                                }
                            }},
                            serde_json::json! {{
                                "data": {
                                    "_entities": [
                                        {"name": "Organization 3"}
                                    ]
                                }
                            }},
                        )
                        .with_header(CACHE_CONTROL, HeaderValue::from_static("public, max-age=3600"))
                        .build(),
                ),
            ]
            .into_iter()
            .collect(),
        );

        let (drop_tx, drop_rx) = tokio::sync::broadcast::channel(2);
        let storage = Storage::new(&Config::test(false, &namespace), drop_rx)
            .await
            .unwrap();
        let response_cache = ResponseCache::for_test_with_cdn_invalidation(
            storage.clone(),
            create_subgraph_conf(map()),
            valid_schema.clone(),
            false,
            drop_tx,
            true,
            CdnInvalidationConfig {
                enabled: true,
                ..Default::default()
            },
        )
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
            .query(query)
            .context(Context::new())
            .build()
            .unwrap();
        let response = service.oneshot(request).await.unwrap();
        let header = get_cache_tag_header(&response).expect("expected a Cache-Tag header");

        assert!(
            header.contains(&"currentUser".to_string()),
            "missing root tag in {header:?}"
        );
        assert!(
            header.contains(&"organizationid-id--1".to_string()),
            "missing hit-path entity tag (org 1) in {header:?}"
        );
        assert!(
            header.contains(&"organizationid-id--2".to_string()),
            "missing hit-path entity tag (org 2) in {header:?}"
        );
        assert!(
            header.contains(&"organizationid-id--3".to_string()),
            "missing miss-path entity tag (org 3) in {header:?}"
        );
    }
    .with_metrics()
    .await;
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
                .boxed_clone()
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
