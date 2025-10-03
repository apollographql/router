use std::collections::HashMap;
use std::collections::HashSet;

use http::Method;
use http::StatusCode;
use http::Uri;
use http::header::CONTENT_TYPE;
use mime::APPLICATION_JSON;
use opentelemetry::KeyValue;
use serde_json_bytes::Value;
use serde_json_bytes::json;
use uuid::Uuid;

use crate::Context;
use crate::context::COUNTED_ERRORS;
use crate::context::OPERATION_KIND;
use crate::context::OPERATION_NAME;
use crate::graphql;
use crate::json_ext::Path;
use crate::metrics::FutureMetricsExt;
use crate::plugins::telemetry::CLIENT_NAME;
use crate::plugins::telemetry::CLIENT_VERSION;
use crate::plugins::telemetry::Telemetry;
use crate::plugins::telemetry::apollo::ErrorConfiguration;
use crate::plugins::telemetry::apollo::ErrorRedactionPolicy;
use crate::plugins::telemetry::apollo::ErrorsConfiguration;
use crate::plugins::telemetry::apollo::ExtendedErrorMetricsMode;
use crate::plugins::telemetry::apollo::SubgraphErrorConfig;
use crate::plugins::telemetry::error_counter::count_execution_errors;
use crate::plugins::telemetry::error_counter::count_operation_errors;
use crate::plugins::telemetry::error_counter::count_router_errors;
use crate::plugins::telemetry::error_counter::count_subgraph_errors;
use crate::plugins::telemetry::error_counter::count_supergraph_errors;
use crate::plugins::telemetry::error_counter::unwrap_from_context;
use crate::plugins::test::PluginTestHarness;
use crate::query_planner::APOLLO_OPERATION_ID;
use crate::services::ExecutionResponse;
use crate::services::RouterResponse;
use crate::services::SubgraphResponse;
use crate::services::SupergraphResponse;
use crate::services::execution;
use crate::services::router;
use crate::services::subgraph;
use crate::services::subgraph::SubgraphRequestId;
use crate::services::supergraph;
use crate::spec::query::EXTENSIONS_VALUE_COMPLETION_KEY;

#[tokio::test]
async fn test_count_supergraph_errors_with_no_previously_counted_errors() {
    async {
        let config = ErrorsConfiguration {
            preview_extended_error_metrics: ExtendedErrorMetricsMode::Enabled,
            ..Default::default()
        };

        let context = Context::default();
        let _ = context.insert(APOLLO_OPERATION_ID, "some-id".to_string());
        let _ = context.insert(OPERATION_NAME, "SomeOperation".to_string());
        let _ = context.insert(OPERATION_KIND, "query".to_string());
        let _ = context.insert(CLIENT_NAME, "client-1".to_string());
        let _ = context.insert(CLIENT_VERSION, "version-1".to_string());

        let error_id = Uuid::new_v4();
        let new_response = count_supergraph_errors(
            SupergraphResponse::fake_builder()
                .header("Accept", "application/json")
                .context(context)
                .status_code(StatusCode::BAD_REQUEST)
                .errors(vec![
                    graphql::Error::builder()
                        .message("You did a bad request.")
                        .extension_code("GRAPHQL_VALIDATION_FAILED")
                        .apollo_id(error_id)
                        .build(),
                ])
                .build()
                .unwrap(),
            &config,
        )
        .await;

        assert_counter!(
            "apollo.router.operations.error",
            1,
            "apollo.operation.id" = "some-id",
            "graphql.operation.name" = "SomeOperation",
            "graphql.operation.type" = "query",
            "apollo.client.name" = "client-1",
            "apollo.client.version" = "version-1",
            "graphql.error.extensions.code" = "GRAPHQL_VALIDATION_FAILED",
            "graphql.error.extensions.severity" = "ERROR",
            "graphql.error.path" = "",
            "apollo.router.error.service" = ""
        );

        assert_counter!(
            "apollo.router.graphql_error",
            1,
            code = "GRAPHQL_VALIDATION_FAILED"
        );

        assert_eq!(
            unwrap_from_context::<HashSet<Uuid>>(&new_response.context, COUNTED_ERRORS),
            HashSet::from([error_id])
        )
    }
    .with_metrics()
    .await;
}

