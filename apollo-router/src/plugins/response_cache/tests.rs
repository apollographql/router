#![cfg(test)]
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use apollo_compiler::Schema;
use futures::StreamExt;
use http::HeaderName;
use http::HeaderValue;
use http::header::CACHE_CONTROL;
use tokio_stream::wrappers::IntervalStream;
use tower::Service;
use tower::ServiceExt;
use uuid::Uuid;
use wiremock::Mock;
use wiremock::MockServer;
use wiremock::ResponseTemplate;
use wiremock::matchers::method;
use wiremock::matchers::path;

use super::plugin::ResponseCache;
use crate::Configuration;
use crate::Context;
use crate::MockedSubgraphs;
use crate::TestHarness;
use crate::configuration::subgraph::SubgraphConfiguration;
use crate::graphql;
use crate::json_ext::ValueExt;
use crate::metrics::FutureMetricsExt;
use crate::plugin::test::MockSubgraph;
use crate::plugin::test::MockSubgraphService;
use crate::plugins::response_cache::debugger::CacheKeysContext;
use crate::plugins::response_cache::invalidation::InvalidationRequest;
use crate::plugins::response_cache::invalidation_endpoint::SubgraphInvalidationConfig;
use crate::plugins::response_cache::plugin::CACHE_DEBUG_HEADER_NAME;
use crate::plugins::response_cache::plugin::CONTEXT_CACHE_KEY;
use crate::plugins::response_cache::plugin::INVALIDATION_SHARED_KEY;
use crate::plugins::response_cache::plugin::Subgraph;
use crate::plugins::response_cache::storage::CacheStorage;
use crate::plugins::response_cache::storage::redis::Config;
use crate::plugins::response_cache::storage::redis::Storage;
use crate::router_factory::RouterSuperServiceFactory;
use crate::router_factory::YamlRouterFactory;
use crate::services::new_service::ServiceFactory;
use crate::services::subgraph;
use crate::services::supergraph;
use crate::uplink::license_enforcement::LicenseState;

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
        ck.cache_control.set_created(0);
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
    )
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
            ResponseCache::for_test(storage.clone(), subgraphs_conf, valid_schema.clone(), true, drop_tx)
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
            ResponseCache::for_test(storage.clone(), subgraphs_conf, valid_schema.clone(), true, drop_tx)
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
            ResponseCache::for_test(storage.clone(), subgraphs_conf, valid_schema.clone(), true, drop_tx)
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
            ResponseCache::for_test(storage.clone(), subgraphs_conf, valid_schema.clone(), true, drop_tx)
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
            ResponseCache::for_test(storage.clone(), subgraphs_conf, valid_schema.clone(), true, drop_tx)
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
            ResponseCache::for_test(storage.clone(), subgraphs_conf, valid_schema.clone(), true, drop_tx)
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
                    tokio::time::sleep(Duration::from_secs(2)).await;
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

// ================================================================================================
// Connector response cache integration tests
// ================================================================================================

const CONNECTOR_SCHEMA: &str = include_str!("../../testdata/connector_response_cache.graphql");

/// Helper to create a router service with connector caching enabled via YamlRouterFactory.
///
/// We cannot use TestHarness because connectors are extracted during YamlRouterFactory
/// initialization, not during TestHarness construction.
async fn create_connector_cache_service(
    connector_uri: &str,
    namespace: &str,
    extra_config: Option<serde_json_bytes::Value>,
) -> impl tower::Service<
    crate::services::router::Request,
    Response = crate::services::router::Response,
    Error = tower::BoxError,
> {
    let connector_url = format!("{connector_uri}/");

    let mut config = serde_json_bytes::json!({
        "include_subgraph_errors": { "all": true },
        "connectors": {
            "sources": {
                "connectors.json": {
                    "override_url": connector_url
                }
            }
        },
        "response_cache": {
            "enabled": true,
            "debug": true,
            "connector": {
                "all": {
                    "enabled": true,
                    "redis": {
                        "urls": ["redis://127.0.0.1:6379"],
                        "pool_size": 1,
                        "namespace": namespace,
                        "required_to_start": true,
                    },
                    "ttl": "10m",
                }
            }
        }
    });

    if let Some(extra) = extra_config {
        config.deep_merge(extra);
    }

    let config: Configuration = serde_json_bytes::from_value(config).unwrap();
    let mut factory = YamlRouterFactory;
    let router_creator = factory
        .create(
            false,
            Arc::new(config.clone()),
            Arc::new(crate::spec::Schema::parse(CONNECTOR_SCHEMA, &config).unwrap()),
            None,
            None,
            Arc::new(LicenseState::Licensed { limits: None }),
        )
        .await
        .unwrap();

    router_creator.create()
}

