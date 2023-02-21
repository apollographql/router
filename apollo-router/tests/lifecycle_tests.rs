use std::convert::Infallible;
use std::net::SocketAddr;
use std::time::Duration;

use apollo_router::graphql;
use apollo_router::Configuration;
use apollo_router::ConfigurationSource;
use apollo_router::Executable;
use apollo_router::SchemaSource;
use apollo_router::ShutdownSource;
use http::header::CONTENT_TYPE;
use http::HeaderValue;
use http::Request;
use http::Response;
use hyper::service::make_service_fn;
use hyper::service::service_fn;
use hyper::Body;
use hyper::Server;
use mime::APPLICATION_JSON;
use serde_json::json;
use tokio::sync::oneshot;
use tower::BoxError;

use crate::common::IntegrationTest;

mod common;

const HAPPY_CONFIG: &str = include_str!("fixtures/jaeger.router.yaml");
const BROKEN_PLUGIN_CONFIG: &str = include_str!("fixtures/broken_plugin.router.yaml");
const INVALID_CONFIG: &str = "garbage: garbage";

#[tokio::test(flavor = "multi_thread")]
async fn test_happy() -> Result<(), BoxError> {
    let mut router = create_router(HAPPY_CONFIG).await?;
    router.start().await;
    router.assert_started().await;
    router.run_query().await;
    router.graceful_shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_invalid_config() -> Result<(), BoxError> {
    let mut router = create_router(INVALID_CONFIG).await?;
    router.start().await;
    router.assert_not_started().await;
    router.assert_shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_reload_config_valid() -> Result<(), BoxError> {
    let mut router = create_router(HAPPY_CONFIG).await?;
    router.start().await;
    router.assert_started().await;
    router.run_query().await;
    router.touch_config().await;
    router.assert_reloaded().await;
    router.run_query().await;
    router.graceful_shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_reload_config_with_broken_plugin() -> Result<(), BoxError> {
    let mut router = create_router(HAPPY_CONFIG).await?;
    router.start().await;
    router.assert_started().await;
    router.run_query().await;
    router.update_config(BROKEN_PLUGIN_CONFIG).await;
    router.assert_not_reloaded().await;
    router.run_query().await;
    router.graceful_shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_reload_config_with_broken_plugin_recovery() -> Result<(), BoxError> {
    let mut router = create_router(HAPPY_CONFIG).await?;
    for i in 0..3 {
        println!("iteration {i}");
        router.start().await;
        router.assert_started().await;
        router.run_query().await;
        router.update_config(BROKEN_PLUGIN_CONFIG).await;
        router.assert_not_reloaded().await;
        router.run_query().await;
        router.update_config(HAPPY_CONFIG).await;
        router.assert_reloaded().await;
        router.run_query().await;
        router.graceful_shutdown().await;
    }
    Ok(())
}

async fn create_router(config: &str) -> Result<IntegrationTest, BoxError> {
    let tracer = opentelemetry_jaeger::new_agent_pipeline()
        .with_service_name("my_app")
        .install_simple()?;

    Ok(IntegrationTest::new(tracer, opentelemetry_jaeger::Propagator::new(), config).await)
}

#[tokio::test(flavor = "multi_thread")]
async fn test_graceful_shutdown() -> Result<(), BoxError> {
    async fn handle(_req: Request<Body>) -> Result<Response<String>, Infallible> {
        tokio::time::sleep(Duration::from_secs(5)).await;
        let mut response = Response::new(
            json!({
                "data":{
                    "currentUser": {
                        "id": 1
                    }
                }
            })
            .to_string(),
        );
        response
            .headers_mut()
            .insert("Content-Type", HeaderValue::from_static("application/json"));

        Ok(response)
    }

    let addr = SocketAddr::from(([127, 0, 0, 1], 0));

    let make_service = make_service_fn(|_conn| async { Ok::<_, Infallible>(service_fn(handle)) });

    let server = Server::bind(&addr).serve(make_service);
    let addr = server.local_addr();

    tokio::task::spawn(async move {
        if let Err(e) = server.await {
            eprintln!("server error: {e}");
        }
    });

    println!("started subgraph at {addr}");

    let schema_sdl = r#"schema
    @core(feature: "https://specs.apollo.dev/core/v0.1")
    @core(feature: "https://specs.apollo.dev/join/v0.1")
    @core(feature: "https://specs.apollo.dev/inaccessible/v0.1")
     {
    query: Query
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
}"#.to_string();

    let config: Configuration = serde_yaml::from_str(&format!(
        r#"
supergraph:
    listen: 127.0.0.1:4321
override_subgraph_url:
    user: http://{addr}
include_subgraph_errors:
    all: true
"#,
    ))
    .unwrap();

    let (shutdown_sender, shutdown_receiver) = oneshot::channel();
    let f = async move {
        let _: Result<(), _> = shutdown_receiver.await;
    };

    let _router_handle = tokio::task::spawn(async move {
        Executable::builder()
            .shutdown(ShutdownSource::Custom(Box::pin(f)))
            .schema(SchemaSource::Static { schema_sdl })
            .config(ConfigurationSource::Static(Box::new(config)))
            .start()
            .await
    });

    tokio::time::sleep(Duration::from_millis(1000)).await;
    println!("started router");

    let client = reqwest::Client::new();

    let request = client
        .post("http://127.0.0.1:4321")
        .header(CONTENT_TYPE, APPLICATION_JSON.essence_str())
        .header("apollographql-client-name", "custom_name")
        .header("apollographql-client-version", "1.0")
        .json(&json!({"query":"{ currentUser { id } }","variables":{}}))
        .build()
        .unwrap();

    let client_handle = tokio::task::spawn(async move {
        let res = client.execute(request).await.unwrap();
        serde_json::from_slice::<graphql::Response>(&res.bytes().await.unwrap()).unwrap()
    });

    // tell the router to shutdown while a client request is in flight
    shutdown_sender.send(()).unwrap();

    let data = client_handle.await.unwrap();
    insta::assert_json_snapshot!(data);

    println!("executed client request");

    Ok(())
}