#[tokio::test]
async fn test_count_supergraph_errors_with_previously_counted_errors() {
    async {
        let config = ErrorsConfiguration {
            preview_extended_error_metrics: ExtendedErrorMetricsMode::Enabled,
            ..Default::default()
        };

        let context = Context::default();
        let validation_error_id = Uuid::new_v4();
        let custom_error_id = Uuid::new_v4();

        let _ = context.insert(COUNTED_ERRORS, HashSet::from([validation_error_id]));

        let _ = context.insert(APOLLO_OPERATION_ID, "some-id".to_string());
        let _ = context.insert(OPERATION_NAME, "SomeOperation".to_string());
        let _ = context.insert(OPERATION_KIND, "query".to_string());
        let _ = context.insert(CLIENT_NAME, "client-1".to_string());
        let _ = context.insert(CLIENT_VERSION, "version-1".to_string());

        let new_response = count_supergraph_errors(
            SupergraphResponse::fake_builder()
                .header("Accept", "application/json")
                .context(context)
                .status_code(StatusCode::BAD_REQUEST)
                .error(
                    graphql::Error::builder()
                        .message("You did a bad request.")
                        .extension_code("GRAPHQL_VALIDATION_FAILED")
                        .apollo_id(validation_error_id)
                        .build(),
                )
                .error(
                    graphql::Error::builder()
                        .message("Custom error text")
                        .extension_code("CUSTOM_ERROR")
                        .apollo_id(custom_error_id)
                        .build(),
                )
                .build()
                .unwrap(),
            &config,
        )
        .await;

        assert_counter!(
            "apollo.router.operations.error",
            1,
            "apollo.operation.id" = "some-id",
            "graphql.operation.name" = "SomeOperation",
            "graphql.operation.type" = "query",
            "apollo.client.name" = "client-1",
            "apollo.client.version" = "version-1",
            "graphql.error.extensions.code" = "CUSTOM_ERROR",
            "graphql.error.extensions.severity" = "ERROR",
            "graphql.error.path" = "",
            "apollo.router.error.service" = ""
        );

        assert_counter!("apollo.router.graphql_error", 1, code = "CUSTOM_ERROR");

        assert_counter_not_exists!(
            "apollo.router.operations.error",
            u64,
            "apollo.operation.id" = "some-id",
            "graphql.operation.name" = "SomeOperation",
            "graphql.operation.type" = "query",
            "apollo.client.name" = "client-1",
            "apollo.client.version" = "version-1",
            "graphql.error.extensions.code" = "GRAPHQL_VALIDATION_FAILED",
            "graphql.error.extensions.severity" = "ERROR",
            "graphql.error.path" = "",
            "apollo.router.error.service" = ""
        );

        assert_counter_not_exists!(
            "apollo.router.graphql_error",
            u64,
            code = "GRAPHQL_VALIDATION_FAILED"
        );

        assert_eq!(
            unwrap_from_context::<HashSet<Uuid>>(&new_response.context, COUNTED_ERRORS),
            HashSet::from([validation_error_id, custom_error_id])
        )
    }
    .with_metrics()
    .await;
}

#[tokio::test]
async fn test_count_subgraph_errors_with_include_subgraphs_enabled() {
    async {
        let config = ErrorsConfiguration {
            preview_extended_error_metrics: ExtendedErrorMetricsMode::Enabled,
            subgraph: SubgraphErrorConfig {
                subgraphs: HashMap::from([(
                    "some-subgraph".to_string(),
                    ErrorConfiguration {
                        send: true,
                        redact: false,
                        redaction_policy: ErrorRedactionPolicy::Strict,
                    },
                )]),
                ..Default::default()
            },
        };

        let context = Context::default();
        let _ = context.insert(APOLLO_OPERATION_ID, "some-id".to_string());
        let _ = context.insert(OPERATION_NAME, "SomeOperation".to_string());
        let _ = context.insert(OPERATION_KIND, "query".to_string());
        let _ = context.insert(CLIENT_NAME, "client-1".to_string());
        let _ = context.insert(CLIENT_VERSION, "version-1".to_string());

        let error_id = Uuid::new_v4();
        let new_response = count_subgraph_errors(
            SubgraphResponse::fake_builder()
                .context(context)
                .subgraph_name("some-subgraph".to_string())
                .status_code(StatusCode::BAD_REQUEST)
                .errors(vec![
                    graphql::Error::builder()
                        .message("You did a bad request.")
                        .path(Path::from("obj/field"))
                        .extension_code("GRAPHQL_VALIDATION_FAILED")
                        .extension("service", "some-subgraph")
                        .apollo_id(error_id)
                        .build(),
                ])
                .build(),
            &config,
        )
        .await;

        assert_counter!(
            "apollo.router.operations.error",
            1,
            "apollo.operation.id" = "some-id",
            "graphql.operation.name" = "SomeOperation",
            "graphql.operation.type" = "query",
            "apollo.client.name" = "client-1",
            "apollo.client.version" = "version-1",
            "graphql.error.extensions.code" = "GRAPHQL_VALIDATION_FAILED",
            "graphql.error.extensions.severity" = "ERROR",
            "graphql.error.path" = "/obj/field",
            "apollo.router.error.service" = "some-subgraph"
        );

        assert_counter!(
            "apollo.router.graphql_error",
            1,
            code = "GRAPHQL_VALIDATION_FAILED"
        );

        assert_eq!(
            unwrap_from_context::<HashSet<Uuid>>(&new_response.context, COUNTED_ERRORS),
            HashSet::from([error_id])
        )
    }
    .with_metrics()
    .await;
}

#[tokio::test]
async fn test_count_subgraph_errors_with_include_subgraphs_disabled() {
    async {
        let config = ErrorsConfiguration {
            preview_extended_error_metrics: ExtendedErrorMetricsMode::Enabled,
            subgraph: SubgraphErrorConfig {
                subgraphs: HashMap::from([(
                    "some-subgraph".to_string(),
                    ErrorConfiguration {
                        send: false,
                        redact: true,
                        redaction_policy: ErrorRedactionPolicy::Strict,
                    },
                )]),
                ..Default::default()
            },
        };

        let context = Context::default();
        let error_id = Uuid::new_v4();
        let new_response = count_subgraph_errors(
            SubgraphResponse::fake_builder()
                .context(context)
                .subgraph_name("some-subgraph".to_string())
                .status_code(StatusCode::BAD_REQUEST)
                .errors(vec![
                    graphql::Error::builder()
                        .message("You did a bad request.")
                        .path(Path::from("obj/field"))
                        .extension_code("GRAPHQL_VALIDATION_FAILED")
                        .extension("service", "some-subgraph")
                        .apollo_id(error_id)
                        .build(),
                ])
                .build(),
            &config,
        )
        .await;

        assert_counter_not_exists!(
            "apollo.router.operations.error",
            u64,
            "apollo.operation.id" = "some-id",
            "graphql.operation.name" = "SomeOperation",
            "graphql.operation.type" = "query",
            "apollo.client.name" = "client-1",
            "apollo.client.version" = "version-1",
            "graphql.error.extensions.code" = "GRAPHQL_VALIDATION_FAILED",
            "graphql.error.extensions.severity" = "ERROR",
            "graphql.error.path" = "/obj/field",
            "apollo.router.error.service" = "some-subgraph"
        );

        assert_counter!(
            // TODO(tim): is this a bug?  Should we not count these when the subgraph is excluded?
            "apollo.router.graphql_error",
            1,
            code = "GRAPHQL_VALIDATION_FAILED"
        );

        assert_eq!(
            unwrap_from_context::<HashSet<Uuid>>(&new_response.context, COUNTED_ERRORS),
            HashSet::from([error_id])
        )
    }
    .with_metrics()
    .await;
}