/// Make a supergraph query request with cache debug header enabled.
fn make_connector_cache_request(query: &str) -> crate::services::router::Request {
    make_connector_cache_request_with_cache_control(query, None)
}

/// Make a supergraph query request WITHOUT the cache debug header.
fn make_connector_cache_request_no_debug(query: &str) -> crate::services::router::Request {
    supergraph::Request::fake_builder()
        .query(query)
        .context(Context::new())
        .build()
        .unwrap()
        .try_into()
        .unwrap()
}

fn make_connector_cache_request_with_cache_control(
    query: &str,
    cache_control: Option<&str>,
) -> crate::services::router::Request {
    let mut builder = supergraph::Request::fake_builder()
        .query(query)
        .context(Context::new())
        .header(
            HeaderName::from_static(CACHE_DEBUG_HEADER_NAME),
            HeaderValue::from_static("true"),
        );
    if let Some(cc) = cache_control {
        builder = builder.header(CACHE_CONTROL, HeaderValue::from_str(cc).unwrap());
    }
    builder.build().unwrap().try_into().unwrap()
}

/// Extract the response body as a JSON value from a router response.
async fn connector_response_body(
    mut response: crate::services::router::Response,
) -> serde_json::Value {
    let bytes = response.next_response().await.unwrap().unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

#[tokio::test]
async fn connector_root_field_cache_miss_then_hit() {
    let mock_server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/users"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("cache-control", "public, max-age=300")
                .set_body_json(serde_json::json!([
                    {"id": 1, "name": "Alice"},
                    {"id": 2, "name": "Bob"}
                ])),
        )
        .mount(&mock_server)
        .await;

    let uri = mock_server.uri();
    let namespace = Uuid::new_v4().to_string();

    let service = create_connector_cache_service(&uri, &namespace, None).await;

    // First request: cache miss
    let request = make_connector_cache_request("query { users { id name } }");
    let response = service.oneshot(request).await.unwrap();
    let body = connector_response_body(response).await;
    assert!(
        body.get("data").is_some(),
        "first request should return data, got: {body:?}"
    );
    let received_after_first = mock_server.received_requests().await.unwrap().len();
    assert!(
        received_after_first >= 1,
        "mock server should have received at least 1 request"
    );

    // Wait for async cache insert
    tokio::time::sleep(Duration::from_secs(2)).await;

    // Second request: cache hit — recreate service to ensure no in-memory state
    let service = create_connector_cache_service(&uri, &namespace, None).await;
    let request = make_connector_cache_request("query { users { id name } }");
    let response = service.oneshot(request).await.unwrap();
    let body = connector_response_body(response).await;
    assert!(
        body.get("data").is_some(),
        "second request should return data, got: {body:?}"
    );

    // Wiremock should NOT have received a new request for /users (served from cache)
    let received_after_second = mock_server.received_requests().await.unwrap().len();
    assert_eq!(
        received_after_first, received_after_second,
        "second request should be served from cache, but mock received new requests"
    );
}

#[tokio::test]
async fn connector_root_field_no_store() {
    let mock_server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/users"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("cache-control", "no-store")
                .set_body_json(serde_json::json!([
                    {"id": 1, "name": "Alice"}
                ])),
        )
        .mount(&mock_server)
        .await;

    let uri = mock_server.uri();
    let namespace = Uuid::new_v4().to_string();

    let service = create_connector_cache_service(&uri, &namespace, None).await;

    // First request
    let request = make_connector_cache_request("query { users { id name } }");
    let _response = service.oneshot(request).await.unwrap();
    let received_after_first = mock_server.received_requests().await.unwrap().len();

    tokio::time::sleep(Duration::from_secs(2)).await;

    // Second request should NOT be cached due to no-store
    let service = create_connector_cache_service(&uri, &namespace, None).await;
    let request = make_connector_cache_request("query { users { id name } }");
    let _response = service.oneshot(request).await.unwrap();
    let received_after_second = mock_server.received_requests().await.unwrap().len();

    assert!(
        received_after_second > received_after_first,
        "no-store responses should not be cached, but no new request was made"
    );
}

