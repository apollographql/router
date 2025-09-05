//
// Please ensure that any tests added to the tests module use the tokio multi-threaded test executor.
//
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;

use axum_extra::headers::HeaderName;
use http::HeaderMap;
use http::HeaderValue;
use http::StatusCode;
use http::header::CONTENT_TYPE;
use insta::assert_snapshot;
use itertools::Itertools;
use opentelemetry::propagation::Injector;
use opentelemetry::propagation::TextMapPropagator;
use opentelemetry::trace::SpanContext;
use opentelemetry::trace::SpanId;
use opentelemetry::trace::TraceContextExt;
use opentelemetry::trace::TraceFlags;
use opentelemetry::trace::TraceId;
use opentelemetry::trace::TraceState;
use serde_json::Value;
use serde_json_bytes::ByteString;
use serde_json_bytes::json;
use tower::Service;
use tower::ServiceExt;
use tower::util::BoxService;

use super::CustomTraceIdPropagator;
use super::EnabledFeatures;
use super::Telemetry;
use super::apollo::ForwardHeaders;
use crate::error::FetchError;
use crate::graphql;
use crate::graphql::Error;
use crate::graphql::IntoGraphQLErrors;
use crate::graphql::Request;
use crate::http_ext;
use crate::json_ext::Object;
use crate::metrics::FutureMetricsExt;
use crate::plugin::DynPlugin;
use crate::plugin::PluginInit;
use crate::plugin::test::MockRouterService;
use crate::plugin::test::MockSubgraphService;
use crate::plugin::test::MockSupergraphService;
use crate::plugins::demand_control::COST_ACTUAL_KEY;
use crate::plugins::demand_control::COST_ESTIMATED_KEY;
use crate::plugins::demand_control::COST_RESULT_KEY;
use crate::plugins::demand_control::COST_STRATEGY_KEY;
use crate::plugins::demand_control::DemandControlError;
use crate::plugins::telemetry::EnableSubgraphFtv1;
use crate::plugins::telemetry::config::TraceIdFormat;
use crate::services::RouterRequest;
use crate::services::RouterResponse;
use crate::services::SubgraphRequest;
use crate::services::SubgraphResponse;
use crate::services::SupergraphRequest;
use crate::services::SupergraphResponse;
use crate::services::router;

