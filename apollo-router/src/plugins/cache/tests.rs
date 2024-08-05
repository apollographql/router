use std::collections::HashMap;
use std::sync::Arc;

use bytes::Bytes;
use fred::error::RedisErrorKind;
use fred::mocks::MockCommand;
use fred::mocks::Mocks;
use fred::prelude::RedisError;
use fred::prelude::RedisValue;
use http::header::CACHE_CONTROL;
use http::HeaderValue;
use parking_lot::Mutex;
use tower::ServiceExt;

use super::entity::EntityCache;
use crate::cache::redis::RedisCacheStorage;
use crate::plugin::test::MockSubgraph;
use crate::plugins::cache::entity::Subgraph;
use crate::services::supergraph;
use crate::Context;
use crate::MockedSubgraphs;
use crate::TestHarness;

const SCHEMA: &str = r#"schema
        @core(feature: "https://specs.apollo.dev/core/v0.1")
        @core(feature: "https://specs.apollo.dev/join/v0.1")
        @core(feature: "https://specs.apollo.dev/inaccessible/v0.1")
         {
        query: Query
        subscription: Subscription
   }
   directive @core(feature: String!) repeatable on SCHEMA
   directive @join__field(graph: join__Graph, requires: join__FieldSet, provides: join__FieldSet) on FIELD_DEFINITION
   directive @join__type(graph: join__Graph!, key: join__FieldSet) repeatable on OBJECT | INTERFACE
   directive @join__owner(graph: join__Graph!) on OBJECT | INTERFACE
   directive @join__graph(name: String!, url: String!) on ENUM_VALUE
   directive @inaccessible on OBJECT | FIELD_DEFINITION | INTERFACE | UNION
   scalar join__FieldSet
   enum join__Graph {
       USER @join__graph(name: "user", url: "http://localhost:4001/graphql")
       ORGA @join__graph(name: "orga", url: "http://localhost:4002/graphql")
   }
   type Query {
       currentUser: User @join__field(graph: USER)
   }

   type Subscription @join__type(graph: USER) {
        userWasCreated: User
   }

   type User
   @join__owner(graph: USER)
   @join__type(graph: ORGA, key: "id")
   @join__type(graph: USER, key: "id"){
       id: ID!
       name: String
       activeOrganization: Organization
       allOrganizations: [Organization]
   }
   type Organization
   @join__owner(graph: ORGA)
   @join__type(graph: ORGA, key: "id")
   @join__type(graph: USER, key: "id") {
       id: ID
       creatorUser: User
       name: String
       nonNullId: ID!
       suborga: [Organization]
   }"#;

#[derive(Debug)]
pub(crate) struct MockStore {
    map: Arc<Mutex<HashMap<Bytes, Bytes>>>,
}

impl MockStore {
    fn new() -> MockStore {
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
                if let Some(RedisValue::Bytes(b)) = command.args.first() {
                    if let Some(bytes) = self.map.lock().get(b) {
                        println!("-> returning {:?}", std::str::from_utf8(bytes));
                        return Ok(RedisValue::Bytes(bytes.clone()));
                    }
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
    let entity_cache = EntityCache::with_mocks(redis_cache.clone(), map)
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
    let entity_cache = EntityCache::with_mocks(redis_cache.clone(), HashMap::new())
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
async fn no_cache_control() {
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
        ).build())
    ].into_iter().collect());

    let redis_cache = RedisCacheStorage::from_mocks(Arc::new(MockStore::new()))
        .await
        .unwrap();
    let entity_cache = EntityCache::with_mocks(redis_cache.clone(), HashMap::new())
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
    let entity_cache = EntityCache::with_mocks(redis_cache.clone(), HashMap::new())
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
    let entity_cache = EntityCache::with_mocks(redis_cache.clone(), map)
        .await
        .unwrap();

    let service = TestHarness::builder()
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
    let response = service
        .oneshot(request)
        .await
        .unwrap()
        .next_response()
        .await
        .unwrap();

    insta::assert_json_snapshot!(response);

    println!("\nNOW WITHOUT SUBGRAPHS\n");
    // Now testing without any mock subgraphs, all the data should come from the cache
    let service = TestHarness::builder()
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
    let response = service
        .clone()
        .oneshot(request)
        .await
        .unwrap()
        .next_response()
        .await
        .unwrap();

    insta::assert_json_snapshot!(response);

    println!("\nNOW WITH DIFFERENT SUB\n");

    let context = Context::new();
    context.insert_json_value("sub", "5678".into());
    let request = supergraph::Request::fake_builder()
        .query(query)
        .context(context)
        .build()
        .unwrap();
    let response = service
        .clone()
        .oneshot(request)
        .await
        .unwrap()
        .next_response()
        .await
        .unwrap();

    insta::assert_json_snapshot!(response);
}

#[tokio::test]
async fn no_data() {
    let query = "query { currentUser { allOrganizations { id name } } }";

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
    let entity_cache = EntityCache::with_mocks(redis_cache.clone(), map)
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

    // Now testing without any mock subgraphs, all the data should come from the cache
    let entity_cache = EntityCache::with_mocks(redis_cache.clone(), HashMap::new())
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
                            "id": "2",
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