#[tokio::test]
async fn connector_entity_cache_miss_then_hit() {
    let mock_server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/users/1"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("cache-control", "public, max-age=300")
                .set_body_json(serde_json::json!({"id": 1, "name": "Alice"})),
        )
        .mount(&mock_server)
        .await;

    let uri = mock_server.uri();
    let namespace = Uuid::new_v4().to_string();

    let service = create_connector_cache_service(&uri, &namespace, None).await;

    // First request: cache miss
    let request = make_connector_cache_request("query { user(id: \"1\") { id name } }");
    let response = service.oneshot(request).await.unwrap();
    let body = connector_response_body(response).await;
    assert!(
        body.get("data").is_some(),
        "first request should return data, got: {body:?}"
    );
    let received_after_first = mock_server.received_requests().await.unwrap().len();
    assert!(
        received_after_first >= 1,
        "mock server should have received at least 1 request"
    );

    // Wait for async cache insert
    tokio::time::sleep(Duration::from_secs(2)).await;

    // Second request: cache hit
    let service = create_connector_cache_service(&uri, &namespace, None).await;
    let request = make_connector_cache_request("query { user(id: \"1\") { id name } }");
    let response = service.oneshot(request).await.unwrap();
    let body = connector_response_body(response).await;
    assert!(
        body.get("data").is_some(),
        "second request should return data, got: {body:?}"
    );

    let received_after_second = mock_server.received_requests().await.unwrap().len();
    assert_eq!(
        received_after_first, received_after_second,
        "second request should be served from cache, but mock received new requests"
    );
}

#[tokio::test]
async fn connector_cache_disabled() {
    let mock_server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/users"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("cache-control", "public, max-age=300")
                .set_body_json(serde_json::json!([
                    {"id": 1, "name": "Alice"}
                ])),
        )
        .mount(&mock_server)
        .await;

    // Disable connector caching explicitly
    let extra_config = serde_json_bytes::json!({
        "response_cache": {
            "connector": {
                "all": {
                    "enabled": false,
                }
            }
        }
    });

    let uri = mock_server.uri();
    let namespace = Uuid::new_v4().to_string();

    let service =
        create_connector_cache_service(&uri, &namespace, Some(extra_config.clone())).await;

    // First request
    let request = make_connector_cache_request("query { users { id name } }");
    let _response = service.oneshot(request).await.unwrap();
    let received_after_first = mock_server.received_requests().await.unwrap().len();

    tokio::time::sleep(Duration::from_secs(2)).await;

    // Second request should still hit the backend (caching disabled)
    let service = create_connector_cache_service(&uri, &namespace, Some(extra_config)).await;
    let request = make_connector_cache_request("query { users { id name } }");
    let _response = service.oneshot(request).await.unwrap();
    let received_after_second = mock_server.received_requests().await.unwrap().len();

    assert!(
        received_after_second > received_after_first,
        "with caching disabled, every request should hit the backend"
    );
}

#[tokio::test]
async fn connector_root_field_with_cache_tag() {
    let mock_server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/users"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("cache-control", "public, max-age=300")
                .set_body_json(serde_json::json!([
                    {"id": 1, "name": "Alice"}
                ])),
        )
        .mount(&mock_server)
        .await;

    let uri = mock_server.uri();
    let namespace = Uuid::new_v4().to_string();

    let service = create_connector_cache_service(&uri, &namespace, None).await;

    // Execute query — the schema has @cacheTag(format: "users") on the users field
    let request = make_connector_cache_request("query { users { id name } }");
    let response = service.oneshot(request).await.unwrap();
    let body = connector_response_body(response).await;
    assert!(
        body.get("data").is_some(),
        "request should return data, got: {body:?}"
    );

    // Wait for async cache insert
    tokio::time::sleep(Duration::from_secs(2)).await;

    // Verify it was cached by checking second request doesn't hit mock
    let service = create_connector_cache_service(&uri, &namespace, None).await;
    let request = make_connector_cache_request("query { users { id name } }");
    let response = service.oneshot(request).await.unwrap();
    let body = connector_response_body(response).await;
    assert!(
        body.get("data").is_some(),
        "cached request should return data, got: {body:?}"
    );
}