macro_rules! assert_prometheus_metrics {
    ($plugin:expr) => {{
        let prometheus_metrics = get_prometheus_metrics($plugin.as_ref()).await;
        let regexp = regex::Regex::new(
            r#"process_executable_name="(?P<process>[^"]+)",?|service_name="(?P<service>[^"]+)",?"#,
        )
        .unwrap();
        let prometheus_metrics = regexp.replace_all(&prometheus_metrics, "").to_owned();
        assert_snapshot!(prometheus_metrics.replace(
            &format!(r#"service_version="{}""#, std::env!("CARGO_PKG_VERSION")),
            r#"service_version="X""#
        ));
    }};
}

async fn create_plugin_with_config(full_config: &str) -> Box<dyn DynPlugin> {
    let full_config = serde_yaml::from_str::<Value>(full_config).expect("yaml must be valid");
    let telemetry_config = full_config
        .as_object()
        .expect("must be an object")
        .get("telemetry")
        .expect("telemetry must be a root key");
    let init = PluginInit::fake_builder()
        .config(telemetry_config.clone())
        .full_config(full_config)
        .build()
        .with_deserialized_config()
        .expect("unable to deserialize telemetry config");

    let plugin = crate::plugin::plugins()
        .find(|factory| factory.name == "apollo.telemetry")
        .expect("Plugin not found")
        .create_instance(init)
        .await
        .expect("unable to create telemetry plugin");

    let downcast = plugin
        .as_any()
        .downcast_ref::<Telemetry>()
        .expect("Telemetry plugin expected");
    if downcast.config.exporters.metrics.prometheus.enabled {
        downcast.activation.lock().reload_metrics();
    }
    plugin
}

async fn get_prometheus_metrics(plugin: &dyn DynPlugin) -> String {
    let web_endpoint = plugin
        .web_endpoints()
        .into_iter()
        .next()
        .unwrap()
        .1
        .into_iter()
        .next()
        .unwrap()
        .into_router();

    let http_req_prom = http::Request::get("http://localhost:9090/metrics")
        .body(axum::body::Body::empty())
        .unwrap();
    let mut resp = web_endpoint.oneshot(http_req_prom).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = router::body::into_bytes(resp.body_mut()).await.unwrap();
    String::from_utf8_lossy(&body)
        .split('\n')
        .filter(|l| l.contains("bucket"))
        .sorted()
        .join("\n")
}

async fn make_supergraph_request(plugin: &dyn DynPlugin) {
    let mut mock_service = MockSupergraphService::new();
    mock_service
        .expect_call()
        .times(1)
        .returning(move |req: SupergraphRequest| {
            Ok(SupergraphResponse::fake_builder()
                .context(req.context)
                .header("x-custom", "coming_from_header")
                .data(json!({"data": {"my_value": 2usize}}))
                .build()
                .unwrap())
        });

    let mut supergraph_service = plugin.supergraph_service(BoxService::new(mock_service));
    let router_req = SupergraphRequest::fake_builder().header("test", "my_value_set");
    let _router_response = supergraph_service
        .ready()
        .await
        .unwrap()
        .call(router_req.build().unwrap())
        .await
        .unwrap()
        .next_response()
        .await
        .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn plugin_registered() {
    let full_config = serde_json::json!({
        "telemetry": {
            "apollo": {
                "schema_id": "abc"
            },
            "exporters": {
                "tracing": {},
            },
        },
    });
    let telemetry_config = full_config["telemetry"].clone();
    crate::plugin::plugins()
        .find(|factory| factory.name == "apollo.telemetry")
        .expect("Plugin not found")
        .create_instance(
            PluginInit::fake_builder()
                .config(telemetry_config)
                .full_config(full_config)
                .build(),
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn config_serialization() {
    create_plugin_with_config(include_str!("testdata/config.router.yaml")).await;
}

#[tokio::test]
async fn test_enabled_features() {
    // Explicitly enabled
    let plugin = create_plugin_with_config(include_str!(
        "testdata/full_config_all_features_enabled.router.yaml"
    ))
    .await;
    let features = enabled_features(plugin.as_ref());
    assert!(
        features.distributed_apq_cache,
        "Telemetry plugin should consider apq feature enabled when explicitly enabled"
    );
    assert!(
        features.entity_cache,
        "Telemetry plugin should consider entity cache feature enabled when explicitly enabled"
    );

    // Explicitly disabled
    let plugin = create_plugin_with_config(include_str!(
        "testdata/full_config_all_features_explicitly_disabled.router.yaml"
    ))
    .await;
    let features = enabled_features(plugin.as_ref());
    assert!(
        !features.distributed_apq_cache,
        "Telemetry plugin should consider apq feature disabled when explicitly disabled"
    );
    assert!(
        !features.entity_cache,
        "Telemetry plugin should consider entity cache feature disabled when explicitly disabled"
    );

    // Default Values
    let plugin = create_plugin_with_config(include_str!(
        "testdata/full_config_all_features_defaults.router.yaml"
    ))
    .await;
    let features = enabled_features(plugin.as_ref());
    assert!(
        !features.distributed_apq_cache,
        "Telemetry plugin should consider apq feature disabled when all values are defaulted"
    );
    assert!(
        !features.entity_cache,
        "Telemetry plugin should consider entity cache feature disabled when all values are defaulted"
    );

    // APQ enabled when default enabled with redis config defined
    let plugin = create_plugin_with_config(include_str!(
        "testdata/full_config_apq_enabled_partial_defaults.router.yaml"
    ))
    .await;
    let features = enabled_features(plugin.as_ref());
    assert!(
        features.distributed_apq_cache,
        "Telemetry plugin should consider apq feature enabled when top-level enabled flag is defaulted and redis config is defined"
    );

    // APQ disabled when default enabled with redis config NOT defined
    let plugin = create_plugin_with_config(include_str!(
        "testdata/full_config_apq_disabled_partial_defaults.router.yaml"
    ))
    .await;
    let features = enabled_features(plugin.as_ref());
    assert!(
        !features.distributed_apq_cache,
        "Telemetry plugin should consider apq feature disabled when redis cache is not enabled"
    );
}

fn enabled_features(plugin: &dyn DynPlugin) -> &EnabledFeatures {
    &plugin
        .as_any()
        .downcast_ref::<Telemetry>()
        .expect("telemetry plugin")
        .enabled_features
}

#[tokio::test]
async fn test_supergraph_metrics_ok() {
    async {
        let plugin =
            create_plugin_with_config(include_str!("testdata/custom_attributes.router.yaml")).await;
        make_supergraph_request(plugin.as_ref()).await;

        assert_counter!(
            "http.request",
            1,
            "another_test" = "my_default_value",
            "my_value" = 2,
            "myname" = "label_value",
            "renamed_value" = "my_value_set",
            "x-custom" = "coming_from_header"
        );
    }
    .with_metrics()
    .await;
}

#[tokio::test]
async fn test_supergraph_metrics_bad_request() {
    async {
        let plugin =
            create_plugin_with_config(include_str!("testdata/custom_attributes.router.yaml")).await;

        let mut mock_bad_request_service = MockSupergraphService::new();
        mock_bad_request_service
            .expect_call()
            .times(1)
            .returning(move |req: SupergraphRequest| {
                Ok(SupergraphResponse::fake_builder()
                    .context(req.context)
                    .status_code(StatusCode::BAD_REQUEST)
                    .errors(vec![
                        crate::graphql::Error::builder()
                            .message("nope")
                            .extension_code("NOPE")
                            .build(),
                    ])
                    .build()
                    .unwrap())
            });
        let mut bad_request_supergraph_service =
            plugin.supergraph_service(BoxService::new(mock_bad_request_service));
        let router_req = SupergraphRequest::fake_builder().header("test", "my_value_set");
        let _router_response = bad_request_supergraph_service
            .ready()
            .await
            .unwrap()
            .call(router_req.build().unwrap())
            .await
            .unwrap()
            .next_response()
            .await
            .unwrap();

        assert_counter!(
            "http.request",
            1,
            "another_test" = "my_default_value",
            "error" = "nope",
            "myname" = "label_value",
            "renamed_value" = "my_value_set"
        );
    }
    .with_metrics()
    .await;
}

#[tokio::test]
async fn test_custom_router_instruments() {
    async {
        let plugin =
            create_plugin_with_config(include_str!("testdata/custom_instruments.router.yaml"))
                .await;

        let mut mock_bad_request_service = MockRouterService::new();
        mock_bad_request_service
            .expect_call()
            .times(2)
            .returning(move |req: RouterRequest| {
                Ok(RouterResponse::fake_builder()
                    .context(req.context)
                    .status_code(StatusCode::BAD_REQUEST)
                    .header("content-type", "application/json")
                    .data(json!({"errors": [{"message": "nope"}]}))
                    .build()
                    .unwrap())
            });
        let mut bad_request_router_service =
            plugin.router_service(BoxService::new(mock_bad_request_service));
        let router_req = RouterRequest::fake_builder()
            .header("x-custom", "TEST")
            .header("conditional-custom", "X")
            .header("custom-length", "55")
            .header("content-length", "55")
            .header("content-type", "application/graphql");
        let _router_response = bad_request_router_service
            .ready()
            .await
            .unwrap()
            .call(router_req.build().unwrap())
            .await
            .unwrap()
            .next_response()
            .await
            .unwrap();

        assert_counter!("acme.graphql.custom_req", 1.0);
        assert_histogram_sum!(
            "http.server.request.body.size",
            55.0,
            "http.response.status_code" = 400,
            "acme.my_attribute" = "application/json"
        );
        assert_histogram_sum!("acme.request.length", 55.0);

        let router_req = RouterRequest::fake_builder()
            .header("x-custom", "TEST")
            .header("custom-length", "5")
            .header("content-length", "5")
            .header("content-type", "application/graphql");
        let _router_response = bad_request_router_service
            .ready()
            .await
            .unwrap()
            .call(router_req.build().unwrap())
            .await
            .unwrap()
            .next_response()
            .await
            .unwrap();
        assert_counter!("acme.graphql.custom_req", 1.0);
        assert_histogram_sum!("acme.request.length", 60.0);
        assert_histogram_sum!(
            "http.server.request.body.size",
            60.0,
            "http.response.status_code" = 400,
            "acme.my_attribute" = "application/json"
        );
    }
    .with_metrics()
    .await;
}

#[tokio::test]
async fn test_custom_router_instruments_with_requirement_level() {
    async {
        let plugin = create_plugin_with_config(include_str!(
            "testdata/custom_instruments_level.router.yaml"
        ))
        .await;

        let mut mock_bad_request_service = MockRouterService::new();
        mock_bad_request_service
            .expect_call()
            .times(2)
            .returning(move |req: RouterRequest| {
                Ok(RouterResponse::fake_builder()
                    .context(req.context)
                    .status_code(StatusCode::BAD_REQUEST)
                    .header("content-type", "application/json")
                    .data(json!({"errors": [{"message": "nope"}]}))
                    .build()
                    .unwrap())
            });
        let mut bad_request_router_service =
            plugin.router_service(BoxService::new(mock_bad_request_service));
        let router_req = RouterRequest::fake_builder()
            .header("x-custom", "TEST")
            .header("conditional-custom", "X")
            .header("custom-length", "55")
            .header("content-length", "55")
            .header("content-type", "application/graphql");
        let _router_response = bad_request_router_service
            .ready()
            .await
            .unwrap()
            .call(router_req.build().unwrap())
            .await
            .unwrap()
            .next_response()
            .await
            .unwrap();

        assert_counter!("acme.graphql.custom_req", 1.0);
        assert_histogram_sum!(
            "http.server.request.body.size",
            55.0,
            "acme.my_attribute" = "application/json",
            "error.type" = "Bad Request",
            "http.response.status_code" = 400,
            "network.protocol.version" = "HTTP/1.1"
        );
        assert_histogram_exists!(
            "http.server.request.duration",
            f64,
            "error.type" = "Bad Request",
            "http.response.status_code" = 400,
            "network.protocol.version" = "HTTP/1.1",
            "http.request.method" = "GET"
        );
        assert_histogram_sum!("acme.request.length", 55.0);

        let router_req = RouterRequest::fake_builder()
            .header("x-custom", "TEST")
            .header("custom-length", "5")
            .header("content-length", "5")
            .header("content-type", "application/graphql");
        let _router_response = bad_request_router_service
            .ready()
            .await
            .unwrap()
            .call(router_req.build().unwrap())
            .await
            .unwrap()
            .next_response()
            .await
            .unwrap();
        assert_counter!("acme.graphql.custom_req", 1.0);
        assert_histogram_sum!("acme.request.length", 60.0);
        assert_histogram_sum!(
            "http.server.request.body.size",
            60.0,
            "http.response.status_code" = 400,
            "acme.my_attribute" = "application/json",
            "error.type" = "Bad Request",
            "http.response.status_code" = 400,
            "network.protocol.version" = "HTTP/1.1"
        );
    }
    .with_metrics()
    .await;
}

#[tokio::test]
async fn test_custom_supergraph_instruments() {
    async {
        let plugin =
            create_plugin_with_config(include_str!("testdata/custom_instruments.router.yaml"))
                .await;

        let mut mock_bad_request_service = MockSupergraphService::new();
        mock_bad_request_service
            .expect_call()
            .times(3)
            .returning(move |req: SupergraphRequest| {
                Ok(SupergraphResponse::fake_builder()
                    .context(req.context)
                    .status_code(StatusCode::BAD_REQUEST)
                    .header("content-type", "application/json")
                    .data(json!({"errors": [{"message": "nope"}]}))
                    .build()
                    .unwrap())
            });
        let mut bad_request_supergraph_service =
            plugin.supergraph_service(BoxService::new(mock_bad_request_service));
        let supergraph_req = SupergraphRequest::fake_builder()
            .header("x-custom", "TEST")
            .header("conditional-custom", "X")
            .header("custom-length", "55")
            .header("content-length", "55")
            .header("content-type", "application/graphql")
            .query("Query test { me {name} }")
            .operation_name("test".to_string());
        let _router_response = bad_request_supergraph_service
            .ready()
            .await
            .unwrap()
            .call(supergraph_req.build().unwrap())
            .await
            .unwrap()
            .next_response()
            .await
            .unwrap();

        assert_counter!(
            "acme.graphql.requests",
            1.0,
            "acme.my_attribute" = "application/json",
            "graphql_query" = "Query test { me {name} }",
            "graphql.document" = "Query test { me {name} }"
        );

        let supergraph_req = SupergraphRequest::fake_builder()
            .header("x-custom", "TEST")
            .header("custom-length", "5")
            .header("content-length", "5")
            .header("content-type", "application/graphql")
            .query("Query test { me {name} }")
            .operation_name("test".to_string());

        let _router_response = bad_request_supergraph_service
            .ready()
            .await
            .unwrap()
            .call(supergraph_req.build().unwrap())
            .await
            .unwrap()
            .next_response()
            .await
            .unwrap();
        assert_counter!(
            "acme.graphql.requests",
            2.0,
            "acme.my_attribute" = "application/json",
            "graphql_query" = "Query test { me {name} }",
            "graphql.document" = "Query test { me {name} }"
        );

        let supergraph_req = SupergraphRequest::fake_builder()
            .header("custom-length", "5")
            .header("content-length", "5")
            .header("content-type", "application/graphql")
            .query("Query test { me {name} }")
            .operation_name("test".to_string());

        let _router_response = bad_request_supergraph_service
            .ready()
            .await
            .unwrap()
            .call(supergraph_req.build().unwrap())
            .await
            .unwrap()
            .next_response()
            .await
            .unwrap();
        assert_counter!(
            "acme.graphql.requests",
            2.0,
            "acme.my_attribute" = "application/json",
            "graphql_query" = "Query test { me {name} }",
            "graphql.document" = "Query test { me {name} }"
        );
    }
    .with_metrics()
    .await;
}

#[tokio::test]
async fn test_custom_subgraph_instruments_level() {
    async {
        let plugin = create_plugin_with_config(include_str!(
            "testdata/custom_instruments_level.router.yaml"
        ))
        .await;

        let mut mock_bad_request_service = MockSubgraphService::new();
        mock_bad_request_service
            .expect_call()
            .times(2)
            .returning(move |req: SubgraphRequest| {
                let mut headers = HeaderMap::new();
                headers.insert(CONTENT_TYPE, "application/json".parse().unwrap());
                let errors = vec![
                    graphql::Error::builder()
                        .message("nope".to_string())
                        .extension_code("NOPE")
                        .build(),
                    graphql::Error::builder()
                        .message("nok".to_string())
                        .extension_code("NOK")
                        .build(),
                ];
                Ok(SubgraphResponse::fake_builder()
                    .context(req.context)
                    .status_code(StatusCode::BAD_REQUEST)
                    .headers(headers)
                    .errors(errors)
                    .build())
            });
        let mut bad_request_subgraph_service =
            plugin.subgraph_service("test", BoxService::new(mock_bad_request_service));
        let sub_req = http::Request::builder()
            .method("POST")
            .uri("http://test")
            .header("x-custom", "TEST")
            .header("conditional-custom", "X")
            .header("custom-length", "55")
            .header("content-length", "55")
            .header("content-type", "application/graphql")
            .body(graphql::Request::builder().query("{ me {name} }").build())
            .unwrap();
        let subgraph_req = SubgraphRequest::fake_builder()
            .subgraph_request(sub_req)
            .subgraph_name("test".to_string())
            .build();

        let _router_response = bad_request_subgraph_service
            .ready()
            .await
            .unwrap()
            .call(subgraph_req)
            .await
            .unwrap();

        assert_counter!(
            "acme.subgraph.error_reqs",
            1.0,
            graphql_error = opentelemetry::Value::Array(opentelemetry::Array::String(vec![
                "nope".into(),
                "nok".into()
            ])),
            subgraph.name = "test"
        );
        let sub_req = http::Request::builder()
            .method("POST")
            .uri("http://test")
            .header("x-custom", "TEST")
            .header("conditional-custom", "X")
            .header("custom-length", "55")
            .header("content-length", "55")
            .header("content-type", "application/graphql")
            .body(graphql::Request::builder().query("{ me {name} }").build())
            .unwrap();
        let subgraph_req = SubgraphRequest::fake_builder()
            .subgraph_request(sub_req)
            .subgraph_name("test".to_string())
            .build();

        let _router_response = bad_request_subgraph_service
            .ready()
            .await
            .unwrap()
            .call(subgraph_req)
            .await
            .unwrap();
        assert_counter!(
            "acme.subgraph.error_reqs",
            2.0,
            graphql_error = opentelemetry::Value::Array(opentelemetry::Array::String(vec![
                "nope".into(),
                "nok".into()
            ])),
            subgraph.name = "test"
        );
        assert_histogram_not_exists!("http.client.request.duration", f64);
    }
    .with_metrics()
    .await;
}

#[tokio::test]
async fn test_custom_subgraph_instruments() {
    async {
        let plugin = Box::new(
            create_plugin_with_config(include_str!("testdata/custom_instruments.router.yaml"))
                .await,
        );

        let mut mock_bad_request_service = MockSubgraphService::new();
        mock_bad_request_service
            .expect_call()
            .times(2)
            .returning(move |req: SubgraphRequest| {
                let mut headers = HeaderMap::new();
                headers.insert(CONTENT_TYPE, "application/json".parse().unwrap());
                let errors = vec![
                    graphql::Error::builder()
                        .message("nope".to_string())
                        .extension_code("NOPE")
                        .build(),
                    graphql::Error::builder()
                        .message("nok".to_string())
                        .extension_code("NOK")
                        .build(),
                ];
                Ok(SubgraphResponse::fake_builder()
                    .context(req.context)
                    .status_code(StatusCode::BAD_REQUEST)
                    .headers(headers)
                    .errors(errors)
                    .build())
            });
        let mut bad_request_subgraph_service =
            plugin.subgraph_service("test", BoxService::new(mock_bad_request_service));
        let sub_req = http::Request::builder()
            .method("POST")
            .uri("http://test")
            .header("x-custom", "TEST")
            .header("conditional-custom", "X")
            .header("custom-length", "55")
            .header("content-length", "55")
            .header("content-type", "application/graphql")
            .body(graphql::Request::builder().query("{ me {name} }").build())
            .unwrap();
        let subgraph_req = SubgraphRequest::fake_builder()
            .subgraph_request(sub_req)
            .subgraph_name("test".to_string())
            .build();

        let _router_response = bad_request_subgraph_service
            .ready()
            .await
            .unwrap()
            .call(subgraph_req)
            .await
            .unwrap();

        assert_counter!(
            "acme.subgraph.error_reqs",
            1.0,
            graphql_error = opentelemetry::Value::Array(opentelemetry::Array::String(vec![
                "nope".into(),
                "nok".into()
            ])),
            subgraph.name = "test"
        );
        let sub_req = http::Request::builder()
            .method("POST")
            .uri("http://test")
            .header("x-custom", "TEST")
            .header("conditional-custom", "X")
            .header("custom-length", "55")
            .header("content-length", "55")
            .header("content-type", "application/graphql")
            .body(graphql::Request::builder().query("{ me {name} }").build())
            .unwrap();
        let subgraph_req = SubgraphRequest::fake_builder()
            .subgraph_request(sub_req)
            .subgraph_name("test".to_string())
            .build();

        let _router_response = bad_request_subgraph_service
            .ready()
            .await
            .unwrap()
            .call(subgraph_req)
            .await
            .unwrap();
        assert_counter!(
            "acme.subgraph.error_reqs",
            2.0,
            graphql_error = opentelemetry::Value::Array(opentelemetry::Array::String(vec![
                "nope".into(),
                "nok".into()
            ])),
            subgraph.name = "test"
        );
    }
    .with_metrics()
    .await;
}

#[tokio::test]
async fn test_field_instrumentation_sampler_with_preview_datadog_agent_sampling() {
    let plugin = create_plugin_with_config(include_str!(
        "testdata/config.field_instrumentation_sampler.router.yaml"
    ))
    .await;

    let ftv1_counter = Arc::new(AtomicUsize::new(0));
    let ftv1_counter_cloned = ftv1_counter.clone();

    let mut mock_request_service = MockSupergraphService::new();
    mock_request_service
        .expect_call()
        .times(10)
        .returning(move |req: SupergraphRequest| {
            if req
                .context
                .extensions()
                .with_lock(|lock| lock.contains_key::<EnableSubgraphFtv1>())
            {
                ftv1_counter_cloned.fetch_add(1, Ordering::Relaxed);
            }
            Ok(SupergraphResponse::fake_builder()
                .context(req.context)
                .status_code(StatusCode::OK)
                .header("content-type", "application/json")
                .data(json!({"errors": [{"message": "nope"}]}))
                .build()
                .unwrap())
        });
    let mut request_supergraph_service =
        plugin.supergraph_service(BoxService::new(mock_request_service));

    for _ in 0..10 {
        let supergraph_req = SupergraphRequest::fake_builder()
            .header("x-custom", "TEST")
            .header("conditional-custom", "X")
            .header("custom-length", "55")
            .header("content-length", "55")
            .header("content-type", "application/graphql")
            .query("Query test { me {name} }")
            .operation_name("test".to_string());
        let _router_response = request_supergraph_service
            .ready()
            .await
            .unwrap()
            .call(supergraph_req.build().unwrap())
            .await
            .unwrap()
            .next_response()
            .await
            .unwrap();
    }
    // It should be 100% because when we set preview_datadog_agent_sampling, we only take the value of field_level_instrumentation_sampler
    assert_eq!(ftv1_counter.load(Ordering::Relaxed), 10);
}

#[tokio::test]
async fn test_subgraph_metrics_ok() {
    async {
        let plugin =
            create_plugin_with_config(include_str!("testdata/custom_attributes.router.yaml")).await;

        let mut mock_subgraph_service = MockSubgraphService::new();
        mock_subgraph_service
            .expect_call()
            .times(1)
            .returning(move |req: SubgraphRequest| {
                let mut extension = Object::new();
                extension.insert(
                    serde_json_bytes::ByteString::from("status"),
                    serde_json_bytes::Value::String(ByteString::from(
                        "custom_error_for_propagation",
                    )),
                );
                let _ = req
                    .context
                    .insert("my_key", "my_custom_attribute_from_context".to_string())
                    .unwrap();
                Ok(SubgraphResponse::fake_builder()
                    .context(req.context)
                    .error(
                        Error::builder()
                            .message(String::from("an error occured"))
                            .extensions(extension)
                            .extension_code("FETCH_ERROR")
                            .build(),
                    )
                    .build())
            });

        let mut subgraph_service =
            plugin.subgraph_service("my_subgraph_name", BoxService::new(mock_subgraph_service));
        let subgraph_req = SubgraphRequest::fake_builder()
            .subgraph_request(
                http_ext::Request::fake_builder()
                    .header("test", "my_value_set")
                    .body(
                        Request::fake_builder()
                            .query(String::from("query { test }"))
                            .build(),
                    )
                    .build()
                    .unwrap(),
            )
            .subgraph_name("my_subgraph_name")
            .build();
        let _subgraph_response = subgraph_service
            .ready()
            .await
            .unwrap()
            .call(subgraph_req)
            .await
            .unwrap();

        assert_histogram_count!(
            "http.client.request.duration",
            1,
            "error" = "custom_error_for_propagation",
            "my_key" = "my_custom_attribute_from_context",
            "query_from_request" = "query { test }",
            "status" = 200,
            "subgraph" = "my_subgraph_name",
            "subgraph_error_extended_code" = "FETCH_ERROR"
        );
    }
    .with_metrics()
    .await;
}

#[tokio::test]
async fn test_subgraph_metrics_http_error() {
    async {
        let plugin =
            create_plugin_with_config(include_str!("testdata/custom_attributes.router.yaml")).await;

        let mut mock_subgraph_service_in_error = MockSubgraphService::new();
        mock_subgraph_service_in_error
            .expect_call()
            .times(1)
            .returning(move |_req: SubgraphRequest| {
                Err(Box::new(FetchError::SubrequestHttpError {
                    status_code: None,
                    service: String::from("my_subgraph_name_error"),
                    reason: String::from("cannot contact the subgraph"),
                }))
            });

        let mut subgraph_service = plugin.subgraph_service(
            "my_subgraph_name_error",
            BoxService::new(mock_subgraph_service_in_error),
        );

        let subgraph_req = SubgraphRequest::fake_builder()
            .subgraph_request(
                http_ext::Request::fake_builder()
                    .header("test", "my_value_set")
                    .body(
                        Request::fake_builder()
                            .query(String::from("query { test }"))
                            .build(),
                    )
                    .build()
                    .unwrap(),
            )
            .subgraph_name("my_subgraph_name_error")
            .build();
        let _subgraph_response = subgraph_service
            .ready()
            .await
            .unwrap()
            .call(subgraph_req)
            .await
            .expect_err("should be an error");

        assert_histogram_count!(
            "http.client.request.duration",
            1,
            "message" =
                "HTTP fetch failed from 'my_subgraph_name_error': cannot contact the subgraph",
            "subgraph" = "my_subgraph_name_error",
            "query_from_request" = "query { test }"
        );
    }
    .with_metrics()
    .await;
}

#[tokio::test]
async fn it_test_prometheus_wrong_endpoint() {
    async {
        let plugin =
            create_plugin_with_config(include_str!("testdata/prometheus.router.yaml")).await;

        let mut web_endpoint = plugin
            .web_endpoints()
            .into_iter()
            .next()
            .unwrap()
            .1
            .into_iter()
            .next()
            .unwrap()
            .into_router();

        let http_req_prom = http::Request::get("http://localhost:9090/WRONG/URL/metrics")
            .body(crate::services::router::body::empty())
            .unwrap();

        let resp = <axum::Router as tower::ServiceExt<http::Request<axum::body::Body>>>::ready(
            &mut web_endpoint,
        )
        .await
        .unwrap()
        .call(http_req_prom)
        .await
        .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }
    .with_metrics()
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn it_test_prometheus_metrics() {
    async {
        let plugin =
            create_plugin_with_config(include_str!("testdata/prometheus.router.yaml")).await;
        u64_histogram!("apollo.test.histo", "it's a test", 1u64);

        make_supergraph_request(plugin.as_ref()).await;
        assert_prometheus_metrics!(plugin);
    }
    .with_metrics()
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn it_test_prometheus_metrics_custom_buckets() {
    async {
        let plugin = create_plugin_with_config(include_str!(
            "testdata/prometheus_custom_buckets.router.yaml"
        ))
        .await;
        u64_histogram!("apollo.test.histo", "it's a test", 1u64);

        make_supergraph_request(plugin.as_ref()).await;
        assert_prometheus_metrics!(plugin);
    }
    .with_metrics()
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn it_test_prometheus_metrics_custom_buckets_for_specific_metrics() {
    async {
        let plugin = create_plugin_with_config(include_str!(
            "testdata/prometheus_custom_buckets_specific_metrics.router.yaml"
        ))
        .await;
        make_supergraph_request(plugin.as_ref()).await;
        u64_histogram!("apollo.test.histo", "it's a test", 1u64);
        assert_prometheus_metrics!(plugin);
    }
    .with_metrics()
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn it_test_prometheus_metrics_custom_view_drop() {
    async {
        let plugin = create_plugin_with_config(include_str!(
            "testdata/prometheus_custom_view_drop.router.yaml"
        ))
        .await;
        make_supergraph_request(plugin.as_ref()).await;
        assert_prometheus_metrics!(plugin);
    }
    .with_metrics()
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn it_test_prometheus_metrics_units_are_included() {
    async {
        let plugin =
            create_plugin_with_config(include_str!("testdata/prometheus.router.yaml")).await;
        u64_histogram_with_unit!("apollo.test.histo1", "no unit", "{request}", 1u64);
        f64_histogram_with_unit!("apollo.test.histo2", "unit", "s", 1f64);

        make_supergraph_request(plugin.as_ref()).await;
        assert_prometheus_metrics!(plugin);
    }
    .with_metrics()
    .await;
}

#[test]
fn it_test_send_headers_to_studio() {
    let fw_headers = ForwardHeaders::Only(vec![
        HeaderName::from_static("test"),
        HeaderName::from_static("apollo-x-name"),
    ]);
    let mut headers = HeaderMap::new();
    headers.insert(
        HeaderName::from_static("authorization"),
        HeaderValue::from_static("xxx"),
    );
    headers.insert(
        HeaderName::from_static("test"),
        HeaderValue::from_static("content"),
    );
    headers.insert(
        HeaderName::from_static("referer"),
        HeaderValue::from_static("test"),
    );
    headers.insert(
        HeaderName::from_static("foo"),
        HeaderValue::from_static("bar"),
    );
    headers.insert(
        HeaderName::from_static("apollo-x-name"),
        HeaderValue::from_static("polaris"),
    );
    let filtered_headers = super::filter_headers(&headers, &fw_headers);
    assert_eq!(
        filtered_headers.as_str(),
        r#"{"apollo-x-name":["polaris"],"test":["content"]}"#
    );
    let filtered_headers = super::filter_headers(&headers, &ForwardHeaders::None);
    assert_eq!(filtered_headers.as_str(), "{}");
}

#[tokio::test]
async fn test_custom_trace_id_propagator_strip_dashes_in_trace_id() {
    let header = String::from("x-trace-id");
    let trace_id = String::from("04f9e396-465c-4840-bc2b-f493b8b1a7fc");
    let expected_trace_id = String::from("04f9e396465c4840bc2bf493b8b1a7fc");

    let propagator = CustomTraceIdPropagator::new(header.clone(), TraceIdFormat::Uuid);
    let mut headers: HashMap<String, String> = HashMap::new();
    headers.insert(header, trace_id);
    let span = propagator.extract_span_context(&headers);
    assert!(span.is_some());
    assert_eq!(span.unwrap().trace_id().to_string(), expected_trace_id);
}

#[test]
fn test_header_propagation_format() {
    struct Injected(HashMap<String, String>);
    impl Injector for Injected {
        fn set(&mut self, key: &str, value: String) {
            self.0.insert(key.to_string(), value);
        }
    }
    let mut injected = Injected(HashMap::new());
    let _ctx = opentelemetry::Context::new()
        .with_remote_span_context(SpanContext::new(
            TraceId::from_u128(0x04f9e396465c4840bc2bf493b8b1a7fc),
            SpanId::INVALID,
            TraceFlags::default(),
            false,
            TraceState::default(),
        ))
        .attach();
    let propagator = CustomTraceIdPropagator::new("my_header".to_string(), TraceIdFormat::Uuid);
    propagator.inject_context(&opentelemetry::Context::current(), &mut injected);
    assert_eq!(
        injected.0.get("my_header").unwrap(),
        "04f9e396-465c-4840-bc2b-f493b8b1a7fc"
    );
}

#[derive(Clone)]
struct CostContext {
    pub(crate) estimated: f64,
    pub(crate) actual: f64,
    pub(crate) result: &'static str,
    pub(crate) strategy: &'static str,
}

async fn make_failed_demand_control_request(plugin: &dyn DynPlugin, cost_details: CostContext) {
    let mut mock_service = MockSupergraphService::new();
    mock_service
        .expect_call()
        .times(1)
        .returning(move |req: SupergraphRequest| {
            req.context.extensions().with_lock(|lock| {
                lock.insert(cost_details.clone());
            });
            req.context
                .insert(COST_ESTIMATED_KEY, cost_details.estimated)
                .unwrap();
            req.context
                .insert(COST_ACTUAL_KEY, cost_details.actual)
                .unwrap();
            req.context
                .insert(COST_RESULT_KEY, cost_details.result.to_string())
                .unwrap();
            req.context
                .insert(COST_STRATEGY_KEY, cost_details.strategy.to_string())
                .unwrap();

            let errors = if cost_details.result == "COST_ESTIMATED_TOO_EXPENSIVE" {
                DemandControlError::EstimatedCostTooExpensive {
                    estimated_cost: cost_details.estimated,
                    max_cost: (cost_details.estimated - 5.0).max(0.0),
                }
                .into_graphql_errors()
                .unwrap()
            } else if cost_details.result == "COST_ACTUAL_TOO_EXPENSIVE" {
                DemandControlError::ActualCostTooExpensive {
                    actual_cost: cost_details.actual,
                    max_cost: (cost_details.actual - 5.0).max(0.0),
                }
                .into_graphql_errors()
                .unwrap()
            } else {
                Vec::new()
            };

            SupergraphResponse::fake_builder()
                .context(req.context)
                .data(
                    serde_json::to_value(graphql::Response::builder().errors(errors).build())
                        .unwrap(),
                )
                .build()
        });

    let mut service = plugin.supergraph_service(BoxService::new(mock_service));
    let router_req = SupergraphRequest::fake_builder().build().unwrap();
    let _router_response = service
        .ready()
        .await
        .unwrap()
        .call(router_req)
        .await
        .unwrap()
        .next_response()
        .await
        .unwrap();
}

#[tokio::test]
async fn test_demand_control_delta_filter() {
    async {
        let plugin = create_plugin_with_config(include_str!(
            "testdata/demand_control_delta_filter.router.yaml"
        ))
        .await;
        make_failed_demand_control_request(
            plugin.as_ref(),
            CostContext {
                estimated: 10.0,
                actual: 8.0,
                result: "COST_ACTUAL_TOO_EXPENSIVE",
                strategy: "static_estimated",
            },
        )
        .await;

        assert_histogram_sum!("cost.rejected.operations", 8.0);
    }
    .with_metrics()
    .await;
}

#[tokio::test]
async fn test_demand_control_result_filter() {
    async {
        let plugin = create_plugin_with_config(include_str!(
            "testdata/demand_control_result_filter.router.yaml"
        ))
        .await;
        make_failed_demand_control_request(
            plugin.as_ref(),
            CostContext {
                estimated: 10.0,
                actual: 0.0,
                result: "COST_ESTIMATED_TOO_EXPENSIVE",
                strategy: "static_estimated",
            },
        )
        .await;

        assert_histogram_sum!("cost.rejected.operations", 10.0);
    }
    .with_metrics()
    .await;
}

#[tokio::test]
async fn test_demand_control_result_attributes() {
    async {
        let plugin = create_plugin_with_config(include_str!(
            "testdata/demand_control_result_attribute.router.yaml"
        ))
        .await;
        make_failed_demand_control_request(
            plugin.as_ref(),
            CostContext {
                estimated: 10.0,
                actual: 0.0,
                result: "COST_ESTIMATED_TOO_EXPENSIVE",
                strategy: "static_estimated",
            },
        )
        .await;

        assert_histogram_sum!(
            "cost.estimated",
            10.0,
            "cost.result" = "COST_ESTIMATED_TOO_EXPENSIVE"
        );
    }
    .with_metrics()
    .await;
}
