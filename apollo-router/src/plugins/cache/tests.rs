use std::collections::HashMap;
use std::sync::Arc;

use apollo_compiler::Schema;
use bytes::Bytes;
use fred::error::ErrorKind as RedisErrorKind;
use fred::mocks::MockCommand;
use fred::mocks::Mocks;
use fred::prelude::Error as RedisError;
use fred::prelude::Value as RedisValue;
use http::HeaderValue;
use http::header::CACHE_CONTROL;
use parking_lot::Mutex;
use serde_json_bytes::ByteString;
use tower::Service;
use tower::ServiceExt;

use super::entity::EntityCache;
use crate::Context;
use crate::MockedSubgraphs;
use crate::TestHarness;
use crate::cache::redis::RedisCacheStorage;
use crate::plugin::test::MockSubgraph;
use crate::plugin::test::MockSubgraphService;
use crate::plugins::cache::entity::CONTEXT_CACHE_KEYS;
use crate::plugins::cache::entity::CacheKeyContext;
use crate::plugins::cache::entity::CacheKeysContext;
use crate::plugins::cache::entity::Subgraph;
use crate::plugins::cache::entity::hash_representation;
use crate::services::subgraph;
use crate::services::supergraph;

pub(super) const SCHEMA: &str = include_str!("../../testdata/orga_supergraph.graphql");
const SCHEMA_REQUIRES: &str = include_str!("../../testdata/supergraph.graphql");
const SCHEMA_NESTED_KEYS: &str = include_str!("../../testdata/supergraph_nested_fields.graphql");
#[derive(Debug)]
pub(crate) struct MockStore {
    map: Arc<Mutex<HashMap<Bytes, Bytes>>>,
}

impl MockStore {
    pub(crate) fn new() -> MockStore {
        MockStore {
            map: Arc::new(Mutex::new(HashMap::new())),
        }
    }
}

impl Mocks for MockStore {
    fn process_command(&self, command: MockCommand) -> Result<RedisValue, RedisError> {
        println!("mock received redis command: {command:?}");

        match &*command.cmd {
            "GET" => {
                if let Some(RedisValue::Bytes(b)) = command.args.first()
                    && let Some(bytes) = self.map.lock().get(b)
                {
                    println!("-> returning {:?}", std::str::from_utf8(bytes));
                    return Ok(RedisValue::Bytes(bytes.clone()));
                }
            }
            "MGET" => {
                let mut result: Vec<RedisValue> = Vec::new();

                let mut args_it = command.args.iter();
                while let Some(RedisValue::Bytes(key)) = args_it.next() {
                    if let Some(bytes) = self.map.lock().get(key) {
                        result.push(RedisValue::Bytes(bytes.clone()));
                    } else {
                        result.push(RedisValue::Null);
                    }
                }
                return Ok(RedisValue::Array(result));
            }
            "SET" => {
                if let (Some(RedisValue::Bytes(key)), Some(RedisValue::Bytes(value))) =
                    (command.args.first(), command.args.get(1))
                {
                    self.map.lock().insert(key.clone(), value.clone());
                    return Ok(RedisValue::Null);
                }
            }
            "MSET" => {
                let mut args_it = command.args.iter();
                while let (Some(RedisValue::Bytes(key)), Some(RedisValue::Bytes(value))) =
                    (args_it.next(), args_it.next())
                {
                    self.map.lock().insert(key.clone(), value.clone());
                }
                return Ok(RedisValue::Null);
            }
            //FIXME: this is not working because fred's mock never sends the response to SCAN to the client
            /*"SCAN" => {
                let mut args_it = command.args.iter();
                if let (
                    Some(RedisValue::String(cursor)),
                    Some(RedisValue::String(_match)),
                    Some(RedisValue::String(pattern)),
                    Some(RedisValue::String(_count)),
                    Some(RedisValue::Integer(max_count)),
                ) = (
                    args_it.next(),
                    args_it.next(),
                    args_it.next(),
                    args_it.next(),
                    args_it.next(),
                ) {
                    let cursor: usize = cursor.parse().unwrap();

                    if cursor > self.map.lock().len() {
                        let res = RedisValue::Array(vec![
                            RedisValue::String(0.to_string().into()),
                            RedisValue::Array(Vec::new()),
                        ]);
                        println!("result: {res:?}");

                        return Ok(res);
                    }

                    let regex = Regex::new(pattern).unwrap();
                    let mut count = 0;
                    let res: Vec<_> = self
                        .map
                        .lock()
                        .keys()
                        .enumerate()
                        .skip(cursor)
                        .map(|(i, key)| {
                            println!("seen key at index {i}");
                            count = i + 1;
                            key
                        })
                        .filter(|key| regex.is_match(&*key))
                        .map(|key| RedisValue::Bytes(key.clone()))
                        .take(*max_count as usize)
                        .collect();

                    println!("scan returns cursor {count}, for {} values", res.len());
                    let res = RedisValue::Array(vec![
                        RedisValue::String(count.to_string().into()),
                        RedisValue::Array(res),
                    ]);
                    println!("result: {res:?}");

                    return Ok(res);
                } else {
                    panic!()
                }
            }*/
            _ => {
                panic!("unrecoginzed command: {command:?}")
            }
        }
        Err(RedisError::new(RedisErrorKind::NotFound, "mock not found"))
    }
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

    let redis_cache = RedisCacheStorage::from_mocks(Arc::new(MockStore::new()))
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
    let entity_cache = EntityCache::with_mocks(redis_cache.clone(), map, valid_schema.clone())
        .await
        .unwrap();

    let service = TestHarness::builder()
        .configuration_json(serde_json::json!({
            "include_subgraph_errors": { "all": true },
            "experimental_mock_subgraphs": subgraphs,
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
            .any(|cache_key| cache_key.key.starts_with(&prefix_key))
    );

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

    let redis_cache = RedisCacheStorage::from_mocks(Arc::new(MockStore::new()))
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
            .any(|cache_key| cache_key.key.starts_with(&prefix_key))
    );
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

    let redis_cache = RedisCacheStorage::from_mocks(Arc::new(MockStore::new()))
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
    let entity_cache = EntityCache::with_mocks(redis_cache.clone(), map, valid_schema.clone())
        .await
        .unwrap();

    let service = TestHarness::builder()
        .configuration_json(serde_json::json!({"include_subgraph_errors": { "all": true }, "experimental_mock_subgraphs": subgraphs.clone() }))
        .unwrap()
        .schema(SCHEMA_NESTED_KEYS)
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
            .any(|cache_key| cache_key.key.starts_with(&prefix_key))
    );

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
        .configuration_json(serde_json::json!({"include_subgraph_errors": { "all": true }, "experimental_mock_subgraphs": subgraphs.clone() }))
        .unwrap()
        .schema(SCHEMA_NESTED_KEYS)
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

    let redis_cache = RedisCacheStorage::from_mocks(Arc::new(MockStore::new()))
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

    let redis_cache = RedisCacheStorage::from_mocks(Arc::new(MockStore::new()))
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

    let redis_cache = RedisCacheStorage::from_mocks(Arc::new(MockStore::new()))
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

    let redis_cache = RedisCacheStorage::from_mocks(Arc::new(MockStore::new()))
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

    let redis_cache = RedisCacheStorage::from_mocks(Arc::new(MockStore::new()))
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