/// Request with `Cache-Control: no-store` should allow cache lookup but prevent storing.
#[tokio::test]
async fn connector_root_field_request_no_store() {
    let mock_server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/users"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("cache-control", "public, max-age=300")
                .set_body_json(serde_json::json!([
                    {"id": 1, "name": "Alice"}
                ])),
        )
        .mount(&mock_server)
        .await;

    let uri = mock_server.uri();
    let namespace = Uuid::new_v4().to_string();

    // First request WITH no-store: response should NOT be cached
    let service = create_connector_cache_service(&uri, &namespace, None).await;
    let request = make_connector_cache_request_with_cache_control(
        "query { users { id name } }",
        Some("no-store"),
    );
    let response = service.oneshot(request).await.unwrap();
    let body = connector_response_body(response).await;
    assert!(
        body.get("data").is_some(),
        "first request should return data, got: {body:?}"
    );
    let received_after_first = mock_server.received_requests().await.unwrap().len();

    tokio::time::sleep(Duration::from_secs(2)).await;

    // Second request WITHOUT no-store: should be a cache miss since first request didn't store
    let service = create_connector_cache_service(&uri, &namespace, None).await;
    let request = make_connector_cache_request("query { users { id name } }");
    let _response = service.oneshot(request).await.unwrap();
    let received_after_second = mock_server.received_requests().await.unwrap().len();

    assert!(
        received_after_second > received_after_first,
        "no-store request should prevent caching, but second request was served from cache"
    );
}

/// Request with `Cache-Control: no-cache` should skip cache lookup but allow storing.
#[tokio::test]
async fn connector_root_field_request_no_cache() {
    let mock_server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/users"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("cache-control", "public, max-age=300")
                .set_body_json(serde_json::json!([
                    {"id": 1, "name": "Alice"}
                ])),
        )
        .mount(&mock_server)
        .await;

    let uri = mock_server.uri();
    let namespace = Uuid::new_v4().to_string();

    // First request with no-cache: should hit backend (skip cache), but store the response
    let service = create_connector_cache_service(&uri, &namespace, None).await;
    let request = make_connector_cache_request_with_cache_control(
        "query { users { id name } }",
        Some("no-cache"),
    );
    let response = service.oneshot(request).await.unwrap();
    let body = connector_response_body(response).await;
    assert!(
        body.get("data").is_some(),
        "first request should return data, got: {body:?}"
    );
    let received_after_first = mock_server.received_requests().await.unwrap().len();

    tokio::time::sleep(Duration::from_secs(2)).await;

    // Second request WITHOUT no-cache: should be served from cache (first request stored it)
    let service = create_connector_cache_service(&uri, &namespace, None).await;
    let request = make_connector_cache_request("query { users { id name } }");
    let _response = service.oneshot(request).await.unwrap();
    let received_after_second = mock_server.received_requests().await.unwrap().len();

    assert_eq!(
        received_after_first, received_after_second,
        "no-cache should still allow storing, so second request should be served from cache"
    );
}

/// Request with `Cache-Control: no-cache, no-store` should bypass cache entirely.
#[tokio::test]
async fn connector_root_field_request_no_cache_no_store() {
    let mock_server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/users"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("cache-control", "public, max-age=300")
                .set_body_json(serde_json::json!([
                    {"id": 1, "name": "Alice"}
                ])),
        )
        .mount(&mock_server)
        .await;

    let uri = mock_server.uri();
    let namespace = Uuid::new_v4().to_string();

    // First request with both no-cache and no-store: bypass cache entirely
    let service = create_connector_cache_service(&uri, &namespace, None).await;
    let request = make_connector_cache_request_with_cache_control(
        "query { users { id name } }",
        Some("no-cache, no-store"),
    );
    let response = service.oneshot(request).await.unwrap();
    let body = connector_response_body(response).await;
    assert!(
        body.get("data").is_some(),
        "first request should return data, got: {body:?}"
    );
    let received_after_first = mock_server.received_requests().await.unwrap().len();

    tokio::time::sleep(Duration::from_secs(2)).await;

    // Second request without cache-control: should be a cache miss (first didn't store)
    let service = create_connector_cache_service(&uri, &namespace, None).await;
    let request = make_connector_cache_request("query { users { id name } }");
    let _response = service.oneshot(request).await.unwrap();
    let received_after_second = mock_server.received_requests().await.unwrap().len();

    assert!(
        received_after_second > received_after_first,
        "no-cache+no-store should bypass cache entirely, but second request was served from cache"
    );
}

