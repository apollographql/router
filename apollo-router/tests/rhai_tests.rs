use std::str::FromStr;
use std::sync::Arc;

use apollo_router::graphql::Request;
use apollo_router::http_ext;
use apollo_router::plugin::plugins;
use apollo_router::plugin::DynPlugin;
use apollo_router::services::PluggableRouterServiceBuilder;
use apollo_router::services::SubgraphService;
use apollo_router::Schema;
use serde_json::Value;
use tower::ServiceExt;

// This test will fail if run with the "multi_thread" flavor.
// This is because tracing_test doesn't set a global subscriber, so logs will be dropped
// if we're crossing a thread boundary
#[tokio::test]
async fn all_rhai_callbacks_are_invoked() {
    let env_filter = "apollo_router=info";
    let mock_writer = tracing_test::internal::MockWriter::new(&tracing_test::internal::GLOBAL_BUF);
    let subscriber = tracing_test::internal::get_subscriber(mock_writer, env_filter);

    let _guard = tracing::dispatcher::set_default(&subscriber);

    let dyn_plugin: Box<dyn DynPlugin> = plugins()
        .get("apollo.rhai")
        .expect("Plugin not found")
        .create_instance(
            &Value::from_str(r#"{"scripts":"tests/fixtures", "main": "test_callbacks.rhai"}"#)
                .unwrap(),
        )
        .await
        .unwrap();

    let schema: Arc<Schema> = Arc::new(
        include_str!("./fixtures/supergraph.graphql")
            .parse()
            .unwrap(),
    );

    let mut builder = PluggableRouterServiceBuilder::new(schema.clone())
        .with_dyn_plugin("apollo.rhai".to_string(), dyn_plugin);

    let subgraphs = schema.subgraphs();
    for (name, _url) in subgraphs {
        let service = SubgraphService::new(name.to_owned());
        builder = builder.with_subgraph_service(name, service);
    }
    let router = builder.build().await.unwrap().test_service();

    let request = http_ext::Request::fake_builder()
        .body(
            Request::builder()
                .query(r#"{ topProducts { name } }"#.to_string())
                .build(),
        )
        .build()
        .unwrap();

    let _ = router
        .oneshot(request.into())
        .await
        .unwrap()
        .next_response()
        .await
        .unwrap();

    for expected_log in [
        "router_service setup",
        "from_router_request",
        "from_router_response",
        "query_planner_service setup",
        "from_query_planner_response",
        "from_query_planner_request",
        "execution_service setup",
        "from_execution_request",
        "from_execution_response",
        "subgraph_service setup",
        "from_subgraph_request",
    ] {
        assert!(tracing_test::internal::logs_with_scope_contain(
            "apollo_router",
            expected_log
        ));
    }
}