#[tokio::test]
async fn test_count_execution_errors() {
    async {
        let config = ErrorsConfiguration {
            preview_extended_error_metrics: ExtendedErrorMetricsMode::Enabled,
            ..Default::default()
        };

        let context = Context::default();
        let _ = context.insert(APOLLO_OPERATION_ID, "some-id".to_string());
        let _ = context.insert(OPERATION_NAME, "SomeOperation".to_string());
        let _ = context.insert(OPERATION_KIND, "query".to_string());
        let _ = context.insert(CLIENT_NAME, "client-1".to_string());
        let _ = context.insert(CLIENT_VERSION, "version-1".to_string());

        let error_id = Uuid::new_v4();
        let new_response = count_execution_errors(
            ExecutionResponse::fake_builder()
                .context(context)
                .status_code(StatusCode::BAD_REQUEST)
                .errors(vec![
                    graphql::Error::builder()
                        .message("You did a bad request.")
                        .path(Path::from("obj/field"))
                        .extension_code("GRAPHQL_VALIDATION_FAILED")
                        .extension("service", "some-subgraph")
                        .apollo_id(error_id)
                        .build(),
                ])
                .build()
                .unwrap(),
            &config,
        )
        .await;

        assert_counter!(
            "apollo.router.operations.error",
            1,
            "apollo.operation.id" = "some-id",
            "graphql.operation.name" = "SomeOperation",
            "graphql.operation.type" = "query",
            "apollo.client.name" = "client-1",
            "apollo.client.version" = "version-1",
            "graphql.error.extensions.code" = "GRAPHQL_VALIDATION_FAILED",
            "graphql.error.extensions.severity" = "ERROR",
            "graphql.error.path" = "/obj/field",
            "apollo.router.error.service" = "some-subgraph"
        );

        assert_counter!(
            "apollo.router.graphql_error",
            1,
            code = "GRAPHQL_VALIDATION_FAILED"
        );

        assert_eq!(
            unwrap_from_context::<HashSet<Uuid>>(&new_response.context, COUNTED_ERRORS),
            HashSet::from([error_id])
        )
    }
    .with_metrics()
    .await;
}

#[tokio::test]
async fn test_count_router_errors() {
    async {
        let config = ErrorsConfiguration {
            preview_extended_error_metrics: ExtendedErrorMetricsMode::Enabled,
            ..Default::default()
        };

        let context = Context::default();
        let _ = context.insert(APOLLO_OPERATION_ID, "some-id".to_string());
        let _ = context.insert(OPERATION_NAME, "SomeOperation".to_string());
        let _ = context.insert(OPERATION_KIND, "query".to_string());
        let _ = context.insert(CLIENT_NAME, "client-1".to_string());
        let _ = context.insert(CLIENT_VERSION, "version-1".to_string());

        let error_id = Uuid::new_v4();
        let new_response = count_router_errors(
            RouterResponse::fake_builder()
                .context(context)
                .status_code(StatusCode::BAD_REQUEST)
                .errors(vec![
                    graphql::Error::builder()
                        .message("You did a bad request.")
                        .path(Path::from("obj/field"))
                        .extension_code("GRAPHQL_VALIDATION_FAILED")
                        .extension("service", "some-subgraph")
                        .apollo_id(error_id)
                        .build(),
                ])
                .build()
                .unwrap(),
            &config,
        )
        .await;

        assert_counter!(
            "apollo.router.operations.error",
            1,
            "apollo.operation.id" = "some-id",
            "graphql.operation.name" = "SomeOperation",
            "graphql.operation.type" = "query",
            "apollo.client.name" = "client-1",
            "apollo.client.version" = "version-1",
            "graphql.error.extensions.code" = "GRAPHQL_VALIDATION_FAILED",
            "graphql.error.extensions.severity" = "ERROR",
            "graphql.error.path" = "/obj/field",
            "apollo.router.error.service" = "some-subgraph"
        );

        assert_counter!(
            "apollo.router.graphql_error",
            1,
            code = "GRAPHQL_VALIDATION_FAILED"
        );

        assert_eq!(
            unwrap_from_context::<HashSet<Uuid>>(&new_response.context, COUNTED_ERRORS),
            HashSet::from([error_id])
        )
    }
    .with_metrics()
    .await;
}