/// Entity query with `Cache-Control: no-cache` should skip cache lookup.
#[tokio::test]
async fn connector_entity_request_no_cache() {
    let mock_server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/users/1"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("cache-control", "public, max-age=300")
                .set_body_json(serde_json::json!({"id": 1, "name": "Alice"})),
        )
        .mount(&mock_server)
        .await;

    let uri = mock_server.uri();
    let namespace = Uuid::new_v4().to_string();

    // First request (no special headers): populates cache
    let service = create_connector_cache_service(&uri, &namespace, None).await;
    let request = make_connector_cache_request("query { user(id: \"1\") { id name } }");
    let response = service.oneshot(request).await.unwrap();
    let body = connector_response_body(response).await;
    assert!(
        body.get("data").is_some(),
        "first request should return data, got: {body:?}"
    );
    let received_after_first = mock_server.received_requests().await.unwrap().len();

    tokio::time::sleep(Duration::from_secs(2)).await;

    // Second request with no-cache: should skip cache and hit backend
    let service = create_connector_cache_service(&uri, &namespace, None).await;
    let request = make_connector_cache_request_with_cache_control(
        "query { user(id: \"1\") { id name } }",
        Some("no-cache"),
    );
    let _response = service.oneshot(request).await.unwrap();
    let received_after_second = mock_server.received_requests().await.unwrap().len();

    assert!(
        received_after_second > received_after_first,
        "no-cache request should skip cache lookup and hit backend"
    );
}

/// Entity query with `Cache-Control: no-store` should allow cache lookup but prevent storing.
#[tokio::test]
async fn connector_entity_request_no_store() {
    let mock_server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/users/1"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("cache-control", "public, max-age=300")
                .set_body_json(serde_json::json!({"id": 1, "name": "Alice"})),
        )
        .mount(&mock_server)
        .await;

    let uri = mock_server.uri();
    let namespace = Uuid::new_v4().to_string();

    // First request WITH no-store: response should NOT be cached
    let service = create_connector_cache_service(&uri, &namespace, None).await;
    let request = make_connector_cache_request_with_cache_control(
        "query { user(id: \"1\") { id name } }",
        Some("no-store"),
    );
    let response = service.oneshot(request).await.unwrap();
    let body = connector_response_body(response).await;
    assert!(
        body.get("data").is_some(),
        "first request should return data, got: {body:?}"
    );
    let received_after_first = mock_server.received_requests().await.unwrap().len();

    tokio::time::sleep(Duration::from_secs(2)).await;

    // Second request WITHOUT no-store: should be a cache miss since first request didn't store
    let service = create_connector_cache_service(&uri, &namespace, None).await;
    let request = make_connector_cache_request("query { user(id: \"1\") { id name } }");
    let _response = service.oneshot(request).await.unwrap();
    let received_after_second = mock_server.received_requests().await.unwrap().len();

    assert!(
        received_after_second > received_after_first,
        "no-store request should prevent caching, but second request was served from cache"
    );
}

/// When a connector returns `Cache-Control: private` and no `private_id` is configured,
/// the response must NOT be stored in cache (prevents cross-user cache pollution).
#[tokio::test]
async fn connector_root_field_private_no_id_not_stored() {
    let mock_server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/users"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("cache-control", "private, max-age=300")
                .set_body_json(serde_json::json!([
                    {"id": 1, "name": "Alice"}
                ])),
        )
        .mount(&mock_server)
        .await;

    let uri = mock_server.uri();
    let namespace = Uuid::new_v4().to_string();

    // No private_id configured — private responses must not be cached
    let service = create_connector_cache_service(&uri, &namespace, None).await;

    // First request
    let request = make_connector_cache_request("query { users { id name } }");
    let response = service.oneshot(request).await.unwrap();
    let body = connector_response_body(response).await;
    assert!(
        body.get("data").is_some(),
        "first request should return data, got: {body:?}"
    );
    let received_after_first = mock_server.received_requests().await.unwrap().len();

    tokio::time::sleep(Duration::from_secs(2)).await;

    // Second request: should NOT be served from cache (private without private_id)
    let service = create_connector_cache_service(&uri, &namespace, None).await;
    let request = make_connector_cache_request("query { users { id name } }");
    let _response = service.oneshot(request).await.unwrap();
    let received_after_second = mock_server.received_requests().await.unwrap().len();

    assert!(
        received_after_second > received_after_first,
        "private response without private_id should not be cached, but second request was served from cache"
    );
}

