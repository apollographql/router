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
use crate::json_ext::Object;
use crate::plugin::test::MockSubgraph;
use crate::plugins::authentication::APOLLO_AUTHENTICATION_JWT_CLAIMS;
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
pub(crate) struct Mock1 {
    set: Mutex<bool>,
}

impl Mock1 {
    fn new() -> Mock1 {
        Mock1 {
            set: Mutex::new(false),
        }
    }
}

static USER_RESPONSE:&str = "{\"control\":{\"created\":1705069368},\"data\":{\"currentUser\":{\"activeOrganization\":{\"__typename\":\"Organization\",\"id\":\"1\"}}}}";
static ORGA_RESPONSE:&str = "{\"control\":{\"created\":1705072093},\"data\":{\"creatorUser\":{\"__typename\":\"User\",\"id\":2}}}";
impl Mocks for Mock1 {
    fn process_command(&self, command: MockCommand) -> Result<RedisValue, RedisError> {
        println!("received redis command: {command:?}");

        match &*command.cmd {
            "GET" => {
                if let Some(RedisValue::Bytes(b)) = command.args.first() {
                    if b == &b"subgraph:user:Query:146a735f805c55554b5233253c17756deaa6ffd06696fafa4d6e3186e6efe592:d9d84a3c7ffc27b0190a671212f3740e5b8478e84e23825830e97822e25cf05c"[..]{
                        let set = self.set.lock();
                        if *set {
                            return Ok(RedisValue::Bytes(Bytes::from(USER_RESPONSE)));
                        }
                    } else if b == &b"subgraph:orga:Organization:5811967f540d300d249ab30ae681359a7815fdb5d3dc71a94be1d491006a6b27:655f22a6af21d7ffe671d3ce4b33464a76ddfea0bf179740b15e804b11983c04:d9d84a3c7ffc27b0190a671212f3740e5b8478e84e23825830e97822e25cf05c"[..] {
                        return Ok(RedisValue::Bytes(Bytes::from(ORGA_RESPONSE)));
                    }
                }
            }
            "SET" => {
                if let Some(RedisValue::Bytes(b)) = command.args.first() {
                    if b ==
                        &b"subgraph:user:Query:146a735f805c55554b5233253c17756deaa6ffd06696fafa4d6e3186e6efe592:d9d84a3c7ffc27b0190a671212f3740e5b8478e84e23825830e97822e25cf05c"[..] {
                            let mut set = self.set.lock();
                            *set = true;

                            //FIXME: can't assert because the creatin date changes
                            //assert_eq!(USER_RESPONSE, command.args.get(1).unwrap().as_str().unwrap(), );
                            return Ok(RedisValue::Null)
                    }
                }
            }
            _ => {}
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

    let redis_cache = RedisCacheStorage::from_mocks(Arc::new(Mock1::new()))
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
    let response = service
        .oneshot(request)
        .await
        .unwrap()
        .next_response()
        .await
        .unwrap();

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
    let response = service
        .oneshot(request)
        .await
        .unwrap()
        .next_response()
        .await
        .unwrap();

    insta::assert_json_snapshot!(response);
}

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
        println!("mock2 received redis command: {command:?}");

        match &*command.cmd {
            "GET" => {
                if let Some(RedisValue::Bytes(b)) = command.args.first() {
                    if let Some(bytes) = self.map.lock().get(b) {
                        return Ok(RedisValue::Bytes(bytes.clone()));
                    }
                }
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
                loop {
                    if let (Some(RedisValue::Bytes(key)), Some(RedisValue::Bytes(value))) =
                        (args_it.next(), args_it.next())
                    {
                        self.map.lock().insert(key.clone(), value.clone());
                    } else {
                        break;
                    }
                }
                return Ok(RedisValue::Null);
            }

            _ => {}
        }
        Err(RedisError::new(RedisErrorKind::NotFound, "mock not found"))
    }
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
    let entity_cache = EntityCache::with_mocks(redis_cache.clone(), HashMap::new())
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

    let mut claims = Object::new();
    claims.insert("sub", "1234".into());
    let context = Context::new();
    context.insert_json_value(APOLLO_AUTHENTICATION_JWT_CLAIMS, claims.into());

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

    let mut claims = Object::new();
    claims.insert("sub", "1234".into());
    let context = Context::new();
    context.insert_json_value(APOLLO_AUTHENTICATION_JWT_CLAIMS, claims.into());
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

    let mut claims = Object::new();
    claims.insert("sub", "5678".into());
    let context = Context::new();
    context.insert_json_value(APOLLO_AUTHENTICATION_JWT_CLAIMS, claims.into());
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