#[tokio::test]
async fn test_count_operation_errors_with_extended_config_enabled() {
    async {
        let config = ErrorsConfiguration {
            preview_extended_error_metrics: ExtendedErrorMetricsMode::Enabled,
            ..Default::default()
        };

        let context = Context::default();
        let _ = context.insert(APOLLO_OPERATION_ID, "some-id".to_string());
        let _ = context.insert(OPERATION_NAME, "SomeOperation".to_string());
        let _ = context.insert(OPERATION_KIND, "query".to_string());
        let _ = context.insert(CLIENT_NAME, "client-1".to_string());
        let _ = context.insert(CLIENT_VERSION, "version-1".to_string());

        let error = graphql::Error::builder()
            .message("some error")
            .extension_code("SOME_ERROR_CODE")
            .extension("service", "mySubgraph")
            .path(Path::from("obj/field"))
            .build();

        count_operation_errors(&[error], &context, &config);

        assert_counter!(
            "apollo.router.operations.error",
            1,
            "apollo.operation.id" = "some-id",
            "graphql.operation.name" = "SomeOperation",
            "graphql.operation.type" = "query",
            "apollo.client.name" = "client-1",
            "apollo.client.version" = "version-1",
            "graphql.error.extensions.code" = "SOME_ERROR_CODE",
            "graphql.error.extensions.severity" = "ERROR",
            "graphql.error.path" = "/obj/field",
            "apollo.router.error.service" = "mySubgraph"
        );

        assert_counter!("apollo.router.graphql_error", 1, code = "SOME_ERROR_CODE");
    }
    .with_metrics()
    .await;
}

#[tokio::test]
async fn test_count_operation_errors_with_all_json_types_and_extended_config_enabled() {
    async {
        let config = ErrorsConfiguration {
            preview_extended_error_metrics: ExtendedErrorMetricsMode::Enabled,
            ..Default::default()
        };

        let context = Context::default();
        let _ = context.insert(APOLLO_OPERATION_ID, "some-id".to_string());
        let _ = context.insert(OPERATION_NAME, "SomeOperation".to_string());
        let _ = context.insert(OPERATION_KIND, "query".to_string());
        let _ = context.insert(CLIENT_NAME, "client-1".to_string());
        let _ = context.insert(CLIENT_VERSION, "version-1".to_string());

        let codes = [
            json!("VALID_ERROR_CODE"),
            json!(400),
            json!(true),
            Value::Null,
            json!(["code1", "code2"]),
            json!({"inner": "myCode"}),
        ];

        let errors = codes.map(|code| {
            graphql::Error::from_value(json!(
            {
              "message": "error occurred",
              "extensions": {
                "code": code,
                "service": "mySubgraph"
              },
              "path": ["obj", "field"]
            }
            ))
            .unwrap()
        });

        count_operation_errors(&errors, &context, &config);

        assert_counter!(
            "apollo.router.operations.error",
            1,
            "apollo.operation.id" = "some-id",
            "graphql.operation.name" = "SomeOperation",
            "graphql.operation.type" = "query",
            "apollo.client.name" = "client-1",
            "apollo.client.version" = "version-1",
            "graphql.error.extensions.code" = "VALID_ERROR_CODE",
            "graphql.error.extensions.severity" = "ERROR",
            "graphql.error.path" = "/obj/field",
            "apollo.router.error.service" = "mySubgraph"
        );

        assert_counter!("apollo.router.graphql_error", 1, code = "VALID_ERROR_CODE");

        assert_counter!(
            "apollo.router.operations.error",
            1,
            "apollo.operation.id" = "some-id",
            "graphql.operation.name" = "SomeOperation",
            "graphql.operation.type" = "query",
            "apollo.client.name" = "client-1",
            "apollo.client.version" = "version-1",
            "graphql.error.extensions.code" = "400",
            "graphql.error.extensions.severity" = "ERROR",
            "graphql.error.path" = "/obj/field",
            "apollo.router.error.service" = "mySubgraph"
        );

        assert_counter!("apollo.router.graphql_error", 1, code = "400");

        // Code is ignored for null, arrays, booleans and objects

        assert_counter!(
            "apollo.router.operations.error",
            4,
            "apollo.operation.id" = "some-id",
            "graphql.operation.name" = "SomeOperation",
            "graphql.operation.type" = "query",
            "apollo.client.name" = "client-1",
            "apollo.client.version" = "version-1",
            "graphql.error.extensions.code" = "",
            "graphql.error.extensions.severity" = "ERROR",
            "graphql.error.path" = "/obj/field",
            "apollo.router.error.service" = "mySubgraph"
        );

        // Ensure these have NO attributes
        assert_counter!("apollo.router.graphql_error", 4, &[]);
    }
    .with_metrics()
    .await;
}