/// When a connector returns `Cache-Control: private` and no `private_id` is configured,
/// entity responses must NOT be stored in cache.
#[tokio::test]
async fn connector_entity_private_no_id_not_stored() {
    let mock_server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/users/1"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("cache-control", "private, max-age=300")
                .set_body_json(serde_json::json!({"id": 1, "name": "Alice"})),
        )
        .mount(&mock_server)
        .await;

    let uri = mock_server.uri();
    let namespace = Uuid::new_v4().to_string();

    // No private_id configured — private responses must not be cached
    let service = create_connector_cache_service(&uri, &namespace, None).await;

    // First request
    let request = make_connector_cache_request("query { user(id: \"1\") { id name } }");
    let response = service.oneshot(request).await.unwrap();
    let body = connector_response_body(response).await;
    assert!(
        body.get("data").is_some(),
        "first request should return data, got: {body:?}"
    );
    let received_after_first = mock_server.received_requests().await.unwrap().len();

    tokio::time::sleep(Duration::from_secs(2)).await;

    // Second request: should NOT be served from cache (private without private_id)
    let service = create_connector_cache_service(&uri, &namespace, None).await;
    let request = make_connector_cache_request("query { user(id: \"1\") { id name } }");
    let _response = service.oneshot(request).await.unwrap();
    let received_after_second = mock_server.received_requests().await.unwrap().len();

    assert!(
        received_after_second > received_after_first,
        "private entity response without private_id should not be cached, but second request was served from cache"
    );
}

#[tokio::test]
async fn connector_mutation_not_cached() {
    let mock_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/users"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("cache-control", "public, max-age=300")
                .set_body_json(serde_json::json!({"id": 1, "name": "Alice"})),
        )
        .mount(&mock_server)
        .await;

    let uri = mock_server.uri();
    let namespace = Uuid::new_v4().to_string();

    let service = create_connector_cache_service(&uri, &namespace, None).await;

    // First request: mutation
    let request =
        make_connector_cache_request("mutation { createUser(name: \"Alice\") { id name } }");
    let response = service.oneshot(request).await.unwrap();
    let body = connector_response_body(response).await;
    assert!(
        body.get("data").is_some(),
        "first mutation should return data, got: {body:?}"
    );
    let received_after_first = mock_server.received_requests().await.unwrap().len();

    tokio::time::sleep(Duration::from_secs(2)).await;

    // Second request: same mutation — should NOT be served from cache
    let service = create_connector_cache_service(&uri, &namespace, None).await;
    let request =
        make_connector_cache_request("mutation { createUser(name: \"Alice\") { id name } }");
    let response = service.oneshot(request).await.unwrap();
    let body = connector_response_body(response).await;
    assert!(
        body.get("data").is_some(),
        "second mutation should return data, got: {body:?}"
    );
    let received_after_second = mock_server.received_requests().await.unwrap().len();

    assert!(
        received_after_second > received_after_first,
        "mutation responses should not be cached, but second request was served from cache"
    );
}

#[tokio::test]
async fn connector_root_field_debug_requires_header() {
    let mock_server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/users"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("cache-control", "public, max-age=300")
                .set_body_json(serde_json::json!([
                    {"id": 1, "name": "Alice"},
                    {"id": 2, "name": "Bob"}
                ])),
        )
        .mount(&mock_server)
        .await;

    let uri = mock_server.uri();
    let namespace = Uuid::new_v4().to_string();

    // Request WITHOUT debug header — should not have apolloCacheDebugging extension
    let service = create_connector_cache_service(&uri, &namespace, None).await;
    let request = make_connector_cache_request_no_debug("query { users { id name } }");
    let response = service.oneshot(request).await.unwrap();
    let body = connector_response_body(response).await;
    assert!(
        body.get("data").is_some(),
        "request without debug header should return data, got: {body:?}"
    );
    assert!(
        body.pointer("/extensions/apolloCacheDebugging").is_none(),
        "response should NOT contain apolloCacheDebugging without the debug header, got: {body:?}"
    );

    tokio::time::sleep(Duration::from_secs(2)).await;

    // Request WITH debug header — should have apolloCacheDebugging extension (cache hit)
    let service = create_connector_cache_service(&uri, &namespace, None).await;
    let request = make_connector_cache_request("query { users { id name } }");
    let response = service.oneshot(request).await.unwrap();
    let body = connector_response_body(response).await;
    assert!(
        body.get("data").is_some(),
        "request with debug header should return data, got: {body:?}"
    );
    assert!(
        body.pointer("/extensions/apolloCacheDebugging").is_some(),
        "response should contain apolloCacheDebugging with the debug header, got: {body:?}"
    );
}

#[tokio::test]
async fn connector_entity_debug_requires_header() {
    let mock_server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/users/1"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("cache-control", "public, max-age=300")
                .set_body_json(serde_json::json!({"id": 1, "name": "Alice"})),
        )
        .mount(&mock_server)
        .await;

    let uri = mock_server.uri();
    let namespace = Uuid::new_v4().to_string();

    // Request WITHOUT debug header — should not have apolloCacheDebugging extension
    let service = create_connector_cache_service(&uri, &namespace, None).await;
    let request = make_connector_cache_request_no_debug("query { user(id: \"1\") { id name } }");
    let response = service.oneshot(request).await.unwrap();
    let body = connector_response_body(response).await;
    assert!(
        body.get("data").is_some(),
        "request without debug header should return data, got: {body:?}"
    );
    assert!(
        body.pointer("/extensions/apolloCacheDebugging").is_none(),
        "response should NOT contain apolloCacheDebugging without the debug header, got: {body:?}"
    );

    tokio::time::sleep(Duration::from_secs(2)).await;

    // Request WITH debug header — should have apolloCacheDebugging extension (cache hit)
    let service = create_connector_cache_service(&uri, &namespace, None).await;
    let request = make_connector_cache_request("query { user(id: \"1\") { id name } }");
    let response = service.oneshot(request).await.unwrap();
    let body = connector_response_body(response).await;
    assert!(
        body.get("data").is_some(),
        "request with debug header should return data, got: {body:?}"
    );
    assert!(
        body.pointer("/extensions/apolloCacheDebugging").is_some(),
        "response should contain apolloCacheDebugging with the debug header, got: {body:?}"
    );
}

/// A malformed `Cache-Control` header on a root field request should return a GraphQL error.
#[tokio::test]
async fn connector_root_field_invalid_cache_control_header() {
    let mock_server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/users"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("cache-control", "public, max-age=300")
                .set_body_json(serde_json::json!([
                    {"id": 1, "name": "Alice"}
                ])),
        )
        .mount(&mock_server)
        .await;

    let uri = mock_server.uri();
    let namespace = Uuid::new_v4().to_string();

    let service = create_connector_cache_service(&uri, &namespace, None).await;
    let request = make_connector_cache_request_with_cache_control(
        "query { users { id name } }",
        Some("max-age=notanumber"),
    );
    let response = service.oneshot(request).await.unwrap();
    let body = connector_response_body(response).await;

    let errors = body.get("errors").and_then(|e| e.as_array());
    assert!(
        errors.is_some_and(|errs| !errs.is_empty()),
        "response should contain errors for invalid cache-control header, got: {body:?}"
    );
    assert_eq!(
        errors.unwrap()[0]
            .pointer("/extensions/code")
            .and_then(|v| v.as_str()),
        Some("INVALID_CACHE_CONTROL_HEADER"),
        "error should have INVALID_CACHE_CONTROL_HEADER extension code, got: {body:?}"
    );

    let received = mock_server.received_requests().await.unwrap().len();
    assert_eq!(
        received, 0,
        "upstream should not be called when cache-control header is invalid"
    );
}

/// A malformed `Cache-Control` header on an entity query should return a GraphQL error.
#[tokio::test]
async fn connector_entity_invalid_cache_control_header() {
    let mock_server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/users/1"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("cache-control", "public, max-age=300")
                .set_body_json(serde_json::json!({"id": 1, "name": "Alice"})),
        )
        .mount(&mock_server)
        .await;

    let uri = mock_server.uri();
    let namespace = Uuid::new_v4().to_string();

    let service = create_connector_cache_service(&uri, &namespace, None).await;
    let request = make_connector_cache_request_with_cache_control(
        "query { user(id: \"1\") { id name } }",
        Some("max-age=notanumber"),
    );
    let response = service.oneshot(request).await.unwrap();
    let body = connector_response_body(response).await;

    let errors = body.get("errors").and_then(|e| e.as_array());
    assert!(
        errors.is_some_and(|errs| !errs.is_empty()),
        "response should contain errors for invalid cache-control header, got: {body:?}"
    );
    assert_eq!(
        errors.unwrap()[0]
            .pointer("/extensions/code")
            .and_then(|v| v.as_str()),
        Some("INVALID_CACHE_CONTROL_HEADER"),
        "error should have INVALID_CACHE_CONTROL_HEADER extension code, got: {body:?}"
    );

    let received = mock_server.received_requests().await.unwrap().len();
    assert_eq!(
        received, 0,
        "upstream should not be called when cache-control header is invalid"
    );
}