#[tokio::test]
async fn test_count_operation_errors_with_duplicate_errors_and_extended_config_enabled() {
    async {
        let config = ErrorsConfiguration {
            preview_extended_error_metrics: ExtendedErrorMetricsMode::Enabled,
            ..Default::default()
        };

        let context = Context::default();
        let _ = context.insert(APOLLO_OPERATION_ID, "some-id".to_string());
        let _ = context.insert(OPERATION_NAME, "SomeOperation".to_string());
        let _ = context.insert(OPERATION_KIND, "query".to_string());
        let _ = context.insert(CLIENT_NAME, "client-1".to_string());
        let _ = context.insert(CLIENT_VERSION, "version-1".to_string());

        let codes = [
            json!("VALID_ERROR_CODE"),
            Value::Null,
            json!("VALID_ERROR_CODE"),
            Value::Null,
        ];

        let errors = codes.map(|code| {
            graphql::Error::from_value(json!(
            {
              "message": "error occurred",
              "extensions": {
                "code": code,
                "service": "mySubgraph"
              },
              "path": ["obj", "field"]
            }
            ))
            .unwrap()
        });

        count_operation_errors(&errors, &context, &config);

        assert_counter!(
            "apollo.router.operations.error",
            2,
            "apollo.operation.id" = "some-id",
            "graphql.operation.name" = "SomeOperation",
            "graphql.operation.type" = "query",
            "apollo.client.name" = "client-1",
            "apollo.client.version" = "version-1",
            "graphql.error.extensions.code" = "VALID_ERROR_CODE",
            "graphql.error.extensions.severity" = "ERROR",
            "graphql.error.path" = "/obj/field",
            "apollo.router.error.service" = "mySubgraph"
        );

        assert_counter!("apollo.router.graphql_error", 2, code = "VALID_ERROR_CODE");

        assert_counter!(
            "apollo.router.operations.error",
            2,
            "apollo.operation.id" = "some-id",
            "graphql.operation.name" = "SomeOperation",
            "graphql.operation.type" = "query",
            "apollo.client.name" = "client-1",
            "apollo.client.version" = "version-1",
            "graphql.error.extensions.code" = "",
            "graphql.error.extensions.severity" = "ERROR",
            "graphql.error.path" = "/obj/field",
            "apollo.router.error.service" = "mySubgraph"
        );

        assert_counter!("apollo.router.graphql_error", 2);
    }
    .with_metrics()
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn test_subgraph_error_counting() {
    async {
        let operation_name = "operationName";
        let operation_type = "query";
        let operation_id = "opId";
        let client_name = "client";
        let client_version = "version";
        let previously_counted_error_id = Uuid::new_v4();
        let subgraph_name = "mySubgraph";
        let subgraph_request_id = SubgraphRequestId("5678".to_string());
        let example_response = graphql::Response::builder()
            .data(json!({"data": null}))
            .errors(vec![
                graphql::Error::builder()
                    .message("previously counted error")
                    .extension_code("ERROR_CODE")
                    .extension("service", subgraph_name)
                    .path(Path::from("obj/field"))
                    .apollo_id(previously_counted_error_id)
                    .build(),
                graphql::Error::builder()
                    .message("error in supergraph layer")
                    .extension_code("SUPERGRAPH_CODE")
                    .extension("service", subgraph_name)
                    .path(Path::from("obj/field"))
                    .build(),
            ])
            .build();
        let config = json!({
            "telemetry":{
                "apollo": {
                    "errors": {
                        "preview_extended_error_metrics": "enabled",
                        "subgraph": {
                            "subgraphs": {
                                "myIgnoredSubgraph": {
                                    "send": false,
                                }
                            }
                        }
                    }
                }
            }
        })
        .to_string();
        let test_harness: PluginTestHarness<Telemetry> = PluginTestHarness::builder()
            .config(&config)
            .build()
            .await
            .expect("test harness");

        let router_service = test_harness.subgraph_service(subgraph_name, move |req| {
            let subgraph_response = example_response.clone();
            let subgraph_request_id = subgraph_request_id.clone();
            async move {
                Ok(SubgraphResponse::new_from_response(
                    http::Response::new(subgraph_response.clone()),
                    req.context,
                    subgraph_name.to_string(),
                    subgraph_request_id,
                ))
            }
        });

        let context = Context::new();
        context.insert_json_value(APOLLO_OPERATION_ID, operation_id.into());
        context.insert_json_value(OPERATION_NAME, operation_name.into());
        context.insert_json_value(OPERATION_KIND, operation_type.into());
        context.insert_json_value(CLIENT_NAME, client_name.into());
        context.insert_json_value(CLIENT_VERSION, client_version.into());
        let _ = context.insert(COUNTED_ERRORS, HashSet::from([previously_counted_error_id]));

        let request = subgraph::Request::fake_builder()
            .subgraph_name(subgraph_name)
            .context(context)
            .build();
        router_service.call(request).await.unwrap();

        assert_counter!(
            "apollo.router.operations.error",
            1,
            &[
                KeyValue::new("apollo.operation.id", operation_id),
                KeyValue::new("graphql.operation.name", operation_name),
                KeyValue::new("graphql.operation.type", operation_type),
                KeyValue::new("apollo.client.name", client_name),
                KeyValue::new("apollo.client.version", client_version),
                KeyValue::new("graphql.error.extensions.code", "SUPERGRAPH_CODE"),
                KeyValue::new("graphql.error.extensions.severity", "ERROR"),
                KeyValue::new("graphql.error.path", "/obj/field"),
                KeyValue::new("apollo.router.error.service", "mySubgraph"),
            ]
        );
        assert_counter!("apollo.router.graphql_error", 1, code = "SUPERGRAPH_CODE");

        assert_counter_not_exists!(
            "apollo.router.operations.error",
            u64,
            "apollo.operation.id" = operation_id,
            "graphql.operation.name" = operation_name,
            "graphql.operation.type" = operation_type,
            "apollo.client.name" = client_name,
            "apollo.client.version" = client_version,
            "graphql.error.extensions.code" = "ERROR_CODE",
            "graphql.error.extensions.severity" = "ERROR",
            "graphql.error.path" = "/obj/field",
            "apollo.router.error.service" = "mySubgraph"
        );

        assert_counter_not_exists!("apollo.router.graphql_error", u64, code = "ERROR_CODE");
    }
    .with_metrics()
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn test_execution_error_counting() {
    async {
        let operation_name = "operationName";
        let operation_type = "query";
        let operation_id = "opId";
        let client_name = "client";
        let client_version = "version";
        let previously_counted_error_id = Uuid::new_v4();
        let subgraph_name = "mySubgraph";
        let example_response = graphql::Response::builder()
            .data(json!({"data": null}))
            .errors(vec![
                graphql::Error::builder()
                    .message("previously counted error")
                    .extension_code("ERROR_CODE")
                    .extension("service", subgraph_name)
                    .path(Path::from("obj/field"))
                    .apollo_id(previously_counted_error_id)
                    .build(),
                graphql::Error::builder()
                    .message("error in supergraph layer")
                    .extension_code("SUPERGRAPH_CODE")
                    .extension("service", subgraph_name)
                    .path(Path::from("obj/field"))
                    .build(),
            ])
            .build();
        let config = json!({
            "telemetry":{
                "apollo": {
                    "errors": {
                        "preview_extended_error_metrics": "enabled",
                        "subgraph": {
                            "subgraphs": {
                                "myIgnoredSubgraph": {
                                    "send": false,
                                }
                            }
                        }
                    }
                }
            }
        })
        .to_string();
        let test_harness: PluginTestHarness<Telemetry> = PluginTestHarness::builder()
            .config(&config)
            .build()
            .await
            .expect("test harness");

        let router_service = test_harness.execution_service(move |req| {
            let execution_response = example_response.clone();
            async move {
                Ok(ExecutionResponse::new_from_graphql_response(
                    execution_response.clone(),
                    req.context,
                ))
            }
        });

        let context = Context::new();
        context.insert_json_value(APOLLO_OPERATION_ID, operation_id.into());
        context.insert_json_value(OPERATION_NAME, operation_name.into());
        context.insert_json_value(OPERATION_KIND, operation_type.into());
        context.insert_json_value(CLIENT_NAME, client_name.into());
        context.insert_json_value(CLIENT_VERSION, client_version.into());
        let _ = context.insert(COUNTED_ERRORS, HashSet::from([previously_counted_error_id]));

        router_service
            .call(execution::Request::fake_builder().context(context).build())
            .await
            .unwrap();

        assert_counter!(
            "apollo.router.operations.error",
            1,
            &[
                KeyValue::new("apollo.operation.id", operation_id),
                KeyValue::new("graphql.operation.name", operation_name),
                KeyValue::new("graphql.operation.type", operation_type),
                KeyValue::new("apollo.client.name", client_name),
                KeyValue::new("apollo.client.version", client_version),
                KeyValue::new("graphql.error.extensions.code", "SUPERGRAPH_CODE"),
                KeyValue::new("graphql.error.extensions.severity", "ERROR"),
                KeyValue::new("graphql.error.path", "/obj/field"),
                KeyValue::new("apollo.router.error.service", "mySubgraph"),
            ]
        );
        assert_counter!("apollo.router.graphql_error", 1, code = "SUPERGRAPH_CODE");

        assert_counter_not_exists!(
            "apollo.router.operations.error",
            u64,
            "apollo.operation.id" = operation_id,
            "graphql.operation.name" = operation_name,
            "graphql.operation.type" = operation_type,
            "apollo.client.name" = client_name,
            "apollo.client.version" = client_version,
            "graphql.error.extensions.code" = "ERROR_CODE",
            "graphql.error.extensions.severity" = "ERROR",
            "graphql.error.path" = "/obj/field",
            "apollo.router.error.service" = "mySubgraph"
        );

        assert_counter_not_exists!("apollo.router.graphql_error", u64, code = "ERROR_CODE");
    }
    .with_metrics()
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn test_supergraph_error_counting() {
    async {
        let query = "query operationName { __typename }";
        let operation_name = "operationName";
        let operation_type = "query";
        let operation_id = "opId";
        let client_name = "client";
        let client_version = "version";
        let previously_counted_error_id = Uuid::new_v4();
        let subgraph_name = "mySubgraph";
        let example_response = graphql::Response::builder()
            .data(json!({"data": null}))
            .errors(vec![
                graphql::Error::builder()
                    .message("previously counted error")
                    .extension_code("ERROR_CODE")
                    .extension("service", subgraph_name)
                    .path(Path::from("obj/field"))
                    .apollo_id(previously_counted_error_id)
                    .build(),
                graphql::Error::builder()
                    .message("error in supergraph layer")
                    .extension_code("SUPERGRAPH_CODE")
                    .extension("service", subgraph_name)
                    .path(Path::from("obj/field"))
                    .build(),
            ])
            .build();
        let config = json!({
            "telemetry":{
                "apollo": {
                    "errors": {
                        "preview_extended_error_metrics": "enabled",
                        "subgraph": {
                            "subgraphs": {
                                "myIgnoredSubgraph": {
                                    "send": false,
                                }
                            }
                        }
                    }
                }
            }
        })
        .to_string();
        let test_harness: PluginTestHarness<Telemetry> = PluginTestHarness::builder()
            .config(&config)
            .build()
            .await
            .expect("test harness");

        let router_service = test_harness.supergraph_service(move |req| {
            let supergraph_response = example_response.clone();
            async move {
                Ok(SupergraphResponse::new_from_graphql_response(
                    supergraph_response.clone(),
                    req.context,
                ))
            }
        });

        let context = Context::new();
        context.insert_json_value(APOLLO_OPERATION_ID, operation_id.into());
        context.insert_json_value(OPERATION_NAME, operation_name.into());
        context.insert_json_value(OPERATION_KIND, operation_type.into());
        context.insert_json_value(CLIENT_NAME, client_name.into());
        context.insert_json_value(CLIENT_VERSION, client_version.into());
        let _ = context.insert(COUNTED_ERRORS, HashSet::from([previously_counted_error_id]));

        router_service
            .call(
                supergraph::Request::builder()
                    .query(query)
                    .operation_name(operation_name)
                    .header(CONTENT_TYPE, APPLICATION_JSON.essence_str())
                    .uri(Uri::from_static("/"))
                    .method(Method::POST)
                    .context(context)
                    .build()
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_counter!(
            "apollo.router.operations.error",
            1,
            &[
                KeyValue::new("apollo.operation.id", operation_id),
                KeyValue::new("graphql.operation.name", operation_name),
                KeyValue::new("graphql.operation.type", operation_type),
                KeyValue::new("apollo.client.name", client_name),
                KeyValue::new("apollo.client.version", client_version),
                KeyValue::new("graphql.error.extensions.code", "SUPERGRAPH_CODE"),
                KeyValue::new("graphql.error.extensions.severity", "ERROR"),
                KeyValue::new("graphql.error.path", "/obj/field"),
                KeyValue::new("apollo.router.error.service", "mySubgraph"),
            ]
        );
        assert_counter!("apollo.router.graphql_error", 1, code = "SUPERGRAPH_CODE");

        assert_counter_not_exists!(
            "apollo.router.operations.error",
            u64,
            "apollo.operation.id" = operation_id,
            "graphql.operation.name" = operation_name,
            "graphql.operation.type" = operation_type,
            "apollo.client.name" = client_name,
            "apollo.client.version" = client_version,
            "graphql.error.extensions.code" = "ERROR_CODE",
            "graphql.error.extensions.severity" = "ERROR",
            "graphql.error.path" = "/obj/field",
            "apollo.router.error.service" = "mySubgraph"
        );

        assert_counter_not_exists!("apollo.router.graphql_error", u64, code = "ERROR_CODE");
    }
    .with_metrics()
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn test_router_error_counting() {
    async {
        let operation_name = "operationName";
        let operation_type = "query";
        let operation_id = "opId";
        let client_name = "client";
        let client_version = "version";
        let previously_counted_error_id =
            Uuid::parse_str("cfe70a37-4651-4228-a56b-bad8444e67ad").unwrap();
        let subgraph_name = "mySubgraph";
        let config = json!({
            "telemetry":{
                "apollo": {
                    "errors": {
                        "preview_extended_error_metrics": "enabled",
                        "subgraph": {
                            "subgraphs": {
                                "myIgnoredSubgraph": {
                                    "send": false,
                                }
                            }
                        }
                    }
                }
            }
        })
        .to_string();
        let test_harness: PluginTestHarness<Telemetry> = PluginTestHarness::builder()
            .config(&config)
            .build()
            .await
            .expect("test harness");

        let router_service = test_harness.router_service(move |req| async move {
            RouterResponse::fake_builder()
                .errors(vec![
                    graphql::Error::builder()
                        .message("previously counted error")
                        .extension_code("ERROR_CODE")
                        .extension("service", subgraph_name)
                        .path(Path::from("obj/field"))
                        .apollo_id(previously_counted_error_id)
                        .build(),
                    graphql::Error::builder()
                        .message("error in supergraph layer")
                        .extension_code("SUPERGRAPH_CODE")
                        .extension("service", subgraph_name)
                        .path(Path::from("obj/field"))
                        .build(),
                ])
                .context(req.context)
                .build()
        });

        let context = Context::new();
        context.insert_json_value(APOLLO_OPERATION_ID, operation_id.into());
        context.insert_json_value(OPERATION_NAME, operation_name.into());
        context.insert_json_value(OPERATION_KIND, operation_type.into());
        context.insert_json_value(CLIENT_NAME, client_name.into());
        context.insert_json_value(CLIENT_VERSION, client_version.into());
        let _ = context.insert(COUNTED_ERRORS, HashSet::from([previously_counted_error_id]));

        router_service
            .call(
                router::Request::fake_builder()
                    .context(context)
                    .build()
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_counter!(
            "apollo.router.operations.error",
            1,
            &[
                KeyValue::new("apollo.operation.id", operation_id),
                KeyValue::new("graphql.operation.name", operation_name),
                KeyValue::new("graphql.operation.type", operation_type),
                KeyValue::new("apollo.client.name", client_name),
                KeyValue::new("apollo.client.version", client_version),
                KeyValue::new("graphql.error.extensions.code", "SUPERGRAPH_CODE"),
                KeyValue::new("graphql.error.extensions.severity", "ERROR"),
                KeyValue::new("graphql.error.path", "/obj/field"),
                KeyValue::new("apollo.router.error.service", "mySubgraph"),
            ]
        );
        assert_counter!("apollo.router.graphql_error", 1, code = "SUPERGRAPH_CODE");

        assert_counter_not_exists!(
            "apollo.router.operations.error",
            u64,
            "apollo.operation.id" = operation_id,
            "graphql.operation.name" = operation_name,
            "graphql.operation.type" = operation_type,
            "apollo.client.name" = client_name,
            "apollo.client.version" = client_version,
            "graphql.error.extensions.code" = "ERROR_CODE",
            "graphql.error.extensions.severity" = "ERROR",
            "graphql.error.path" = "/obj/field",
            "apollo.router.error.service" = "mySubgraph"
        );

        assert_counter_not_exists!("apollo.router.graphql_error", u64, code = "ERROR_CODE");
    }
    .with_metrics()
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn test_operation_errors_emitted_when_config_is_enabled() {
    async {
        let query = "query operationName { __typename }";
        let operation_name = "operationName";
        let operation_type = "query";
        let operation_id = "opId";
        let client_name = "client";
        let client_version = "version";

        let config = json!({
            "telemetry":{
                "apollo": {
                    "errors": {
                        "preview_extended_error_metrics": "enabled",
                        "subgraph": {
                            "subgraphs": {
                                "myIgnoredSubgraph": {
                                    "send": false,
                                }
                            }
                        }
                    }
                }
            }
        })
        .to_string();

        let test_harness: PluginTestHarness<Telemetry> = PluginTestHarness::builder()
            .config(&config)
            .build()
            .await
            .expect("test harness");

        let router_service =
            test_harness.supergraph_service(|req| async {
                let example_response = graphql::Response::builder()
                .data(json!({"data": null}))
                .extension(EXTENSIONS_VALUE_COMPLETION_KEY, json!([{
                        "message": "Cannot return null for non-nullable field SomeType.someField",
                        "path": Path::from("someType/someField")
                    }]))
                .errors(vec![
                    graphql::Error::builder()
                        .message("some error")
                        .extension_code("SOME_ERROR_CODE")
                        .extension("service", "mySubgraph")
                        .path(Path::from("obj/field"))
                        .build(),
                    graphql::Error::builder()
                        .message("some other error")
                        .extension_code("SOME_OTHER_ERROR_CODE")
                        .extension("service", "myOtherSubgraph")
                        .path(Path::from("obj/arr/@/firstElementField"))
                        .build(),
                    graphql::Error::builder()
                        .message("some ignored error")
                        .extension_code("SOME_IGNORED_ERROR_CODE")
                        .extension("service", "myIgnoredSubgraph")
                        .path(Path::from("obj/arr/@/firstElementField"))
                        .build(),
                ])
                .build();
                Ok(SupergraphResponse::new_from_graphql_response(
                    example_response,
                    req.context,
                ))
            });

        let context = Context::new();
        context.insert_json_value(APOLLO_OPERATION_ID, operation_id.into());
        context.insert_json_value(OPERATION_NAME, operation_name.into());
        context.insert_json_value(OPERATION_KIND, operation_type.into());
        context.insert_json_value(CLIENT_NAME, client_name.into());
        context.insert_json_value(CLIENT_VERSION, client_version.into());

        router_service
            .call(
                supergraph::Request::builder()
                    .query(query)
                    .operation_name(operation_name)
                    .header(CONTENT_TYPE, APPLICATION_JSON.essence_str())
                    .uri(Uri::from_static("/"))
                    .method(Method::POST)
                    .context(context)
                    .build()
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_counter!(
            "apollo.router.operations.error",
            1,
            &[
                KeyValue::new("apollo.operation.id", operation_id),
                KeyValue::new("graphql.operation.name", operation_name),
                KeyValue::new("graphql.operation.type", operation_type),
                KeyValue::new("apollo.client.name", client_name),
                KeyValue::new("apollo.client.version", client_version),
                KeyValue::new("graphql.error.extensions.code", "SOME_ERROR_CODE"),
                KeyValue::new("graphql.error.extensions.severity", "ERROR"),
                KeyValue::new("graphql.error.path", "/obj/field"),
                KeyValue::new("apollo.router.error.service", "mySubgraph"),
            ]
        );
        assert_counter!(
            "apollo.router.operations.error",
            1,
            &[
                KeyValue::new("apollo.operation.id", operation_id),
                KeyValue::new("graphql.operation.name", operation_name),
                KeyValue::new("graphql.operation.type", operation_type),
                KeyValue::new("apollo.client.name", client_name),
                KeyValue::new("apollo.client.version", client_version),
                KeyValue::new("graphql.error.extensions.code", "SOME_OTHER_ERROR_CODE"),
                KeyValue::new("graphql.error.extensions.severity", "ERROR"),
                KeyValue::new("graphql.error.path", "/obj/arr/@/firstElementField"),
                KeyValue::new("apollo.router.error.service", "myOtherSubgraph"),
            ]
        );
        assert_counter!(
            "apollo.router.operations.error",
            1,
            &[
                KeyValue::new("apollo.operation.id", operation_id),
                KeyValue::new("graphql.operation.name", operation_name),
                KeyValue::new("graphql.operation.type", operation_type),
                KeyValue::new("apollo.client.name", client_name),
                KeyValue::new("apollo.client.version", client_version),
                KeyValue::new(
                    "graphql.error.extensions.code",
                    "RESPONSE_VALIDATION_FAILED"
                ),
                KeyValue::new("graphql.error.extensions.severity", "WARN"),
                KeyValue::new("graphql.error.path", "/someType/someField"),
                KeyValue::new("apollo.router.error.service", ""),
            ]
        );
        assert_counter_not_exists!(
            "apollo.router.operations.error",
            u64,
            &[
                KeyValue::new("apollo.operation.id", operation_id),
                KeyValue::new("graphql.operation.name", operation_name),
                KeyValue::new("graphql.operation.type", operation_type),
                KeyValue::new("apollo.client.name", client_name),
                KeyValue::new("apollo.client.version", client_version),
                KeyValue::new("graphql.error.extensions.code", "SOME_IGNORED_ERROR_CODE"),
                KeyValue::new("graphql.error.extensions.severity", "ERROR"),
                KeyValue::new("graphql.error.path", "/obj/arr/@/firstElementField"),
                KeyValue::new("apollo.router.error.service", "myIgnoredSubgraph"),
            ]
        );
    }
    .with_metrics()
    .await;
}