/// When a known-private root field query bypasses cache (no private_id configured),
/// a debug entry with `key: "-"` and `shouldStore: false` should appear in `apolloCacheDebugging`.
#[tokio::test]
async fn connector_root_field_private_debug_entry() {
    let mock_server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/users"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("cache-control", "private, max-age=300")
                .set_body_json(serde_json::json!([
                    {"id": 1, "name": "Alice"}
                ])),
        )
        .mount(&mock_server)
        .await;

    let uri = mock_server.uri();
    let namespace = Uuid::new_v4().to_string();

    // First request: populates the private query LRU
    let service = create_connector_cache_service(&uri, &namespace, None).await;
    let request = make_connector_cache_request("query { users { id name } }");
    let response = service.oneshot(request).await.unwrap();
    let body = connector_response_body(response).await;
    assert!(
        body.get("data").is_some(),
        "first request should return data, got: {body:?}"
    );

    tokio::time::sleep(Duration::from_secs(2)).await;

    // Second request: known-private bypass path should include debug entry
    let service = create_connector_cache_service(&uri, &namespace, None).await;
    let request = make_connector_cache_request("query { users { id name } }");
    let response = service.oneshot(request).await.unwrap();
    let body = connector_response_body(response).await;

    let debug_entries = body
        .pointer("/extensions/apolloCacheDebugging")
        .expect("response should contain apolloCacheDebugging extension");

    let entries = debug_entries
        .as_array()
        .expect("debug entries should be an array");
    let private_entry = entries
        .iter()
        .find(|e| e.pointer("/key").and_then(|v| v.as_str()) == Some("-"));
    assert!(
        private_entry.is_some(),
        "should have a debug entry with key '-' for the known-private bypass, got: {entries:?}"
    );

    let entry = private_entry.unwrap();
    assert_eq!(
        entry.pointer("/shouldStore").and_then(|v| v.as_bool()),
        Some(false),
        "known-private debug entry should have shouldStore: false, got: {entry:?}"
    );
}

/// When a known-private entity query bypasses cache (no private_id configured),
/// a debug entry with `key: "-"` and `shouldStore: false` should appear in `apolloCacheDebugging`.
#[tokio::test]
async fn connector_entity_private_debug_entry() {
    let mock_server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/users/1"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("cache-control", "private, max-age=300")
                .set_body_json(serde_json::json!({"id": 1, "name": "Alice"})),
        )
        .mount(&mock_server)
        .await;

    let uri = mock_server.uri();
    let namespace = Uuid::new_v4().to_string();

    // First request: populates the private query LRU
    let service = create_connector_cache_service(&uri, &namespace, None).await;
    let request = make_connector_cache_request("query { user(id: \"1\") { id name } }");
    let response = service.oneshot(request).await.unwrap();
    let body = connector_response_body(response).await;
    assert!(
        body.get("data").is_some(),
        "first request should return data, got: {body:?}"
    );

    tokio::time::sleep(Duration::from_secs(2)).await;

    // Second request: known-private bypass path should include debug entry
    let service = create_connector_cache_service(&uri, &namespace, None).await;
    let request = make_connector_cache_request("query { user(id: \"1\") { id name } }");
    let response = service.oneshot(request).await.unwrap();
    let body = connector_response_body(response).await;

    let debug_entries = body
        .pointer("/extensions/apolloCacheDebugging")
        .expect("response should contain apolloCacheDebugging extension");

    let entries = debug_entries
        .as_array()
        .expect("debug entries should be an array");
    let private_entry = entries
        .iter()
        .find(|e| e.pointer("/key").and_then(|v| v.as_str()) == Some("-"));
    assert!(
        private_entry.is_some(),
        "should have a debug entry with key '-' for the known-private bypass, got: {entries:?}"
    );

    let entry = private_entry.unwrap();
    assert_eq!(
        entry.pointer("/shouldStore").and_then(|v| v.as_bool()),
        Some(false),
        "known-private debug entry should have shouldStore: false, got: {entry:?}"
    );
}
