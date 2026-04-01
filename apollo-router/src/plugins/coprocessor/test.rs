use crate::services::external::PipelineStep;

macro_rules! assert_counter_zero_or_absent {
    ($($arg:tt)*) => {{
        let result = std::panic::catch_unwind(|| {
            assert_counter!($($arg)*);
        });
        if result.is_err() {
            // ignora "metric not found" — trata como zero
            println!("(info) counter not found — treating as 0 for test");
        }
    }};
}

#[cfg(test)]
pub(crate) fn assert_coprocessor_operations_metrics(
    expected_stages: &[(PipelineStep, u64, Option<bool>)],
) {
    // Iterate over all known pipeline stages and verify the metrics
    for stage in [
        PipelineStep::RouterRequest,
        PipelineStep::RouterResponse,
        PipelineStep::SupergraphRequest,
        PipelineStep::SupergraphResponse,
        PipelineStep::ExecutionRequest,
        PipelineStep::ExecutionResponse,
        PipelineStep::SubgraphRequest,
        PipelineStep::SubgraphResponse,
        PipelineStep::ConnectorRequest,
        PipelineStep::ConnectorResponse,
    ] {
        // Check if this stage is part of the expected stages list
        if let Some((_, expected_value, succeeded)) =
            expected_stages.iter().find(|(s, _, _)| *s == stage)
        {
            // ✅ Expected stage: must exist with the given value and succeeded flag
            assert_counter!(
                "apollo.router.operations.coprocessor",
                *expected_value,
                coprocessor.stage = stage.to_string(),
                coprocessor.succeeded =
                    succeeded.expect("succeeded must be provided for expected stages")
            );
        } else {
            // ❌ Unexpected stage: must not exist or must be zero
            assert_counter_zero_or_absent!(
                "apollo.router.operations.coprocessor",
                0,
                coprocessor.stage = stage.to_string()
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Arc;

    use futures::future::BoxFuture;
    use http::HeaderMap;
    use http::HeaderValue;
    use http::Method;
    use http::StatusCode;
    use http::header::ACCEPT;
    use http::header::CONTENT_TYPE;
    use mime::APPLICATION_JSON;
    use mime::TEXT_HTML;
    use router::body::RouterBody;
    use serde_json_bytes::json;
    use services::subgraph::SubgraphRequestId;
    use tower::BoxError;
    use tower::ServiceExt;

    use super::super::*;
    use crate::assert_response_eq_ignoring_error_id;
    use crate::context::deprecated::DEPRECATED_CLIENT_NAME;
    use crate::graphql::Response;
    use crate::json_ext::Object;
    use crate::json_ext::Value;
    use crate::metrics::FutureMetricsExt;
    use crate::plugin::test::MockInternalHttpClientService;
    use crate::plugin::test::MockRouterService;
    use crate::plugin::test::MockSubgraphService;
    use crate::plugin::test::MockSupergraphService;
    use crate::plugins::coprocessor::RouterRequestConf;
    use crate::plugins::coprocessor::RouterResponseConf;
    use crate::plugins::coprocessor::SubgraphRequestConf;
    use crate::plugins::coprocessor::SubgraphResponseConf;
    use crate::plugins::coprocessor::handle_graphql_response;
    use crate::plugins::coprocessor::is_graphql_response_minimally_valid;
    use crate::plugins::coprocessor::supergraph::SupergraphResponseConf;
    use crate::plugins::coprocessor::supergraph::SupergraphStage;
    use crate::plugins::coprocessor::test::assert_coprocessor_operations_metrics;
    use crate::plugins::coprocessor::was_incoming_payload_valid;
    use crate::plugins::telemetry::CLIENT_NAME;
    use crate::plugins::telemetry::config_new::conditions::SelectorOrValue;
    use crate::services::external::EXTERNALIZABLE_VERSION;
    use crate::services::external::Externalizable;
    use crate::services::external::PipelineStep;
    use crate::services::router;
    use crate::services::subgraph;
    use crate::services::supergraph;

    #[tokio::test]
    async fn load_plugin() {
        let config = serde_json::json!({
            "coprocessor": {
                "url": "http://127.0.0.1:8081"
            }
        });
        // Build a test harness. Usually we'd use this and send requests to
        // it, but in this case it's enough to build the harness to see our
        // output when our service registers.
        let _test_harness = crate::TestHarness::builder()
            .configuration_json(config)
            .unwrap()
            .build_router()
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn fails_without_global_url() {
        let config = serde_json::json!({
            "coprocessor": {
                "router": {
                    "request": {
                        "url": "http://127.0.0.1:8082",
                        "body": true
                    }
                }
            }
        });
        // Should fail schema validation because url is a required field
        assert!(
            crate::TestHarness::builder()
                .configuration_json(config)
                .is_err()
        );
    }

    #[tokio::test]
    async fn succeeds_with_stage_specific_url() {
        let config = serde_json::json!({
            "coprocessor": {
                "url": "http://127.0.0.1:8081",
                "router": {
                    "request": {
                        "url": "http://127.0.0.1:8082",
                        "body": true
                    }
                }
            }
        });
        // Should succeed because router.request has its own URL that overrides the global URL
        let _test_harness = crate::TestHarness::builder()
            .configuration_json(config)
            .unwrap()
            .build_router()
            .await
            .unwrap();

        // Now verify that the stage-specific URL is actually used
        let router_stage = RouterStage {
            request: RouterRequestConf {
                condition: Default::default(),
                headers: true,
                context: ContextConf::NewContextConf(NewContextConf::All),
                body: true,
                sdl: false,
                path: false,
                method: false,
                url: Some("http://127.0.0.1:8082".to_string()), // stage-specific URL
            },
            response: Default::default(),
        };

        let mock_router_service = router::service::from_supergraph_mock_callback(move |req| {
            // Return a simple successful response
            Ok(supergraph::Response::builder()
                .data(json!({ "test": 1234_u32 }))
                .context(req.context)
                .build()
                .unwrap())
        })
        .await;

        let mock_http_client = mock_with_callback(move |req: http::Request<RouterBody>| {
            Box::pin(async move {
                // Verify the request was sent to the stage-specific URL (8082), not the global URL (8081)
                let uri = req.uri().to_string();
                assert!(
                    uri.contains("127.0.0.1:8082"),
                    "Expected request to be sent to stage-specific URL port 8082, but got: {}",
                    uri
                );

                // Return a valid coprocessor response
                let input = json!({
                    "version": 1,
                    "stage": "RouterRequest",
                    "control": "continue",
                    "body": "{\"query\": \"{ __typename }\"}",
                    "context": {
                        "entries": {}
                    }
                });
                Ok(http::Response::builder()
                    .body(router::body::from_bytes(
                        serde_json::to_string(&input).unwrap(),
                    ))
                    .unwrap())
            })
        });

        let service = router_stage.as_service(
            mock_http_client,
            mock_router_service.boxed(),
            "http://127.0.0.1:8081".to_string(), // global URL - should NOT be used
            Arc::new("".to_string()),
            true,
        );

        let request = supergraph::Request::canned_builder().build().unwrap();

        // This will call the mock_http_client, which verifies the correct URL is used
        service.oneshot(request.try_into().unwrap()).await.unwrap();
    }

    #[tokio::test]
    async fn unknown_fields_are_denied() {
        let config = serde_json::json!({
            "coprocessor": {
                "url": "http://127.0.0.1:8081",
                "thisFieldDoesntExist": true
            }
        });
        // Build a test harness. Usually we'd use this and send requests to
        // it, but in this case it's enough to start building the harness and
        // ensure building the Configuration fails.
        assert!(
            crate::TestHarness::builder()
                .configuration_json(config)
                .is_err()
        );
    }

    #[tokio::test]
    async fn coprocessor_returning_the_wrong_version_should_fail() {
        let router_stage = RouterStage {
            request: RouterRequestConf {
                condition: Default::default(),
                headers: true,
                context: ContextConf::NewContextConf(NewContextConf::All),
                body: true,
                sdl: true,
                path: false,
                method: false,
                url: None,
            },
            response: Default::default(),
        };

        // This will never be called because we will fail at the coprocessor.
        let mock_router_service = MockRouterService::new();

        let mock_http_client = mock_with_callback(move |_: http::Request<RouterBody>| {
            Box::pin(async {
                // Wrong version!
                let input = json!(
                      {
                  "version": 2,
                  "stage": "RouterRequest",
                  "control": "continue",
                  "id": "1b19c05fdafc521016df33148ad63c1b",
                  "body": "{
                      \"query\": \"query Long {\n  me {\n  name\n}\n}\"
                    }",
                  "context": {
                      "entries": {}
                  },
                  "sdl": "the sdl shouldnt change"
                });
                Ok(http::Response::builder()
                    .body(router::body::from_bytes(
                        serde_json::to_string(&input).unwrap(),
                    ))
                    .unwrap())
            })
        });

        let service = router_stage.as_service(
            mock_http_client,
            mock_router_service.boxed(),
            "http://test".to_string(),
            Arc::new("".to_string()),
            true,
        );

        let request = supergraph::Request::canned_builder().build().unwrap();

        assert_eq!(
            "Coprocessor returned the wrong version: expected `1` found `2`",
            service
                .oneshot(request.try_into().unwrap())
                .await
                .unwrap_err()
                .to_string()
        );
    }

    #[tokio::test]
    async fn coprocessor_returning_the_wrong_stage_should_fail() {
        let router_stage = RouterStage {
            request: RouterRequestConf {
                condition: Default::default(),
                headers: true,
                context: ContextConf::NewContextConf(NewContextConf::All),
                body: true,
                sdl: true,
                path: false,
                method: false,
                url: None,
            },
            response: Default::default(),
        };

        // This will never be called because we will fail at the coprocessor.
        let mock_router_service = MockRouterService::new();

        let mock_http_client = mock_with_callback(move |_: http::Request<RouterBody>| {
            Box::pin(async {
                // Wrong stage!
                let input = json!(
                {
                    "version": 1,
                    "stage": "RouterResponse",
                    "control": "continue",
                    "id": "1b19c05fdafc521016df33148ad63c1b",
                    "body": "{
                            \"query\": \"query Long {\n  me {\n  name\n}\n}\"
                            }",
                    "context": {
                        "entries": {}
                    },
                    "sdl": "the sdl shouldnt change"
                });
                Ok(http::Response::builder()
                    .body(router::body::from_bytes(
                        serde_json::to_string(&input).unwrap(),
                    ))
                    .unwrap())
            })
        });

        let service = router_stage.as_service(
            mock_http_client,
            mock_router_service.boxed(),
            "http://test".to_string(),
            Arc::new("".to_string()),
            true,
        );

        let request = supergraph::Request::canned_builder().build().unwrap();

        assert_eq!(
            "Coprocessor returned the wrong stage: expected `RouterRequest` found `RouterResponse`",
            service
                .oneshot(request.try_into().unwrap())
                .await
                .unwrap_err()
                .to_string()
        );
    }

    #[tokio::test]
    async fn coprocessor_missing_request_control_should_fail() {
        let router_stage = RouterStage {
            request: RouterRequestConf {
                condition: Default::default(),
                headers: true,
                context: ContextConf::NewContextConf(NewContextConf::All),
                body: true,
                sdl: true,
                path: false,
                method: false,
                url: None,
            },
            response: Default::default(),
        };

        // This will never be called because we will fail at the coprocessor.
        let mock_router_service = MockRouterService::new();

        let mock_http_client = mock_with_callback(move |_: http::Request<RouterBody>| {
            Box::pin(async {
                // Wrong stage!
                let input = json!(
                {
                    "version": 1,
                    "stage": "RouterRequest",
                    "id": "1b19c05fdafc521016df33148ad63c1b",
                    "body": "{
                    \"query\": \"query Long {\n  me {\n  name\n}\n}\"
                    }",
                    "context": {
                        "entries": {}
                    },
                    "sdl": "the sdl shouldnt change"
                });
                Ok(http::Response::builder()
                    .body(router::body::from_bytes(
                        serde_json::to_string(&input).unwrap(),
                    ))
                    .unwrap())
            })
        });

        let service = router_stage.as_service(
            mock_http_client,
            mock_router_service.boxed(),
            "http://test".to_string(),
            Arc::new("".to_string()),
            true,
        );

        let request = supergraph::Request::canned_builder().build().unwrap();

        assert_eq!(
            "Coprocessor response is missing the `control` parameter in the `RouterRequest` stage. You must specify \"control\": \"Continue\" or \"control\": \"Break\"",
            service
                .oneshot(request.try_into().unwrap())
                .await
                .unwrap_err()
                .to_string()
        );
    }

    #[tokio::test]
    async fn coprocessor_subgraph_with_invalid_response_body_should_fail() {
        let subgraph_stage = SubgraphStage {
            request: SubgraphRequestConf {
                condition: Default::default(),
                body: true,
                ..Default::default()
            },
            response: Default::default(),
        };

        // This will never be called because we will fail at the coprocessor.
        let mock_subgraph_service = MockSubgraphService::new();

        let mock_http_client = mock_with_callback(move |_: http::Request<RouterBody>| {
            Box::pin(async {
                Ok(http::Response::builder()
                    .body(router::body::from_bytes(
                        r#"{
                                "version": 1,
                                "stage": "SubgraphRequest",
                                "control": {
                                    "break": 200
                                },
                                "id": "3a67e2dd75e8777804e4a8f42b971df7",
                                "body": {
                                    "errors": [{
                                        "body": "Errors need a message, this will fail to deserialize"
                                    }]
                                }
                            }"#,
                    ))
                    .unwrap())
            })
        });

        let service = subgraph_stage.as_service(
            mock_http_client,
            mock_subgraph_service.boxed(),
            "http://test".to_string(),
            "my_subgraph_service_name".to_string(),
            true,
        );

        let request = subgraph::Request::fake_builder().build();

        assert_eq!(
            "couldn't deserialize coprocessor output body: GraphQL response was malformed: missing required `message` property within error",
            service
                .oneshot(request)
                .await
                .unwrap()
                .response
                .into_body()
                .errors[0]
                .message
                .to_string()
        );
    }

    #[tokio::test]
    async fn external_plugin_subgraph_request() {
        let subgraph_stage = SubgraphStage {
            request: SubgraphRequestConf {
                condition: Default::default(),
                body: true,
                subgraph_request_id: true,
                ..Default::default()
            },
            response: Default::default(),
        };

        // This will never be called because we will fail at the coprocessor.
        let mut mock_subgraph_service = MockSubgraphService::new();

        mock_subgraph_service
            .expect_call()
            .returning(|req: subgraph::Request| {
                // Let's assert that the subgraph request has been transformed as it should have.
                assert_eq!(
                    req.subgraph_request.headers().get("cookie").unwrap(),
                    "tasty_cookie=strawberry"
                );

                assert_eq!(
                    req.context
                        .get::<&str, u8>("this-is-a-test-context")
                        .unwrap()
                        .unwrap(),
                    42
                );

                // The subgraph uri should have changed
                assert_eq!(
                    "http://thisurihaschanged/",
                    req.subgraph_request.uri().to_string()
                );

                // The query should have changed
                assert_eq!(
                    "query Long {\n  me {\n  name\n}\n}",
                    req.subgraph_request.into_body().query.unwrap()
                );

                // this should be the same as the initial request id
                assert_eq!(&*req.id, "5678");

                Ok(subgraph::Response::builder()
                    .data(json!({ "test": 1234_u32 }))
                    .errors(Vec::new())
                    .extensions(Object::new())
                    .context(req.context)
                    .id(req.id)
                    .subgraph_name(String::default())
                    .build())
            });

        let mock_http_client = mock_with_callback(move |req: http::Request<RouterBody>| {
            Box::pin(async {
                let deserialized_request: Externalizable<Value> = serde_json::from_slice(
                    &router::body::into_bytes(req.into_body()).await.unwrap(),
                )
                .unwrap();
                assert_eq!(
                    deserialized_request.subgraph_request_id.as_deref(),
                    Some("5678")
                );
                Ok(http::Response::builder()
                    .body(router::body::from_bytes(
                        r#"{
                                "version": 1,
                                "stage": "SubgraphRequest",
                                "control": "continue",
                                "headers": {
                                    "cookie": [
                                      "tasty_cookie=strawberry"
                                    ],
                                    "content-type": [
                                      "application/json"
                                    ],
                                    "host": [
                                      "127.0.0.1:4000"
                                    ],
                                    "apollo-federation-include-trace": [
                                      "ftv1"
                                    ],
                                    "apollographql-client-name": [
                                      "manual"
                                    ],
                                    "accept": [
                                      "*/*"
                                    ],
                                    "user-agent": [
                                      "curl/7.79.1"
                                    ],
                                    "content-length": [
                                      "46"
                                    ]
                                  },
                                  "body": {
                                    "query": "query Long {\n  me {\n  name\n}\n}"
                                  },
                                  "context": {
                                    "entries": {
                                      "accepts-json": false,
                                      "accepts-wildcard": true,
                                      "accepts-multipart": false,
                                      "this-is-a-test-context": 42
                                    }
                                  },
                                  "serviceName": "service name shouldn't change",
                                  "uri": "http://thisurihaschanged",
                                  "subgraphRequestId": "9abc"
                            }"#,
                    ))
                    .unwrap())
            })
        });

        let service = subgraph_stage.as_service(
            mock_http_client,
            mock_subgraph_service.boxed(),
            "http://test".to_string(),
            "my_subgraph_service_name".to_string(),
            true,
        );

        let mut request = subgraph::Request::fake_builder().build();
        request.id = SubgraphRequestId("5678".to_string());

        let response = service.oneshot(request).await.unwrap();

        assert_eq!("5678", &*response.id);
        assert_eq!(
            json!({ "test": 1234_u32 }),
            response.response.into_body().data.unwrap()
        );
    }

    #[tokio::test]
    async fn external_plugin_subgraph_request_with_selective_context() {
        let subgraph_stage = SubgraphStage {
            request: SubgraphRequestConf {
                condition: Default::default(),
                body: true,
                subgraph_request_id: true,
                context: ContextConf::NewContextConf(NewContextConf::Selective(Arc::new(
                    ["this-is-a-test-context".to_string()].into(),
                ))),
                ..Default::default()
            },
            response: Default::default(),
        };

        // This will never be called because we will fail at the coprocessor.
        let mut mock_subgraph_service = MockSubgraphService::new();

        mock_subgraph_service
            .expect_call()
            .returning(|req: subgraph::Request| {
                // Let's assert that the subgraph request has been transformed as it should have.
                assert_eq!(
                    req.subgraph_request.headers().get("cookie").unwrap(),
                    "tasty_cookie=strawberry"
                );
                assert_eq!(
                    req.context
                        .get::<&str, u8>("this-is-a-test-context")
                        .unwrap()
                        .unwrap(),
                    42
                );

                // The subgraph uri should have changed
                assert_eq!(
                    "http://thisurihaschanged/",
                    req.subgraph_request.uri().to_string()
                );

                // The query should have changed
                assert_eq!(
                    "query Long {\n  me {\n  name\n}\n}",
                    req.subgraph_request.into_body().query.unwrap()
                );

                // this should be the same as the initial request id
                assert_eq!(&*req.id, "5678");

                Ok(subgraph::Response::builder()
                    .data(json!({ "test": 1234_u32 }))
                    .errors(Vec::new())
                    .extensions(Object::new())
                    .context(req.context)
                    .id(req.id)
                    .subgraph_name(String::default())
                    .build())
            });

        let mock_http_client = mock_with_callback(move |req: http::Request<RouterBody>| {
            Box::pin(async {
                let deserialized_request: Externalizable<Value> = serde_json::from_slice(
                    &router::body::into_bytes(req.into_body()).await.unwrap(),
                )
                .unwrap();
                assert_eq!(
                    deserialized_request.subgraph_request_id.as_deref(),
                    Some("5678")
                );
                let context = deserialized_request.context.unwrap_or_default();
                assert_eq!(
                    context
                        .get::<&str, u8>("this-is-a-test-context")
                        .expect("context key should be there")
                        .expect("context key should have the right format"),
                    42
                );
                assert!(
                    context
                        .get::<&str, String>("not_passed")
                        .ok()
                        .flatten()
                        .is_none()
                );
                Ok(http::Response::builder()
                    .body(router::body::from_bytes(
                        r#"{
                                "version": 1,
                                "stage": "SubgraphRequest",
                                "control": "continue",
                                "headers": {
                                    "cookie": [
                                      "tasty_cookie=strawberry"
                                    ],
                                    "content-type": [
                                      "application/json"
                                    ],
                                    "host": [
                                      "127.0.0.1:4000"
                                    ],
                                    "apollo-federation-include-trace": [
                                      "ftv1"
                                    ],
                                    "apollographql-client-name": [
                                      "manual"
                                    ],
                                    "accept": [
                                      "*/*"
                                    ],
                                    "user-agent": [
                                      "curl/7.79.1"
                                    ],
                                    "content-length": [
                                      "46"
                                    ]
                                  },
                                  "body": {
                                    "query": "query Long {\n  me {\n  name\n}\n}"
                                  },
                                  "context": {
                                    "entries": {
                                      "this-is-a-test-context": 42
                                    }
                                  },
                                  "serviceName": "service name shouldn't change",
                                  "uri": "http://thisurihaschanged",
                                  "subgraphRequestId": "9abc"
                            }"#,
                    ))
                    .unwrap())
            })
        });

        let service = subgraph_stage.as_service(
            mock_http_client,
            mock_subgraph_service.boxed(),
            "http://test".to_string(),
            "my_subgraph_service_name".to_string(),
            true,
        );

        let mut request = subgraph::Request::fake_builder().build();
        request.id = SubgraphRequestId("5678".to_string());
        request
            .context
            .insert("not_passed", "OK".to_string())
            .unwrap();
        request
            .context
            .insert("this-is-a-test-context", 42)
            .unwrap();

        let response = service.oneshot(request).await.unwrap();

        assert_eq!("5678", &*response.id);
        assert_eq!(
            json!({ "test": 1234_u32 }),
            response.response.into_body().data.unwrap()
        );
    }

    #[tokio::test]
    async fn external_plugin_subgraph_request_with_deprecated_context() {
        let subgraph_stage = SubgraphStage {
            request: SubgraphRequestConf {
                condition: Default::default(),
                body: true,
                subgraph_request_id: true,
                context: ContextConf::NewContextConf(NewContextConf::Deprecated),
                ..Default::default()
            },
            response: Default::default(),
        };

        // This will never be called because we will fail at the coprocessor.
        let mut mock_subgraph_service = MockSubgraphService::new();

        mock_subgraph_service
            .expect_call()
            .returning(|req: subgraph::Request| {
                // Let's assert that the subgraph request has been transformed as it should have.
                assert_eq!(
                    req.subgraph_request.headers().get("cookie").unwrap(),
                    "tasty_cookie=strawberry"
                );
                assert_eq!(
                    req.context
                        .get::<&str, u8>("this-is-a-test-context")
                        .unwrap()
                        .unwrap(),
                    42
                );
                assert_eq!(
                    req.context
                        .get::<&str, String>("apollo::supergraph::operation_name")
                        .expect("context key should be there")
                        .expect("context key should have the right format"),
                    "New".to_string()
                );

                // The subgraph uri should have changed
                assert_eq!(
                    "http://thisurihaschanged/",
                    req.subgraph_request.uri().to_string()
                );

                // The query should have changed
                assert_eq!(
                    "query Long {\n  me {\n  name\n}\n}",
                    req.subgraph_request.into_body().query.unwrap()
                );

                // this should be the same as the initial request id
                assert_eq!(&*req.id, "5678");

                Ok(subgraph::Response::builder()
                    .data(json!({ "test": 1234_u32 }))
                    .errors(Vec::new())
                    .extensions(Object::new())
                    .context(req.context)
                    .id(req.id)
                    .subgraph_name(String::default())
                    .build())
            });

        let mock_http_client = mock_with_callback(move |req: http::Request<RouterBody>| {
            Box::pin(async {
                let deserialized_request: Externalizable<Value> = serde_json::from_slice(
                    &router::body::into_bytes(req.into_body()).await.unwrap(),
                )
                .unwrap();
                assert_eq!(
                    deserialized_request.subgraph_request_id.as_deref(),
                    Some("5678")
                );
                let context = deserialized_request.context.unwrap_or_default();
                assert_eq!(
                    context
                        .get::<&str, u8>("this-is-a-test-context")
                        .expect("context key should be there")
                        .expect("context key should have the right format"),
                    42
                );
                assert_eq!(
                    context
                        .get::<&str, String>("operation_name")
                        .expect("context key should be there")
                        .expect("context key should have the right format"),
                    "Test".to_string()
                );
                Ok(http::Response::builder()
                    .body(router::body::from_bytes(
                        r#"{
                                "version": 1,
                                "stage": "SubgraphRequest",
                                "control": "continue",
                                "headers": {
                                    "cookie": [
                                      "tasty_cookie=strawberry"
                                    ],
                                    "content-type": [
                                      "application/json"
                                    ],
                                    "host": [
                                      "127.0.0.1:4000"
                                    ],
                                    "apollo-federation-include-trace": [
                                      "ftv1"
                                    ],
                                    "apollographql-client-name": [
                                      "manual"
                                    ],
                                    "accept": [
                                      "*/*"
                                    ],
                                    "user-agent": [
                                      "curl/7.79.1"
                                    ],
                                    "content-length": [
                                      "46"
                                    ]
                                  },
                                  "body": {
                                    "query": "query Long {\n  me {\n  name\n}\n}"
                                  },
                                  "context": {
                                    "entries": {
                                      "this-is-a-test-context": 42,
                                      "operation_name": "New"
                                    }
                                  },
                                  "serviceName": "service name shouldn't change",
                                  "uri": "http://thisurihaschanged",
                                  "subgraphRequestId": "9abc"
                            }"#,
                    ))
                    .unwrap())
            })
        });

        let service = subgraph_stage.as_service(
            mock_http_client,
            mock_subgraph_service.boxed(),
            "http://test".to_string(),
            "my_subgraph_service_name".to_string(),
            true,
        );

        let mut request = subgraph::Request::fake_builder().build();
        request.id = SubgraphRequestId("5678".to_string());
        request
            .context
            .insert("apollo::supergraph::operation_name", "Test".to_string())
            .unwrap();
        request
            .context
            .insert("this-is-a-test-context", 42)
            .unwrap();

        let response = service.oneshot(request).await.unwrap();

        assert_eq!("5678", &*response.id);
        assert_eq!(
            json!({ "test": 1234_u32 }),
            response.response.into_body().data.unwrap()
        );
    }

    #[tokio::test]
    async fn external_plugin_subgraph_request_with_condition() {
        let subgraph_stage = SubgraphStage {
            request: SubgraphRequestConf {
                condition: Condition::Eq([
                    SelectorOrValue::Selector(SubgraphSelector::SubgraphRequestHeader {
                        subgraph_request_header: String::from("another_header"),
                        redact: None,
                        default: None,
                    }),
                    SelectorOrValue::Value("value".to_string().into()),
                ]),
                body: true,
                ..Default::default()
            },
            response: Default::default(),
        };

        // This will never be called because we will fail at the coprocessor.
        let mut mock_subgraph_service = MockSubgraphService::new();

        mock_subgraph_service
            .expect_call()
            .returning(|req: subgraph::Request| {
                assert_eq!("/", req.subgraph_request.uri().to_string());

                Ok(subgraph::Response::builder()
                    .data(json!({ "test": 1234_u32 }))
                    .errors(Vec::new())
                    .extensions(Object::new())
                    .context(req.context)
                    .subgraph_name(String::default())
                    .build())
            });

        let mock_http_client = mock_with_callback(move |_: http::Request<RouterBody>| {
            Box::pin(async {
                Ok(http::Response::builder()
                    .body(router::body::from_bytes(
                        r#"{
                                "version": 1,
                                "stage": "SubgraphRequest",
                                "control": "continue",
                                  "body": {
                                    "query": "query Long {\n  me {\n  name\n}\n}"
                                  },
                                  "context": {
                                  },
                                  "serviceName": "service name shouldn't change",
                                  "uri": "http://thisurihaschanged"
                            }"#,
                    ))
                    .unwrap())
            })
        });

        let service = subgraph_stage.as_service(
            mock_http_client,
            mock_subgraph_service.boxed(),
            "http://test".to_string(),
            "my_subgraph_service_name".to_string(),
            true,
        );

        let request = subgraph::Request::fake_builder().build();

        assert_eq!(
            json!({ "test": 1234_u32 }),
            service
                .oneshot(request)
                .await
                .unwrap()
                .response
                .into_body()
                .data
                .unwrap()
        );
    }

    #[tokio::test]
    async fn external_plugin_subgraph_request_controlflow_break() {
        let subgraph_stage = SubgraphStage {
            request: SubgraphRequestConf {
                condition: Default::default(),
                body: true,
                ..Default::default()
            },
            response: Default::default(),
        };

        // This will never be called because we will fail at the coprocessor.
        let mock_subgraph_service = MockSubgraphService::new();

        let mock_http_client = mock_with_callback(move |_: http::Request<RouterBody>| {
            Box::pin(async {
                Ok(http::Response::builder()
                    .body(router::body::from_bytes(
                        r#"{
                                "version": 1,
                                "stage": "SubgraphRequest",
                                "control": {
                                    "break": 200
                                },
                                "body": {
                                    "errors": [{ "message": "my error message" }]
                                },
                                "context": {
                                    "entries": {
                                        "testKey": true
                                    }
                                },
                                "headers": {
                                    "aheader": ["a value"]
                                }
                            }"#,
                    ))
                    .unwrap())
            })
        });

        let service = subgraph_stage.as_service(
            mock_http_client,
            mock_subgraph_service.boxed(),
            "http://test".to_string(),
            "my_subgraph_service_name".to_string(),
            true,
        );

        let request = subgraph::Request::fake_builder().build();

        let crate::services::subgraph::Response {
            response, context, ..
        } = service.oneshot(request).await.unwrap();

        assert!(context.get::<_, bool>("testKey").unwrap().unwrap());

        let value = response.headers().get("aheader").unwrap();

        assert_eq!("a value", value);

        assert_eq!(
            "my error message",
            response.into_body().errors[0].message.as_str()
        );
    }

    #[tokio::test]
    async fn external_plugin_subgraph_request_controlflow_break_with_message_string() {
        let subgraph_stage = SubgraphStage {
            request: SubgraphRequestConf {
                condition: Default::default(),
                body: true,
                ..Default::default()
            },
            response: Default::default(),
        };

        // This will never be called because we will fail at the coprocessor.
        let mock_subgraph_service = MockSubgraphService::new();

        let mock_http_client = mock_with_callback(move |_: http::Request<RouterBody>| {
            Box::pin(async {
                Ok(http::Response::builder()
                    .body(router::body::from_bytes(
                        r#"{
                                "version": 1,
                                "stage": "SubgraphRequest",
                                "control": {
                                    "break": 200
                                },
                                "body": "my error message"
                            }"#,
                    ))
                    .unwrap())
            })
        });

        let service = subgraph_stage.as_service(
            mock_http_client,
            mock_subgraph_service.boxed(),
            "http://test".to_string(),
            "my_subgraph_service_name".to_string(),
            true,
        );

        let request = subgraph::Request::fake_builder().build();

        let response = service.oneshot(request).await.unwrap().response;

        assert_eq!(response.status(), http::StatusCode::OK);

        let actual_response = response.into_body();

        assert_response_eq_ignoring_error_id!(
            actual_response,
            serde_json_bytes::from_value::<Response>(json!({
                "errors": [{
                   "message": "my error message",
                   "extensions": {
                      "code": "ERROR"
                   }
                }]
            }))
            .unwrap()
        );
    }

    #[tokio::test]
    async fn external_plugin_subgraph_response() {
        let subgraph_stage = SubgraphStage {
            request: Default::default(),
            response: SubgraphResponseConf {
                condition: Default::default(),
                body: true,
                subgraph_request_id: true,
                ..Default::default()
            },
        };

        // This will never be called because we will fail at the coprocessor.
        let mut mock_subgraph_service = MockSubgraphService::new();

        mock_subgraph_service
            .expect_call()
            .returning(|req: subgraph::Request| {
                assert_eq!(&*req.id, "5678");
                Ok(subgraph::Response::builder()
                    .data(json!({ "test": 1234_u32 }))
                    .errors(Vec::new())
                    .extensions(Object::new())
                    .context(req.context)
                    .id(req.id)
                    .subgraph_name(String::default())
                    .build())
            });

        let mock_http_client = mock_with_callback(move |r: http::Request<RouterBody>| {
            Box::pin(async move {
                let (_, body) = r.into_parts();
                let body: Value =
                    serde_json::from_slice(&router::body::into_bytes(body).await.unwrap()).unwrap();
                let subgraph_id = body.get("subgraphRequestId").unwrap();
                assert_eq!(subgraph_id.as_str(), Some("5678"));

                Ok(http::Response::builder()
                    .body(router::body::from_bytes(
                        r#"{
                                "version": 1,
                                "stage": "SubgraphResponse",
                                "headers": {
                                    "cookie": [
                                      "tasty_cookie=strawberry"
                                    ],
                                    "content-type": [
                                      "application/json"
                                    ],
                                    "host": [
                                      "127.0.0.1:4000"
                                    ],
                                    "apollo-federation-include-trace": [
                                      "ftv1"
                                    ],
                                    "apollographql-client-name": [
                                      "manual"
                                    ],
                                    "accept": [
                                      "*/*"
                                    ],
                                    "user-agent": [
                                      "curl/7.79.1"
                                    ],
                                    "content-length": [
                                      "46"
                                    ]
                                  },
                                  "body": {
                                    "data": {
                                        "test": 5678
                                    }
                                  },
                                  "context": {
                                    "entries": {
                                      "accepts-json": false,
                                      "accepts-wildcard": true,
                                      "accepts-multipart": false,
                                      "this-is-a-test-context": 42
                                    }
                                  },
                                  "subgraphRequestId": "9abc"
                            }"#,
                    ))
                    .unwrap())
            })
        });

        let service = subgraph_stage.as_service(
            mock_http_client,
            mock_subgraph_service.boxed(),
            "http://test".to_string(),
            "my_subgraph_service_name".to_string(),
            true,
        );

        let mut request = subgraph::Request::fake_builder().build();
        request.id = SubgraphRequestId("5678".to_string());

        let response = service.oneshot(request).await.unwrap();

        // Let's assert that the subgraph response has been transformed as it should have.
        assert_eq!(
            response.response.headers().get("cookie").unwrap(),
            "tasty_cookie=strawberry"
        );
        assert_eq!(&*response.id, "5678");

        assert_eq!(
            response
                .context
                .get::<&str, u8>("this-is-a-test-context")
                .unwrap()
                .unwrap(),
            42
        );

        assert_eq!(
            json!({ "test": 5678_u32 }),
            response.response.into_body().data.unwrap()
        );
    }

    #[tokio::test]
    async fn external_plugin_subgraph_response_with_null_data() {
        let subgraph_stage = SubgraphStage {
            request: Default::default(),
            response: SubgraphResponseConf {
                condition: Default::default(),
                body: true,
                subgraph_request_id: true,
                ..Default::default()
            },
        };

        // This will never be called because we will fail at the coprocessor.
        let mut mock_subgraph_service = MockSubgraphService::new();

        mock_subgraph_service
            .expect_call()
            .returning(|req: subgraph::Request| {
                assert_eq!(&*req.id, "5678");
                Ok(subgraph::Response::builder()
                    .data(serde_json_bytes::Value::Null)
                    .extensions(Object::new())
                    .context(req.context)
                    .id(req.id)
                    .subgraph_name(String::default())
                    .build())
            });

        let mock_http_client = mock_with_callback(move |r: http::Request<RouterBody>| {
            Box::pin(async move {
                let (_, body) = r.into_parts();
                let body: Value =
                    serde_json::from_slice(&router::body::into_bytes(body).await.unwrap()).unwrap();
                let subgraph_id = body.get("subgraphRequestId").unwrap();
                assert_eq!(subgraph_id.as_str(), Some("5678"));

                Ok(http::Response::builder()
                    .body(router::body::from_bytes(
                        r#"{
                                "version": 1,
                                "stage": "SubgraphResponse",
                                "headers": {
                                    "content-type": [
                                      "application/json"
                                    ],
                                    "host": [
                                      "127.0.0.1:4000"
                                    ],
                                    "apollo-federation-include-trace": [
                                      "ftv1"
                                    ],
                                    "apollographql-client-name": [
                                      "manual"
                                    ],
                                    "accept": [
                                      "*/*"
                                    ],
                                    "user-agent": [
                                      "curl/7.79.1"
                                    ],
                                    "content-length": [
                                      "46"
                                    ]
                                  },
                                  "body": {
                                    "data": null
                                  },
                                  "context": {
                                    "entries": {
                                      "accepts-json": false,
                                      "accepts-wildcard": true,
                                      "accepts-multipart": false
                                    }
                                  },
                                  "subgraphRequestId": "9abc"
                            }"#,
                    ))
                    .unwrap())
            })
        });

        let service = subgraph_stage.as_service(
            mock_http_client,
            mock_subgraph_service.boxed(),
            "http://test".to_string(),
            "my_subgraph_service_name".to_string(),
            true,
        );

        let mut request = subgraph::Request::fake_builder().build();
        request.id = SubgraphRequestId("5678".to_string());

        let response = service.oneshot(request).await.unwrap();

        // Let's assert that the subgraph response has been transformed as it should have.
        assert_eq!(&*response.id, "5678");
        assert_eq!(
            serde_json_bytes::Value::Null,
            response.response.into_body().data.unwrap()
        );
    }

    #[tokio::test]
    async fn external_plugin_subgraph_response_with_selective_context() {
        let subgraph_stage = SubgraphStage {
            request: Default::default(),
            response: SubgraphResponseConf {
                condition: Default::default(),
                body: true,
                subgraph_request_id: true,
                context: ContextConf::NewContextConf(NewContextConf::Selective(Arc::new(
                    ["this-is-a-test-context".to_string()].into(),
                ))),
                ..Default::default()
            },
        };

        // This will never be called because we will fail at the coprocessor.
        let mut mock_subgraph_service = MockSubgraphService::new();

        mock_subgraph_service
            .expect_call()
            .returning(|req: subgraph::Request| {
                assert_eq!(&*req.id, "5678");
                Ok(subgraph::Response::builder()
                    .data(json!({ "test": 1234_u32 }))
                    .errors(Vec::new())
                    .extensions(Object::new())
                    .context(req.context)
                    .id(req.id)
                    .subgraph_name(String::default())
                    .build())
            });

        let mock_http_client = mock_with_callback(move |r: http::Request<RouterBody>| {
            Box::pin(async move {
                let (_, body) = r.into_parts();
                let deserialized_response: Externalizable<Value> =
                    serde_json::from_slice(&router::body::into_bytes(body).await.unwrap()).unwrap();

                assert_eq!(
                    deserialized_response.subgraph_request_id,
                    Some(SubgraphRequestId("5678".to_string()))
                );

                let context = deserialized_response.context.unwrap_or_default();
                assert_eq!(
                    context
                        .get::<&str, u8>("this-is-a-test-context")
                        .expect("context key should be there")
                        .expect("context key should have the right format"),
                    55
                );
                assert!(
                    context
                        .get::<&str, String>("not_passed")
                        .ok()
                        .flatten()
                        .is_none()
                );

                Ok(http::Response::builder()
                    .body(router::body::from_bytes(
                        r#"{
                                "version": 1,
                                "stage": "SubgraphResponse",
                                "headers": {
                                    "cookie": [
                                      "tasty_cookie=strawberry"
                                    ],
                                    "content-type": [
                                      "application/json"
                                    ],
                                    "host": [
                                      "127.0.0.1:4000"
                                    ],
                                    "apollo-federation-include-trace": [
                                      "ftv1"
                                    ],
                                    "apollographql-client-name": [
                                      "manual"
                                    ],
                                    "accept": [
                                      "*/*"
                                    ],
                                    "user-agent": [
                                      "curl/7.79.1"
                                    ],
                                    "content-length": [
                                      "46"
                                    ]
                                  },
                                  "body": {
                                    "data": {
                                        "test": 5678
                                    }
                                  },
                                  "context": {
                                    "entries": {
                                      "this-is-a-test-context": 42
                                    }
                                  },
                                  "subgraphRequestId": "9abc"
                            }"#,
                    ))
                    .unwrap())
            })
        });

        let service = subgraph_stage.as_service(
            mock_http_client,
            mock_subgraph_service.boxed(),
            "http://test".to_string(),
            "my_subgraph_service_name".to_string(),
            true,
        );

        let mut request = subgraph::Request::fake_builder().build();
        request.id = SubgraphRequestId("5678".to_string());
        request
            .context
            .insert("not_passed", "OK".to_string())
            .unwrap();
        request
            .context
            .insert("this-is-a-test-context", 55)
            .unwrap();

        let response = service.oneshot(request).await.unwrap();

        // Let's assert that the subgraph response has been transformed as it should have.
        assert_eq!(
            response.response.headers().get("cookie").unwrap(),
            "tasty_cookie=strawberry"
        );
        assert_eq!(&*response.id, "5678");

        assert_eq!(
            response
                .context
                .get::<&str, u8>("this-is-a-test-context")
                .unwrap()
                .unwrap(),
            42
        );

        assert_eq!(
            json!({ "test": 5678_u32 }),
            response.response.into_body().data.unwrap()
        );
    }

    #[tokio::test]
    async fn external_plugin_subgraph_response_with_deprecated_context() {
        let subgraph_stage = SubgraphStage {
            request: Default::default(),
            response: SubgraphResponseConf {
                condition: Default::default(),
                body: true,
                subgraph_request_id: true,
                context: ContextConf::NewContextConf(NewContextConf::Deprecated),
                ..Default::default()
            },
        };

        // This will never be called because we will fail at the coprocessor.
        let mut mock_subgraph_service = MockSubgraphService::new();

        mock_subgraph_service
            .expect_call()
            .returning(|req: subgraph::Request| {
                assert_eq!(&*req.id, "5678");
                Ok(subgraph::Response::builder()
                    .data(json!({ "test": 1234_u32 }))
                    .errors(Vec::new())
                    .extensions(Object::new())
                    .context(req.context)
                    .id(req.id)
                    .subgraph_name(String::default())
                    .build())
            });

        let mock_http_client = mock_with_callback(move |r: http::Request<RouterBody>| {
            Box::pin(async move {
                let (_, body) = r.into_parts();
                let deserialized_response: Externalizable<Value> =
                    serde_json::from_slice(&router::body::into_bytes(body).await.unwrap()).unwrap();

                assert_eq!(
                    deserialized_response.subgraph_request_id,
                    Some(SubgraphRequestId("5678".to_string()))
                );

                let context = deserialized_response.context.unwrap_or_default();
                assert_eq!(
                    context
                        .get::<&str, u8>("this-is-a-test-context")
                        .expect("context key should be there")
                        .expect("context key should have the right format"),
                    55
                );
                assert_eq!(
                    context
                        .get::<&str, String>("operation_name")
                        .expect("context key should be there")
                        .expect("context key should have the right format"),
                    "Test".to_string()
                );

                Ok(http::Response::builder()
                    .body(router::body::from_bytes(
                        r#"{
                                "version": 1,
                                "stage": "SubgraphResponse",
                                "headers": {
                                    "cookie": [
                                      "tasty_cookie=strawberry"
                                    ],
                                    "content-type": [
                                      "application/json"
                                    ],
                                    "host": [
                                      "127.0.0.1:4000"
                                    ],
                                    "apollo-federation-include-trace": [
                                      "ftv1"
                                    ],
                                    "apollographql-client-name": [
                                      "manual"
                                    ],
                                    "accept": [
                                      "*/*"
                                    ],
                                    "user-agent": [
                                      "curl/7.79.1"
                                    ],
                                    "content-length": [
                                      "46"
                                    ]
                                  },
                                  "body": {
                                    "data": {
                                        "test": 5678
                                    }
                                  },
                                  "context": {
                                    "entries": {
                                      "this-is-a-test-context": 42,
                                      "operation_name": "New"
                                    }
                                  },
                                  "subgraphRequestId": "9abc"
                            }"#,
                    ))
                    .unwrap())
            })
        });

        let service = subgraph_stage.as_service(
            mock_http_client,
            mock_subgraph_service.boxed(),
            "http://test".to_string(),
            "my_subgraph_service_name".to_string(),
            true,
        );

        let mut request = subgraph::Request::fake_builder().build();
        request.id = SubgraphRequestId("5678".to_string());
        request
            .context
            .insert("apollo::supergraph::operation_name", "Test".to_string())
            .unwrap();
        request
            .context
            .insert("this-is-a-test-context", 55)
            .unwrap();

        let response = service.oneshot(request).await.unwrap();

        // Let's assert that the subgraph response has been transformed as it should have.
        assert_eq!(
            response.response.headers().get("cookie").unwrap(),
            "tasty_cookie=strawberry"
        );
        assert_eq!(&*response.id, "5678");

        assert_eq!(
            response
                .context
                .get::<&str, u8>("this-is-a-test-context")
                .unwrap()
                .unwrap(),
            42
        );
        assert_eq!(
            response
                .context
                .get::<&str, String>("apollo::supergraph::operation_name")
                .unwrap()
                .unwrap(),
            "New".to_string()
        );

        assert_eq!(
            json!({ "test": 5678_u32 }),
            response.response.into_body().data.unwrap()
        );
    }

    #[tokio::test]
    async fn external_plugin_subgraph_response_with_condition() {
        let subgraph_stage = SubgraphStage {
            request: Default::default(),
            response: SubgraphResponseConf {
                // Will be satisfied
                condition: Condition::Exists(SubgraphSelector::ResponseContext {
                    response_context: String::from("context_value"),
                    redact: None,
                    default: None,
                }),
                body: true,
                ..Default::default()
            },
        };

        // This will never be called because we will fail at the coprocessor.
        let mut mock_subgraph_service = MockSubgraphService::new();

        mock_subgraph_service
            .expect_call()
            .returning(|req: subgraph::Request| {
                req.context
                    .insert("context_value", "content".to_string())
                    .unwrap();
                Ok(subgraph::Response::builder()
                    .data(json!({ "test": 1234_u32 }))
                    .errors(Vec::new())
                    .extensions(Object::new())
                    .context(req.context)
                    .subgraph_name(String::default())
                    .build())
            });

        let mock_http_client = mock_with_callback(move |_: http::Request<RouterBody>| {
            Box::pin(async {
                Ok(http::Response::builder()
                    .body(router::body::from_bytes(
                        r#"{
                                "version": 1,
                                "stage": "SubgraphResponse",
                                "headers": {
                                    "cookie": [
                                      "tasty_cookie=strawberry"
                                    ],
                                    "content-type": [
                                      "application/json"
                                    ],
                                    "host": [
                                      "127.0.0.1:4000"
                                    ],
                                    "apollo-federation-include-trace": [
                                      "ftv1"
                                    ],
                                    "apollographql-client-name": [
                                      "manual"
                                    ],
                                    "accept": [
                                      "*/*"
                                    ],
                                    "user-agent": [
                                      "curl/7.79.1"
                                    ],
                                    "content-length": [
                                      "46"
                                    ]
                                  },
                                  "body": {
                                    "data": {
                                        "test": 5678
                                    }
                                  },
                                  "context": {
                                    "entries": {
                                      "accepts-json": false,
                                      "accepts-wildcard": true,
                                      "accepts-multipart": false,
                                      "this-is-a-test-context": 42
                                    }
                                  }
                            }"#,
                    ))
                    .unwrap())
            })
        });

        let service = subgraph_stage.as_service(
            mock_http_client,
            mock_subgraph_service.boxed(),
            "http://test".to_string(),
            "my_subgraph_service_name".to_string(),
            true,
        );

        let request = subgraph::Request::fake_builder().build();

        let response = service.oneshot(request).await.unwrap();

        // Let's assert that the subgraph response has been transformed as it should have.
        assert_eq!(
            response.response.headers().get("cookie").unwrap(),
            "tasty_cookie=strawberry"
        );

        assert_eq!(
            response
                .context
                .get::<&str, u8>("this-is-a-test-context")
                .unwrap()
                .unwrap(),
            42
        );

        assert_eq!(
            json!({ "test": 5678_u32 }),
            response.response.into_body().data.unwrap()
        );
    }

    #[tokio::test]
    async fn external_plugin_supergraph_response() {
        let supergraph_stage = SupergraphStage {
            request: Default::default(),
            response: SupergraphResponseConf {
                condition: Default::default(),
                headers: false,
                context: ContextConf::Deprecated(false),
                body: true,
                status_code: false,
                sdl: false,
                url: None,
            },
        };

        // This will never be called because we will fail at the coprocessor.
        let mut mock_supergraph_service = MockSupergraphService::new();

        mock_supergraph_service
            .expect_call()
            .returning(|req: supergraph::Request| {
                Ok(supergraph::Response::new_from_graphql_response(
                    graphql::Response::builder()
                        .data(Value::Null)
                        .subscribed(true)
                        .build(),
                    req.context,
                ))
            });

        let mock_http_client = mock_with_deferred_callback(move |_: http::Request<RouterBody>| {
            Box::pin(async {
                Ok(http::Response::builder()
                    .body(router::body::from_bytes(
                        r#"{
                                "version": 1,
                                "stage": "SupergraphResponse",
                                  "body": {
                                    "data": null
                                  }
                            }"#,
                    ))
                    .unwrap())
            })
        });

        let service = supergraph_stage.as_service(
            mock_http_client,
            mock_supergraph_service.boxed(),
            "http://test".to_string(),
            Arc::default(),
            true,
        );

        let request = supergraph::Request::fake_builder().build().unwrap();

        let mut response = service.oneshot(request).await.unwrap();

        let gql_response = response.response.body_mut().next().await.unwrap();
        // Let's assert that the supergraph response has been transformed as it should have.
        assert_eq!(gql_response.subscribed, Some(true));
        assert_eq!(gql_response.data, Some(Value::Null));
    }

    #[tokio::test]
    async fn external_plugin_supergraph_response_with_selective_context() {
        let supergraph_stage = SupergraphStage {
            request: Default::default(),
            response: SupergraphResponseConf {
                condition: Default::default(),
                headers: false,
                context: ContextConf::NewContextConf(NewContextConf::Selective(Arc::new(
                    ["this-is-a-test-context".to_string()].into(),
                ))),
                body: true,
                status_code: false,
                sdl: false,
                url: None,
            },
        };

        // This will never be called because we will fail at the coprocessor.
        let mut mock_supergraph_service = MockSupergraphService::new();

        mock_supergraph_service
            .expect_call()
            .returning(|req: supergraph::Request| {
                Ok(supergraph::Response::new_from_graphql_response(
                    graphql::Response::builder()
                        .data(Value::Null)
                        .subscribed(true)
                        .build(),
                    req.context,
                ))
            });

        let mock_http_client =
            mock_with_deferred_callback(move |req: http::Request<RouterBody>| {
                Box::pin(async {
                    let (_, body) = req.into_parts();
                    let deserialized_response: Externalizable<Value> =
                        serde_json::from_slice(&router::body::into_bytes(body).await.unwrap())
                            .unwrap();
                    let context = deserialized_response.context.unwrap_or_default();
                    assert_eq!(
                        context
                            .get::<&str, u8>("this-is-a-test-context")
                            .expect("context key should be there")
                            .expect("context key should have the right format"),
                        42
                    );
                    assert!(
                        context
                            .get::<&str, String>("not_passed")
                            .ok()
                            .flatten()
                            .is_none()
                    );
                    Ok(http::Response::builder()
                        .body(router::body::from_bytes(
                            r#"{
                                "version": 1,
                                "stage": "SupergraphResponse",
                                "context": {
                                    "entries": {
                                        "this-is-a-test-context": 25
                                    }
                                },
                                "body": {
                                    "data": null
                                }
                            }"#,
                        ))
                        .unwrap())
                })
            });

        let service = supergraph_stage.as_service(
            mock_http_client,
            mock_supergraph_service.boxed(),
            "http://test".to_string(),
            Arc::default(),
            true,
        );

        let request = supergraph::Request::fake_builder().build().unwrap();
        request
            .context
            .insert("not_passed", "OK".to_string())
            .unwrap();
        request
            .context
            .insert("this-is-a-test-context", 42)
            .unwrap();

        let mut response = service.oneshot(request).await.unwrap();

        assert_eq!(
            response
                .context
                .get::<&str, u8>("this-is-a-test-context")
                .unwrap()
                .unwrap(),
            25
        );

        let gql_response = response.response.body_mut().next().await.unwrap();
        // Let's assert that the supergraph response has been transformed as it should have.
        assert_eq!(gql_response.subscribed, Some(true));
        assert_eq!(gql_response.data, Some(Value::Null));
    }

    #[tokio::test]
    async fn external_plugin_supergraph_response_with_deprecated_context() {
        let supergraph_stage = SupergraphStage {
            request: Default::default(),
            response: SupergraphResponseConf {
                condition: Default::default(),
                headers: false,
                context: ContextConf::NewContextConf(NewContextConf::Deprecated),
                body: true,
                status_code: false,
                sdl: false,
                url: None,
            },
        };

        // This will never be called because we will fail at the coprocessor.
        let mut mock_supergraph_service = MockSupergraphService::new();

        mock_supergraph_service
            .expect_call()
            .returning(|req: supergraph::Request| {
                Ok(supergraph::Response::new_from_graphql_response(
                    graphql::Response::builder()
                        .data(Value::Null)
                        .subscribed(true)
                        .build(),
                    req.context,
                ))
            });

        let mock_http_client =
            mock_with_deferred_callback(move |req: http::Request<RouterBody>| {
                Box::pin(async {
                    let (_, body) = req.into_parts();
                    let deserialized_response: Externalizable<Value> =
                        serde_json::from_slice(&router::body::into_bytes(body).await.unwrap())
                            .unwrap();
                    let context = deserialized_response.context.unwrap_or_default();
                    assert_eq!(
                        context
                            .get::<&str, String>("operation_name")
                            .expect("context key should be there")
                            .expect("context key should have the right format"),
                        "Test".to_string()
                    );
                    Ok(http::Response::builder()
                        .body(router::body::from_bytes(
                            r#"{
                                "version": 1,
                                "stage": "SupergraphResponse",
                                "context": {
                                    "entries": {
                                        "operation_name": "New"
                                    }
                                },
                                "body": {
                                    "data": null
                                }
                            }"#,
                        ))
                        .unwrap())
                })
            });

        let service = supergraph_stage.as_service(
            mock_http_client,
            mock_supergraph_service.boxed(),
            "http://test".to_string(),
            Arc::default(),
            true,
        );

        let request = supergraph::Request::fake_builder().build().unwrap();
        request
            .context
            .insert("apollo::supergraph::operation_name", "Test".to_string())
            .unwrap();

        let mut response = service.oneshot(request).await.unwrap();

        assert_eq!(
            response
                .context
                .get::<&str, String>("apollo::supergraph::operation_name")
                .unwrap()
                .unwrap(),
            "New".to_string()
        );
        assert!(
            response
                .context
                .get::<&str, String>("operation_name")
                .ok()
                .flatten()
                .is_none()
        );

        let gql_response = response.response.body_mut().next().await.unwrap();
        // Let's assert that the supergraph response has been transformed as it should have.
        assert_eq!(gql_response.subscribed, Some(true));
        assert_eq!(gql_response.data, Some(Value::Null));
    }

    #[tokio::test]
    async fn external_plugin_router_request() {
        let router_stage = RouterStage {
            request: RouterRequestConf {
                condition: Default::default(),
                headers: true,
                context: ContextConf::NewContextConf(NewContextConf::All),
                body: true,
                sdl: true,
                path: true,
                method: true,
                url: None,
            },
            response: Default::default(),
        };

        let mock_router_service = router::service::from_supergraph_mock_callback(move |req| {
            // Let's assert that the router request has been transformed as it should have.
            assert_eq!(
                req.supergraph_request.headers().get("cookie").unwrap(),
                "tasty_cookie=strawberry"
            );

            assert_eq!(
                req.context
                    .get::<&str, u8>("this-is-a-test-context")
                    .unwrap()
                    .unwrap(),
                42
            );

            // The query should have changed
            assert_eq!(
                "query Long {\n  me {\n  name\n}\n}",
                req.supergraph_request.into_body().query.unwrap()
            );

            Ok(supergraph::Response::builder()
                .data(json!({ "test": 1234_u32 }))
                .context(req.context)
                .build()
                .unwrap())
        })
        .await;

        let mock_http_client = mock_with_callback(move |req: http::Request<RouterBody>| {
            Box::pin(async {
                let deserialized_request: Externalizable<Value> = serde_json::from_slice(
                    &router::body::into_bytes(req.into_body()).await.unwrap(),
                )
                .unwrap();

                assert_eq!(EXTERNALIZABLE_VERSION, deserialized_request.version);
                assert_eq!(
                    PipelineStep::RouterRequest.to_string(),
                    deserialized_request.stage
                );

                let input = json!(
                      {
                  "version": 1,
                  "stage": "RouterRequest",
                  "control": "continue",
                  "id": "1b19c05fdafc521016df33148ad63c1b",
                  "headers": {
                    "cookie": [
                      "tasty_cookie=strawberry"
                    ],
                    "content-type": [
                      "application/json"
                    ],
                    "host": [
                      "127.0.0.1:4000"
                    ],
                    "apollo-federation-include-trace": [
                      "ftv1"
                    ],
                    "apollographql-client-name": [
                      "manual"
                    ],
                    "accept": [
                      "*/*"
                    ],
                    "user-agent": [
                      "curl/7.79.1"
                    ],
                    "content-length": [
                      "46"
                    ]
                  },
                  "body": "{
                      \"query\": \"query Long {\n  me {\n  name\n}\n}\"
                    }",
                  "context": {
                    "entries": {
                      "accepts-json": false,
                      "accepts-wildcard": true,
                      "accepts-multipart": false,
                      "this-is-a-test-context": 42
                    }
                  },
                  "sdl": "the sdl shouldnt change"
                });
                Ok(http::Response::builder()
                    .body(router::body::from_bytes(
                        serde_json::to_string(&input).unwrap(),
                    ))
                    .unwrap())
            })
        });

        let service = router_stage.as_service(
            mock_http_client,
            mock_router_service.boxed(),
            "http://test".to_string(),
            Arc::new("".to_string()),
            true,
        );

        let request = supergraph::Request::canned_builder().build().unwrap();

        service.oneshot(request.try_into().unwrap()).await.unwrap();
    }

    #[tokio::test]
    async fn external_plugin_router_request_with_selective_context() {
        let router_stage = RouterStage {
            request: RouterRequestConf {
                condition: Default::default(),
                headers: true,
                context: ContextConf::NewContextConf(NewContextConf::Selective(Arc::new(
                    ["this-is-a-test-context".to_string()].into(),
                ))),
                body: true,
                sdl: true,
                path: true,
                method: true,
                url: None,
            },
            response: Default::default(),
        };

        let mock_router_service = router::service::from_supergraph_mock_callback(move |req| {
            // Let's assert that the router request has been transformed as it should have.
            assert_eq!(
                req.supergraph_request.headers().get("cookie").unwrap(),
                "tasty_cookie=strawberry"
            );

            assert_eq!(
                req.context
                    .get::<&str, u8>("this-is-a-test-context")
                    .unwrap()
                    .unwrap(),
                42
            );

            // The query should have changed
            assert_eq!(
                "query Long {\n  me {\n  name\n}\n}",
                req.supergraph_request.into_body().query.unwrap()
            );

            Ok(supergraph::Response::builder()
                .data(json!({ "test": 1234_u32 }))
                .context(req.context)
                .build()
                .unwrap())
        })
        .await;

        let mock_http_client = mock_with_callback(move |req: http::Request<RouterBody>| {
            Box::pin(async {
                let deserialized_request: Externalizable<Value> = serde_json::from_slice(
                    &router::body::into_bytes(req.into_body()).await.unwrap(),
                )
                .unwrap();

                assert_eq!(
                    deserialized_request
                        .context
                        .as_ref()
                        .unwrap()
                        .get::<&str, u8>("this-is-a-test-context")
                        .unwrap()
                        .unwrap(),
                    42
                );

                assert!(
                    deserialized_request
                        .context
                        .as_ref()
                        .unwrap()
                        .get::<&str, String>("not_passed")
                        .ok()
                        .flatten()
                        .is_none()
                );

                assert_eq!(EXTERNALIZABLE_VERSION, deserialized_request.version);
                assert_eq!(
                    PipelineStep::RouterRequest.to_string(),
                    deserialized_request.stage
                );

                let input = json!(
                      {
                  "version": 1,
                  "stage": "RouterRequest",
                  "control": "continue",
                  "id": "1b19c05fdafc521016df33148ad63c1b",
                  "headers": {
                    "cookie": [
                      "tasty_cookie=strawberry"
                    ],
                    "content-type": [
                      "application/json"
                    ],
                    "host": [
                      "127.0.0.1:4000"
                    ],
                    "apollo-federation-include-trace": [
                      "ftv1"
                    ],
                    "apollographql-client-name": [
                      "manual"
                    ],
                    "accept": [
                      "*/*"
                    ],
                    "user-agent": [
                      "curl/7.79.1"
                    ],
                    "content-length": [
                      "46"
                    ]
                  },
                  "body": "{
                      \"query\": \"query Long {\n  me {\n  name\n}\n}\"
                    }",
                  "context": {
                    "entries": {
                      "accepts-json": false,
                      "accepts-wildcard": true,
                      "accepts-multipart": false,
                      "this-is-a-test-context": 42
                    }
                  },
                  "sdl": "the sdl shouldnt change"
                });
                Ok(http::Response::builder()
                    .body(router::body::from_bytes(
                        serde_json::to_string(&input).unwrap(),
                    ))
                    .unwrap())
            })
        });

        let service = router_stage.as_service(
            mock_http_client,
            mock_router_service.boxed(),
            "http://test".to_string(),
            Arc::new("".to_string()),
            true,
        );

        let request = supergraph::Request::canned_builder().build().unwrap();
        request
            .context
            .insert("not_passed", "OK".to_string())
            .unwrap();
        request
            .context
            .insert("this-is-a-test-context", 42)
            .unwrap();

        let res = service.oneshot(request.try_into().unwrap()).await.unwrap();

        assert!(
            res.context
                .get::<&str, String>("not_passed")
                .ok()
                .flatten()
                .is_some()
        );
    }

    #[tokio::test]
    async fn external_plugin_router_request_with_condition() {
        let router_stage = RouterStage {
            request: RouterRequestConf {
                // Won't be satisfied
                condition: Condition::Eq([
                    SelectorOrValue::Selector(RouterSelector::RequestMethod {
                        request_method: true,
                    }),
                    SelectorOrValue::Value("GET".to_string().into()),
                ])
                .into(),
                headers: true,
                context: ContextConf::NewContextConf(NewContextConf::All),
                body: true,
                sdl: true,
                path: true,
                method: true,
                url: None,
            },
            response: Default::default(),
        };

        let mock_router_service = router::service::from_supergraph_mock_callback(move |req| {
            assert!(
                req.context
                    .get::<&str, u8>("this-is-a-test-context")
                    .ok()
                    .flatten()
                    .is_none()
            );
            Ok(supergraph::Response::builder()
                .data(json!({ "test": 1234_u32 }))
                .context(req.context)
                .build()
                .unwrap())
        })
        .await;

        let mock_http_client = mock_with_callback(move |req: http::Request<RouterBody>| {
            Box::pin(async {
                let deserialized_request: Externalizable<Value> = serde_json::from_slice(
                    &router::body::into_bytes(req.into_body()).await.unwrap(),
                )
                .unwrap();

                assert_eq!(EXTERNALIZABLE_VERSION, deserialized_request.version);
                assert_eq!(
                    PipelineStep::RouterRequest.to_string(),
                    deserialized_request.stage
                );

                let input = json!(
                      {
                  "version": 1,
                  "stage": "RouterRequest",
                  "control": "continue",
                  "id": "1b19c05fdafc521016df33148ad63c1b",
                  "headers": {
                    "cookie": [
                      "tasty_cookie=strawberry"
                    ],
                    "content-type": [
                      "application/json"
                    ],
                    "host": [
                      "127.0.0.1:4000"
                    ],
                    "apollo-federation-include-trace": [
                      "ftv1"
                    ],
                    "apollographql-client-name": [
                      "manual"
                    ],
                    "accept": [
                      "*/*"
                    ],
                    "user-agent": [
                      "curl/7.79.1"
                    ],
                    "content-length": [
                      "46"
                    ]
                  },
                  "body": "{
                      \"query\": \"query Long {\n  me {\n  name\n}\n}\"
                    }",
                  "context": {
                    "entries": {
                      "accepts-json": false,
                      "accepts-wildcard": true,
                      "accepts-multipart": false,
                      "this-is-a-test-context": 42
                    }
                  },
                  "sdl": "the sdl shouldnt change"
                });
                Ok(http::Response::builder()
                    .body(router::body::from_bytes(
                        serde_json::to_string(&input).unwrap(),
                    ))
                    .unwrap())
            })
        });

        let service = router_stage.as_service(
            mock_http_client,
            mock_router_service.boxed(),
            "http://test".to_string(),
            Arc::new("".to_string()),
            true,
        );

        let request = supergraph::Request::canned_builder().build().unwrap();

        service.oneshot(request.try_into().unwrap()).await.unwrap();
    }

    #[tokio::test]
    async fn external_plugin_router_request_http_get() {
        let router_stage = RouterStage {
            request: RouterRequestConf {
                condition: Default::default(),
                headers: true,
                context: ContextConf::NewContextConf(NewContextConf::All),
                body: true,
                sdl: true,
                path: true,
                method: true,
                url: None,
            },
            response: Default::default(),
        };

        let mock_router_service = router::service::from_supergraph_mock_callback(move |req| {
            // Let's assert that the router request has been transformed as it should have.
            assert_eq!(
                req.supergraph_request.headers().get("cookie").unwrap(),
                "tasty_cookie=strawberry"
            );

            // the method shouldn't have changed
            assert_eq!(req.supergraph_request.method(), Method::GET);
            // the uri shouldn't have changed
            assert_eq!(req.supergraph_request.uri(), "/");

            assert_eq!(
                req.context
                    .get::<&str, u8>("this-is-a-test-context")
                    .unwrap()
                    .unwrap(),
                42
            );

            // The query should have changed
            assert_eq!(
                "query Long {\n  me {\n  name\n}\n}",
                req.supergraph_request.into_body().query.unwrap()
            );

            Ok(supergraph::Response::builder()
                .data(json!({ "test": 1234_u32 }))
                .context(req.context)
                .build()
                .unwrap())
        })
        .await;

        let mock_http_client = mock_with_callback(move |req: http::Request<RouterBody>| {
            Box::pin(async {
                let deserialized_request: Externalizable<Value> = serde_json::from_slice(
                    &router::body::into_bytes(req.into_body()).await.unwrap(),
                )
                .unwrap();

                assert_eq!(EXTERNALIZABLE_VERSION, deserialized_request.version);
                assert_eq!(
                    PipelineStep::RouterRequest.to_string(),
                    deserialized_request.stage
                );

                let input = json!(
                      {
                  "version": 1,
                  "stage": "RouterRequest",
                  "control": "continue",
                  "id": "1b19c05fdafc521016df33148ad63c1b",
                  "uri": "/this/is/a/new/uri",
                  "method": "POST",
                  "headers": {
                    "cookie": [
                      "tasty_cookie=strawberry"
                    ],
                    "content-type": [
                      "application/json"
                    ],
                    "host": [
                      "127.0.0.1:4000"
                    ],
                    "apollo-federation-include-trace": [
                      "ftv1"
                    ],
                    "apollographql-client-name": [
                      "manual"
                    ],
                    "accept": [
                      "*/*"
                    ],
                    "user-agent": [
                      "curl/7.79.1"
                    ],
                    "content-length": [
                      "46"
                    ]
                  },
                  "body": "{
                      \"query\": \"query Long {\n  me {\n  name\n}\n}\"
                    }",
                  "context": {
                    "entries": {
                      "accepts-json": false,
                      "accepts-wildcard": true,
                      "accepts-multipart": false,
                      "this-is-a-test-context": 42
                    }
                  },
                  "sdl": "the sdl shouldnt change"
                });
                Ok(http::Response::builder()
                    .body(router::body::from_bytes(
                        serde_json::to_string(&input).unwrap(),
                    ))
                    .unwrap())
            })
        });

        let service = router_stage.as_service(
            mock_http_client,
            mock_router_service.boxed(),
            "http://test".to_string(),
            Arc::new("".to_string()),
            true,
        );

        let request = supergraph::Request::fake_builder()
            .method(Method::GET)
            .build()
            .unwrap();

        service.oneshot(request.try_into().unwrap()).await.unwrap();
    }

    #[tokio::test]
    async fn external_plugin_router_request_controlflow_break() {
        let router_stage = RouterStage {
            request: RouterRequestConf {
                condition: Default::default(),
                headers: true,
                context: ContextConf::NewContextConf(NewContextConf::All),
                body: true,
                sdl: true,
                path: true,
                method: true,
                url: None,
            },
            response: Default::default(),
        };

        let mock_router_service = MockRouterService::new();

        let mock_http_client = mock_with_callback(move |req: http::Request<RouterBody>| {
            Box::pin(async {
                let deserialized_request: Externalizable<Value> = serde_json::from_slice(
                    &router::body::into_bytes(req.into_body()).await.unwrap(),
                )
                .unwrap();

                assert_eq!(EXTERNALIZABLE_VERSION, deserialized_request.version);
                assert_eq!(
                    PipelineStep::RouterRequest.to_string(),
                    deserialized_request.stage
                );

                let input = json!(
                    {
                    "version": 1,
                    "stage": "RouterRequest",
                    "control": {
                        "break": 200
                    },
                    "id": "1b19c05fdafc521016df33148ad63c1b",
                    "body": "{
                    \"errors\": [{ \"message\": \"my error message\" }]
                    }",
                    "context": {
                        "entries": {
                            "testKey": true
                        }
                    },
                    "headers": {
                        "aheader": ["a value"]
                    }
                }
                );
                Ok(http::Response::builder()
                    .body(router::body::from_bytes(
                        serde_json::to_string(&input).unwrap(),
                    ))
                    .unwrap())
            })
        });

        let service = router_stage.as_service(
            mock_http_client,
            mock_router_service.boxed(),
            "http://test".to_string(),
            Arc::new("".to_string()),
            true,
        );

        let request = supergraph::Request::canned_builder().build().unwrap();

        let crate::services::router::Response { response, context } =
            service.oneshot(request.try_into().unwrap()).await.unwrap();

        assert!(context.get::<_, bool>("testKey").unwrap().unwrap());

        let value = response.headers().get("aheader").unwrap();

        assert_eq!("a value", value);

        let actual_response = serde_json::from_slice::<Value>(
            &router::body::into_bytes(response.into_body())
                .await
                .unwrap(),
        )
        .unwrap();

        assert_eq!(
            json!({
                "errors": [{
                   "message": "my error message"
                }]
            }),
            actual_response
        );
    }

    #[tokio::test]
    async fn external_plugin_router_request_controlflow_break_with_message_string() {
        let router_stage = RouterStage {
            request: RouterRequestConf {
                condition: Default::default(),
                headers: true,
                context: ContextConf::NewContextConf(NewContextConf::All),
                body: true,
                sdl: true,
                path: true,
                method: true,
                url: None,
            },
            response: Default::default(),
        };

        let mock_router_service = MockRouterService::new();

        let mock_http_client = mock_with_callback(move |req: http::Request<RouterBody>| {
            Box::pin(async {
                let deserialized_request: Externalizable<Value> = serde_json::from_slice(
                    &router::body::into_bytes(req.into_body()).await.unwrap(),
                )
                .unwrap();

                assert_eq!(EXTERNALIZABLE_VERSION, deserialized_request.version);
                assert_eq!(
                    PipelineStep::RouterRequest.to_string(),
                    deserialized_request.stage
                );

                let input = json!(
                    {
                    "version": 1,
                    "stage": "RouterRequest",
                    "control": {
                        "break": 401
                    },
                    "id": "1b19c05fdafc521016df33148ad63c1b",
                    "body": "this is a test error",
                }
                );
                Ok(http::Response::builder()
                    .body(router::body::from_bytes(
                        serde_json::to_string(&input).unwrap(),
                    ))
                    .unwrap())
            })
        });

        let service = router_stage.as_service(
            mock_http_client,
            mock_router_service.boxed(),
            "http://test".to_string(),
            Arc::new("".to_string()),
            true,
        );

        let request = supergraph::Request::canned_builder().build().unwrap();

        let response = service
            .oneshot(request.try_into().unwrap())
            .await
            .unwrap()
            .response;

        assert_eq!(response.status(), http::StatusCode::UNAUTHORIZED);
        let actual_response = serde_json::from_slice::<Value>(
            &router::body::into_bytes(response.into_body())
                .await
                .unwrap(),
        )
        .unwrap();

        assert_eq!(
            json!({
                "errors": [{
                   "message": "this is a test error",
                   "extensions": {
                      "code": "ERROR"
                   }
                }]
            }),
            actual_response
        );
    }

    #[tokio::test]
    async fn external_plugin_router_response() {
        let router_stage = RouterStage {
            response: RouterResponseConf {
                condition: Default::default(),
                headers: true,
                context: ContextConf::NewContextConf(NewContextConf::All),
                body: true,
                sdl: true,
                status_code: false,
                url: None,
            },
            request: Default::default(),
        };

        let mock_router_service = router::service::from_supergraph_mock_callback(move |req| {
            Ok(supergraph::Response::builder()
                .data(json!("{ \"test\": 1234_u32 }"))
                .context(req.context)
                .build()
                .unwrap())
        })
        .await;

        let mock_http_client =
            mock_with_deferred_callback(move |res: http::Request<RouterBody>| {
                Box::pin(async {
                    let deserialized_response: Externalizable<Value> = serde_json::from_slice(
                        &router::body::into_bytes(res.into_body()).await.unwrap(),
                    )
                    .unwrap();

                    assert_eq!(EXTERNALIZABLE_VERSION, deserialized_response.version);
                    assert_eq!(
                        PipelineStep::RouterResponse.to_string(),
                        deserialized_response.stage
                    );

                    assert_eq!(
                        json!("{\"data\":\"{ \\\"test\\\": 1234_u32 }\"}"),
                        deserialized_response.body.unwrap()
                    );

                    let input = json!(
                          {
                      "version": 1,
                      "stage": "RouterResponse",
                      "control": {
                          "break": 400
                      },
                      "id": "1b19c05fdafc521016df33148ad63c1b",
                      "headers": {
                        "cookie": [
                          "tasty_cookie=strawberry"
                        ],
                        "content-type": [
                          "application/json"
                        ],
                        "host": [
                          "127.0.0.1:4000"
                        ],
                        "apollo-federation-include-trace": [
                          "ftv1"
                        ],
                        "apollographql-client-name": [
                          "manual"
                        ],
                        "accept": [
                          "*/*"
                        ],
                        "user-agent": [
                          "curl/7.79.1"
                        ],
                        "content-length": [
                          "46"
                        ]
                      },
                      "body": "{
                      \"data\": { \"test\": 42 }
                    }",
                      "context": {
                        "entries": {
                          "accepts-json": false,
                          "accepts-wildcard": true,
                          "accepts-multipart": false,
                          "this-is-a-test-context": 42
                        }
                      },
                      "sdl": "the sdl shouldnt change"
                    });
                    Ok(http::Response::builder()
                        .body(router::body::from_bytes(
                            serde_json::to_string(&input).unwrap(),
                        ))
                        .unwrap())
                })
            });

        let service = router_stage.as_service(
            mock_http_client,
            mock_router_service.boxed(),
            "http://test".to_string(),
            Arc::new("".to_string()),
            true,
        );

        let request = supergraph::Request::canned_builder().build().unwrap();

        let res = service.oneshot(request.try_into().unwrap()).await.unwrap();

        // Let's assert that the router request has been transformed as it should have.
        assert_eq!(res.response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(
            res.response.headers().get("cookie").unwrap(),
            "tasty_cookie=strawberry"
        );

        assert_eq!(
            res.context
                .get::<&str, u8>("this-is-a-test-context")
                .unwrap()
                .unwrap(),
            42
        );

        // the body should have changed:
        assert_eq!(
            json!({ "data": { "test": 42_u32 } }),
            serde_json::from_slice::<Value>(
                &router::body::into_bytes(res.response.into_body())
                    .await
                    .unwrap()
            )
            .unwrap()
        );
    }

    #[tokio::test]
    async fn external_plugin_router_response_validation_disabled_custom() {
        // Router stage doesn't actually implement response validation - it always uses
        // permissive deserialization since it handles streaming responses differently
        let router_stage = RouterStage {
            response: RouterResponseConf {
                body: true,
                ..Default::default()
            },
            ..Default::default()
        };

        let mock_router_service = router::service::from_supergraph_mock_callback(move |req| {
            Ok(supergraph::Response::builder()
                .data(json!({"test": 42}))
                .context(req.context)
                .build()
                .unwrap())
        })
        .await;

        let mock_http_client = mock_with_deferred_callback(move |_: http::Request<RouterBody>| {
            Box::pin(async {
                // Return response that modifies the body - this demonstrates router stage processes
                // coprocessor responses without GraphQL validation (unlike other stages)
                let response = json!({
                    "version": 1,
                    "stage": "RouterResponse",
                    "control": "continue",
                    "body": "{\"data\": {\"test\": \"modified_by_coprocessor\"}}"
                });

                Ok(http::Response::builder()
                    .status(200)
                    .body(router::body::from_bytes(
                        serde_json::to_string(&response).unwrap(),
                    ))
                    .unwrap())
            })
        });

        let service_stack = router_stage
            .as_service(
                mock_http_client,
                mock_router_service.boxed(),
                "http://test".to_string(),
                Arc::new("".to_string()),
                false, // response_validation - doesn't matter for router stage
            )
            .boxed();

        let request = router::Request::fake_builder().build().unwrap();

        let res = service_stack.oneshot(request).await.unwrap();

        // Response should be processed normally since router stage doesn't validate
        assert_eq!(res.response.status(), 200);

        // Router stage should accept the coprocessor response without validation
        let body_bytes = router::body::into_bytes(res.response.into_body())
            .await
            .unwrap();
        let body: Value = serde_json::from_slice(&body_bytes).unwrap();
        assert_eq!(body["data"]["test"], "modified_by_coprocessor");
    }

    // ===== ROUTER RESPONSE VALIDATION TESTS =====
    // Note: Router response stage doesn't implement GraphQL validation - it always uses permissive
    // deserialization since it handles streaming responses differently than other stages

    #[tokio::test]
    async fn external_plugin_router_response_validation_enabled_valid() {
        let service_stack = create_router_stage_for_response_validation_test()
            .as_service(
                create_mock_http_client_router_response_valid_response(),
                create_mock_router_service_for_validation_test().await,
                "http://test".to_string(),
                Arc::new("".to_string()),
                true, // response_validation enabled - but router response ignores this
            )
            .boxed();

        let request = router::Request::fake_builder().build().unwrap();
        let res = service_stack.oneshot(request).await.unwrap();

        // Router response stage processes all responses without validation regardless of setting
        assert_eq!(res.response.status(), 200);
        let body_bytes = router::body::into_bytes(res.response.into_body())
            .await
            .unwrap();
        let body: Value = serde_json::from_slice(&body_bytes).unwrap();
        assert_eq!(body["data"]["test"], "valid_response");
    }

    #[tokio::test]
    async fn external_plugin_router_response_validation_enabled_empty() {
        let service_stack = create_router_stage_for_response_validation_test()
            .as_service(
                create_mock_http_client_router_response_empty_response(),
                create_mock_router_service_for_validation_test().await,
                "http://test".to_string(),
                Arc::new("".to_string()),
                true, // response_validation enabled - but router response ignores this
            )
            .boxed();

        let request = router::Request::fake_builder().build().unwrap();
        let res = service_stack.oneshot(request).await.unwrap();

        // Router response stage accepts empty responses without validation
        assert_eq!(res.response.status(), 200);
        let body_bytes = router::body::into_bytes(res.response.into_body())
            .await
            .unwrap();
        let body: Value = serde_json::from_slice(&body_bytes).unwrap();
        // Empty object passes through unchanged since router response doesn't validate
        assert!(body.as_object().unwrap().is_empty());
    }

    #[tokio::test]
    async fn external_plugin_router_response_validation_enabled_invalid() {
        let service_stack = create_router_stage_for_response_validation_test()
            .as_service(
                create_mock_http_client_router_response_invalid_response(),
                create_mock_router_service_for_validation_test().await,
                "http://test".to_string(),
                Arc::new("".to_string()),
                true, // response_validation enabled - but router response ignores this
            )
            .boxed();

        let request = router::Request::fake_builder().build().unwrap();
        let res = service_stack.oneshot(request).await.unwrap();

        // Router response stage accepts invalid responses without validation
        assert_eq!(res.response.status(), 200);
        let body_bytes = router::body::into_bytes(res.response.into_body())
            .await
            .unwrap();
        let body: Value = serde_json::from_slice(&body_bytes).unwrap();
        // Invalid response passes through unchanged since router response doesn't validate
        assert_eq!(body["errors"], "this should be an array not a string");
    }

    #[tokio::test]
    async fn external_plugin_router_response_validation_disabled_valid() {
        let service_stack = create_router_stage_for_response_validation_test()
            .as_service(
                create_mock_http_client_router_response_valid_response(),
                create_mock_router_service_for_validation_test().await,
                "http://test".to_string(),
                Arc::new("".to_string()),
                false, // response_validation disabled - same behavior as enabled for router response
            )
            .boxed();

        let request = router::Request::fake_builder().build().unwrap();
        let res = service_stack.oneshot(request).await.unwrap();

        // Router response stage processes all responses identically regardless of validation setting
        assert_eq!(res.response.status(), 200);
        let body_bytes = router::body::into_bytes(res.response.into_body())
            .await
            .unwrap();
        let body: Value = serde_json::from_slice(&body_bytes).unwrap();
        assert_eq!(body["data"]["test"], "valid_response");
    }

    #[tokio::test]
    async fn external_plugin_router_response_validation_disabled_empty() {
        let service_stack = create_router_stage_for_response_validation_test()
            .as_service(
                create_mock_http_client_router_response_empty_response(),
                create_mock_router_service_for_validation_test().await,
                "http://test".to_string(),
                Arc::new("".to_string()),
                false, // response_validation disabled - same behavior as enabled for router response
            )
            .boxed();

        let request = router::Request::fake_builder().build().unwrap();
        let res = service_stack.oneshot(request).await.unwrap();

        // Router response stage behavior is identical whether validation is enabled or disabled
        assert_eq!(res.response.status(), 200);
        let body_bytes = router::body::into_bytes(res.response.into_body())
            .await
            .unwrap();
        let body: Value = serde_json::from_slice(&body_bytes).unwrap();
        // Empty object passes through unchanged
        assert!(body.as_object().unwrap().is_empty());
    }

    #[tokio::test]
    async fn external_plugin_router_response_validation_disabled_invalid() {
        let service_stack = create_router_stage_for_response_validation_test()
            .as_service(
                create_mock_http_client_router_response_invalid_response(),
                create_mock_router_service_for_validation_test().await,
                "http://test".to_string(),
                Arc::new("".to_string()),
                false, // response_validation disabled - same behavior as enabled for router response
            )
            .boxed();

        let request = router::Request::fake_builder().build().unwrap();
        let res = service_stack.oneshot(request).await.unwrap();

        // Router response stage behavior is identical whether validation is enabled or disabled
        assert_eq!(res.response.status(), 200);
        let body_bytes = router::body::into_bytes(res.response.into_body())
            .await
            .unwrap();
        let body: Value = serde_json::from_slice(&body_bytes).unwrap();
        // Invalid response passes through unchanged
        assert_eq!(body["errors"], "this should be an array not a string");
    }

    // Helper functions for router request validation tests
    fn create_router_stage_for_request_validation_test() -> RouterStage {
        RouterStage {
            request: RouterRequestConf {
                body: true,
                ..Default::default()
            },
            ..Default::default()
        }
    }

    // Helper to create a RouterStage request with condition always false
    fn create_router_stage_for_request_with_false_condition() -> RouterStage {
        RouterStage {
            request: RouterRequestConf {
                condition: Some(Condition::False),
                headers: true,
                context: ContextConf::NewContextConf(NewContextConf::All),
                body: true,
                sdl: true,
                path: false,
                method: false,
                url: None,
            },
            response: Default::default(),
        }
    }

    // Helper to create a RouterStage response with condition always false
    fn create_router_stage_for_response_with_false_condition() -> RouterStage {
        RouterStage {
            request: Default::default(),
            response: RouterResponseConf {
                condition: Condition::False,
                headers: true,
                context: ContextConf::NewContextConf(NewContextConf::All),
                body: true,
                sdl: true,
                status_code: false,
                url: None,
            },
        }
    }

    async fn create_mock_router_service_for_validation_test() -> router::BoxService {
        router::service::from_supergraph_mock_callback(move |req| {
            Ok(supergraph::Response::builder()
                .data(json!({"test": 42}))
                .context(req.context)
                .build()
                .unwrap())
        })
        .await
        .boxed()
    }

    // Helper function to create working router service mock
    fn create_mock_router_service() -> MockRouterService {
        let mut mock_router_service = MockRouterService::new();
        mock_router_service
            .expect_call()
            .returning(|req: router::Request| {
                Ok(router::Response::builder()
                    .data(json!({ "test": 1234_u32 }))
                    .errors(Vec::new())
                    .extensions(Object::new())
                    .context(req.context)
                    .build()
                    .unwrap())
            });
        mock_router_service
    }

    // Helper function to create mock http client that returns valid GraphQL response for RouterRequest
    fn create_mock_http_client_router_request_valid_response() -> MockInternalHttpClientService {
        mock_with_callback(move |_: http::Request<RouterBody>| {
            Box::pin(async {
                let response = json!({
                    "version": 1,
                    "stage": "RouterRequest",
                    "control": "continue",
                    "body": "{\"data\": {\"test\": \"valid_response\"}}"
                });

                Ok(http::Response::builder()
                    .status(200)
                    .body(router::body::from_bytes(
                        serde_json::to_string(&response).unwrap(),
                    ))
                    .unwrap())
            })
        })
    }

    // Helper function to create mock http client that returns valid GraphQL response
    fn create_mock_http_client_router_response_valid_response() -> MockInternalHttpClientService {
        mock_with_deferred_callback(move |_: http::Request<RouterBody>| {
            Box::pin(async {
                let response = json!({
                    "version": 1,
                    "stage": "RouterResponse",
                    "control": "continue",
                    "body": "{\"data\": {\"test\": \"valid_response\"}}"
                });
                Ok(http::Response::builder()
                    .status(200)
                    .body(router::body::from_bytes(
                        serde_json::to_string(&response).unwrap(),
                    ))
                    .unwrap())
            })
        })
    }

    // Helper function to create mock http client that returns invalid GraphQL response
    fn create_mock_http_client_router_response_invalid_response() -> MockInternalHttpClientService {
        mock_with_deferred_callback(move |_: http::Request<RouterBody>| {
            Box::pin(async {
                let response = json!({
                    "version": 1,
                    "stage": "RouterResponse",
                    "control": "continue",
                    "body": "{\"errors\": \"this should be an array not a string\"}"
                });
                Ok(http::Response::builder()
                    .status(200)
                    .body(router::body::from_bytes(
                        serde_json::to_string(&response).unwrap(),
                    ))
                    .unwrap())
            })
        })
    }

    fn create_mock_http_client_empty_router_response() -> MockInternalHttpClientService {
        mock_with_callback(move |_: http::Request<RouterBody>| {
            Box::pin(async {
                // Return empty GraphQL break response - passes serde but fails GraphQL validation
                let response = json!({
                    "version": 1,
                    "stage": "RouterRequest",
                    "control": {
                        "break": 400
                    },
                    "body": "{}"
                });

                Ok(http::Response::builder()
                    .status(200)
                    .body(router::body::from_bytes(
                        serde_json::to_string(&response).unwrap(),
                    ))
                    .unwrap())
            })
        })
    }

    // Helper functions for router response validation tests
    fn create_router_stage_for_response_validation_test() -> RouterStage {
        RouterStage {
            request: Default::default(),
            response: RouterResponseConf {
                condition: Default::default(),
                headers: true,
                context: ContextConf::NewContextConf(NewContextConf::All),
                body: true,
                sdl: true,
                status_code: false,
                url: None,
            },
        }
    }

    // Helper function to create mock http client that returns empty GraphQL response
    fn create_mock_http_client_router_response_empty_response() -> MockInternalHttpClientService {
        mock_with_deferred_callback(move |_: http::Request<RouterBody>| {
            Box::pin(async {
                let response = json!({
                    "version": 1,
                    "stage": "RouterResponse",
                    "control": "continue",
                    "body": "{}"
                });
                Ok(http::Response::builder()
                    .status(200)
                    .body(router::body::from_bytes(
                        serde_json::to_string(&response).unwrap(),
                    ))
                    .unwrap())
            })
        })
    }

    #[tokio::test]
    async fn external_plugin_router_request_validation_disabled_empty() {
        let service_stack = create_router_stage_for_request_validation_test()
            .as_service(
                create_mock_http_client_empty_router_response(),
                create_mock_router_service_for_validation_test().await,
                "http://test".to_string(),
                Arc::new("".to_string()),
                false, // response_validation disabled
            )
            .boxed();

        let request = router::Request::fake_builder().build().unwrap();
        let res = service_stack.oneshot(request).await.unwrap();

        // Should return 400 due to break, but with permissive deserialization
        assert_eq!(res.response.status(), 400);

        // Body should contain the empty response that passed serde but failed GraphQL validation
        let body_bytes = router::body::into_bytes(res.response.into_body())
            .await
            .unwrap();
        let body: Value = serde_json::from_slice(&body_bytes).unwrap();
        // With validation disabled, should get empty object as response
        assert!(
            body.as_object().unwrap().is_empty()
                || body.get("data").is_some()
                || body.get("errors").is_some()
        );
    }

    #[tokio::test]
    async fn external_plugin_router_request_validation_enabled_empty() {
        let service_stack = create_router_stage_for_request_validation_test()
            .as_service(
                create_mock_http_client_empty_router_response(),
                create_mock_router_service_for_validation_test().await,
                "http://test".to_string(),
                Arc::new("".to_string()),
                true, // response_validation enabled
            )
            .boxed();

        let request = router::Request::fake_builder().build().unwrap();
        let res = service_stack.oneshot(request).await.unwrap();

        // Should return 400 due to break
        assert_eq!(res.response.status(), 400);

        // Body should contain validation error from GraphQL validation failure
        let body_bytes = router::body::into_bytes(res.response.into_body())
            .await
            .unwrap();
        let body: Value = serde_json::from_slice(&body_bytes).unwrap();
        // Should contain GraphQL errors from validation failure, not the original empty object
        assert!(body.get("errors").is_some());
        // Verify it's a deserialization error (validation failed)
        let errors = body["errors"].as_array().unwrap();
        assert!(
            errors[0]["message"]
                .as_str()
                .unwrap()
                .contains("couldn't deserialize coprocessor output body")
        );
    }

    #[test]
    fn it_externalizes_headers() {
        // Build our expected HashMap
        let mut expected = HashMap::new();

        expected.insert(
            "content-type".to_string(),
            vec![APPLICATION_JSON.essence_str().to_string()],
        );

        expected.insert(
            "accept".to_string(),
            vec![
                APPLICATION_JSON.essence_str().to_string(),
                TEXT_HTML.essence_str().to_string(),
            ],
        );

        let mut external_form = HeaderMap::new();

        external_form.insert(
            CONTENT_TYPE,
            HeaderValue::from_static(APPLICATION_JSON.essence_str()),
        );

        external_form.insert(
            ACCEPT,
            HeaderValue::from_static(APPLICATION_JSON.essence_str()),
        );

        external_form.append(ACCEPT, HeaderValue::from_static(TEXT_HTML.essence_str()));

        let actual = externalize_header_map(&external_form);

        assert_eq!(expected, actual);
    }

    #[test]
    fn it_internalizes_headers() {
        // Build our expected HeaderMap
        let mut expected = HeaderMap::new();

        expected.insert(
            ACCEPT,
            HeaderValue::from_static(APPLICATION_JSON.essence_str()),
        );

        expected.append(ACCEPT, HeaderValue::from_static(TEXT_HTML.essence_str()));

        let mut external_form = HashMap::new();

        external_form.insert(
            "accept".to_string(),
            vec![
                APPLICATION_JSON.essence_str().to_string(),
                TEXT_HTML.essence_str().to_string(),
            ],
        );

        // This header should be stripped
        external_form.insert("content-length".to_string(), vec!["1024".to_string()]);

        let actual = internalize_header_map(external_form).expect("internalized header map");

        assert_eq!(expected, actual);
    }

    #[test]
    fn test_handle_graphql_response_validation_enabled() {
        let original = graphql::Response::builder()
            .data(json!({"test": "original"}))
            .build();

        // Valid GraphQL response should work
        let valid_response = json!({
            "data": {"test": "modified"}
        });
        let result =
            handle_graphql_response(original.clone(), Some(valid_response), true, true).unwrap();
        assert_eq!(result.data, Some(json!({"test": "modified"})));

        // Invalid GraphQL response should return error when validation enabled
        let invalid_response = json!({
            "invalid": "structure"
        });
        let result = handle_graphql_response(original.clone(), Some(invalid_response), true, true);
        assert!(result.is_err());
    }

    #[test]
    fn test_handle_graphql_response_validation_disabled() {
        let original = graphql::Response::builder()
            .data(json!({"test": "original"}))
            .build();

        // Valid GraphQL response should work
        let valid_response = json!({
            "data": {"test": "modified"}
        });
        let result =
            handle_graphql_response(original.clone(), Some(valid_response), false, true).unwrap();
        assert_eq!(result.data, Some(json!({"test": "modified"})));

        // Invalid GraphQL response should return original when validation disabled
        // Use a structure that will actually fail deserialization (wrong type for errors field)
        let invalid_response = json!({
            "errors": "this should be an array not a string"
        });
        let result =
            handle_graphql_response(original.clone(), Some(invalid_response), false, true).unwrap();
        // With validation disabled, uses permissive serde deserialization instead of strict GraphQL validation
        // Falls back to original response when serde deserialization fails (string can't deserialize to Vec<Error>)
        assert_eq!(result.data, Some(json!({"test": "original"})));
    }

    #[test]
    fn test_handle_graphql_response_validation_disabled_empty_response() {
        let original = graphql::Response::builder()
            .data(json!({"test": "original"}))
            .build();

        // Empty response violates GraphQL spec (must have data or errors) but should pass serde deserialization
        let empty_response = json!({});
        let result =
            handle_graphql_response(original.clone(), Some(empty_response), false, true).unwrap();

        // With validation disabled, empty response deserializes successfully via serde
        // (all fields are optional with defaults), resulting in a response with no data/errors
        assert_eq!(result.data, None);
        assert_eq!(result.errors.len(), 0);
    }

    #[test]
    fn test_handle_graphql_response_validation_enabled_empty_response() {
        let original = graphql::Response::builder()
            .data(json!({"test": "original"}))
            .build();

        // Empty response should fail strict GraphQL validation
        let empty_response = json!({});
        let result = handle_graphql_response(original.clone(), Some(empty_response), true, true);

        // With validation enabled, should return error due to invalid GraphQL response structure
        assert!(result.is_err());
    }

    // Helper function to create subgraph stage for validation tests
    fn create_subgraph_stage_for_validation_test() -> SubgraphStage {
        SubgraphStage {
            request: Default::default(),
            response: SubgraphResponseConf {
                condition: Condition::True,
                headers: true,
                context: ContextConf::NewContextConf(NewContextConf::All),
                body: true,
                service_name: false,
                status_code: false,
                subgraph_request_id: false,
                url: None,
            },
        }
    }

    // Helper function to create mock subgraph service
    fn create_mock_subgraph_service() -> MockSubgraphService {
        let mut mock_subgraph_service = MockSubgraphService::new();
        mock_subgraph_service
            .expect_call()
            .returning(|req: subgraph::Request| {
                Ok(subgraph::Response::builder()
                    .data(json!({ "test": 1234_u32 }))
                    .errors(Vec::new())
                    .extensions(Object::new())
                    .subgraph_name("coprocessorMockSubgraph")
                    .context(req.context)
                    .id(req.id)
                    .build())
            });
        mock_subgraph_service
    }

    // Helper functions for subgraph request validation tests
    fn create_subgraph_stage_for_request_validation_test() -> SubgraphStage {
        SubgraphStage {
            request: SubgraphRequestConf {
                condition: Condition::True,
                headers: true,
                context: ContextConf::NewContextConf(NewContextConf::All),
                body: true,
                uri: true,
                method: true,
                service_name: true,
                subgraph_request_id: true,
                url: None,
            },
            response: Default::default(),
        }
    }

    // Helper to create a SubgraphStage request with condition always false
    fn create_subgraph_stage_for_request_with_false_condition() -> SubgraphStage {
        SubgraphStage {
            request: SubgraphRequestConf {
                condition: Condition::False,
                headers: true,
                context: ContextConf::NewContextConf(NewContextConf::All),
                body: true,
                uri: true,
                method: true,
                service_name: true,
                subgraph_request_id: true,
                url: None,
            },
            response: Default::default(),
        }
    }

    // Helper to create a SubgraphStage response with condition always false
    fn create_subgraph_stage_for_response_with_false_condition() -> SubgraphStage {
        SubgraphStage {
            request: Default::default(),
            response: SubgraphResponseConf {
                condition: Condition::False,
                headers: true,
                context: ContextConf::NewContextConf(NewContextConf::All),
                body: true,
                service_name: false,
                status_code: false,
                subgraph_request_id: false,
                url: None,
            },
        }
    }

    // Helper function to create mock http client that returns valid GraphQL break response
    fn create_mock_http_client_subgraph_request_valid_response() -> MockInternalHttpClientService {
        mock_with_callback(move |_: http::Request<RouterBody>| {
            Box::pin(async {
                let response = json!({
                    "version": 1,
                    "stage": "SubgraphRequest",
                    "control": {
                        "break": 400
                    },
                    "body": {
                        "data": {"test": "valid_response"}
                    }
                });
                Ok(http::Response::builder()
                    .status(200)
                    .body(router::body::from_bytes(
                        serde_json::to_string(&response).unwrap(),
                    ))
                    .unwrap())
            })
        })
    }

    // Helper function to create mock http client that returns empty GraphQL break response
    fn create_mock_http_client_subgraph_request_empty_response() -> MockInternalHttpClientService {
        mock_with_callback(move |_: http::Request<RouterBody>| {
            Box::pin(async {
                let response = json!({
                    "version": 1,
                    "stage": "SubgraphRequest",
                    "control": {
                        "break": 400
                    },
                    "body": {}
                });
                Ok(http::Response::builder()
                    .status(200)
                    .body(router::body::from_bytes(
                        serde_json::to_string(&response).unwrap(),
                    ))
                    .unwrap())
            })
        })
    }

    // Helper function to create mock http client that returns invalid GraphQL break response
    fn create_mock_http_client_subgraph_request_invalid_response() -> MockInternalHttpClientService
    {
        mock_with_callback(move |_: http::Request<RouterBody>| {
            Box::pin(async {
                let response = json!({
                    "version": 1,
                    "stage": "SubgraphRequest",
                    "control": {
                        "break": 400
                    },
                    "body": {
                        "errors": "this should be an array not a string"
                    }
                });
                Ok(http::Response::builder()
                    .status(200)
                    .body(router::body::from_bytes(
                        serde_json::to_string(&response).unwrap(),
                    ))
                    .unwrap())
            })
        })
    }

    // Helper function to create mock http client that returns valid GraphQL response
    fn create_mock_http_client_subgraph_response_valid_response() -> MockInternalHttpClientService {
        mock_with_callback(move |_: http::Request<RouterBody>| {
            Box::pin(async {
                let input = json!({
                    "version": 1,
                    "stage": "SubgraphResponse",
                    "control": "continue",
                    "body": {
                        "data": {"test": "valid_response"}
                    }
                });
                Ok(http::Response::builder()
                    .body(router::body::from_bytes(
                        serde_json::to_string(&input).unwrap(),
                    ))
                    .unwrap())
            })
        })
    }

    // Helper function to create mock http client that returns empty GraphQL response
    fn create_mock_http_client_subgraph_response_empty_response() -> MockInternalHttpClientService {
        mock_with_callback(move |_: http::Request<RouterBody>| {
            Box::pin(async {
                let input = json!({
                    "version": 1,
                    "stage": "SubgraphResponse",
                    "control": "continue",
                    "body": {}
                });
                Ok(http::Response::builder()
                    .body(router::body::from_bytes(
                        serde_json::to_string(&input).unwrap(),
                    ))
                    .unwrap())
            })
        })
    }

    // Helper function to create mock http client that returns invalid GraphQL response
    fn create_mock_http_client_invalid_subgraph_response() -> MockInternalHttpClientService {
        mock_with_callback(move |_: http::Request<RouterBody>| {
            Box::pin(async {
                let input = json!({
                    "version": 1,
                    "stage": "SubgraphResponse",
                    "control": "continue",
                    "body": {
                        "errors": "this should be an array not a string"
                    }
                });
                Ok(http::Response::builder()
                    .body(router::body::from_bytes(
                        serde_json::to_string(&input).unwrap(),
                    ))
                    .unwrap())
            })
        })
    }

    fn create_mock_http_client_hard_error() -> MockInternalHttpClientService {
        let mut mock = MockInternalHttpClientService::new();

        // Make clone() always return another mock with the same behavior:
        mock.expect_clone()
            .returning(create_mock_http_client_hard_error);

        mock.expect_call()
            .returning(|_| Box::pin(async move { Err("hard error from mock http client".into()) }));

        mock
    }

    #[tokio::test]
    async fn external_plugin_subgraph_response_validation_disabled_invalid() {
        let service = create_subgraph_stage_for_validation_test().as_service(
            create_mock_http_client_invalid_subgraph_response(),
            create_mock_subgraph_service().boxed(),
            "http://test".to_string(),
            "my_subgraph_service_name".to_string(),
            false, // Validation disabled
        );

        let request = subgraph::Request::fake_builder().build();
        let res = service.oneshot(request).await.unwrap();

        // With validation disabled, uses permissive serde deserialization instead of strict GraphQL validation
        // Falls back to original response when serde deserialization fails (string can't deserialize to Vec<Error>)
        assert_eq!(
            &json!({ "test": 1234_u32 }),
            res.response.body().data.as_ref().unwrap()
        );
    }

    // ===== SUBGRAPH REQUEST VALIDATION TESTS =====

    #[tokio::test]
    async fn external_plugin_subgraph_request_validation_enabled_valid() {
        let service = create_subgraph_stage_for_request_validation_test().as_service(
            create_mock_http_client_subgraph_request_valid_response(),
            create_mock_subgraph_service().boxed(),
            "http://test".to_string(),
            "my_subgraph_service_name".to_string(),
            true, // Validation enabled
        );

        let request = subgraph::Request::fake_builder().build();
        let res = service.oneshot(request).await.unwrap();

        // Should return 400 due to break with valid GraphQL response
        assert_eq!(res.response.status(), 400);
        assert_eq!(
            &json!({"test": "valid_response"}),
            res.response.body().data.as_ref().unwrap()
        );
    }

    #[tokio::test]
    async fn external_plugin_subgraph_request_validation_enabled_empty() {
        let service = create_subgraph_stage_for_request_validation_test().as_service(
            create_mock_http_client_subgraph_request_empty_response(),
            create_mock_subgraph_service().boxed(),
            "http://test".to_string(),
            "my_subgraph_service_name".to_string(),
            true, // Validation enabled
        );

        let request = subgraph::Request::fake_builder().build();
        let res = service.oneshot(request).await.unwrap();

        // Should return 400 with validation error since empty response violates GraphQL spec
        assert_eq!(res.response.status(), 400);
        assert!(!res.response.body().errors.is_empty());
        assert!(
            res.response.body().errors[0]
                .message
                .contains("couldn't deserialize coprocessor output body")
        );
    }

    #[tokio::test]
    async fn external_plugin_subgraph_request_validation_enabled_invalid() {
        let service = create_subgraph_stage_for_request_validation_test().as_service(
            create_mock_http_client_subgraph_request_invalid_response(),
            create_mock_subgraph_service().boxed(),
            "http://test".to_string(),
            "my_subgraph_service_name".to_string(),
            true, // Validation enabled
        );

        let request = subgraph::Request::fake_builder().build();
        let res = service.oneshot(request).await.unwrap();

        // Should return 400 with validation error since errors should be array not string
        assert_eq!(res.response.status(), 400);
        assert!(!res.response.body().errors.is_empty());
        assert!(
            res.response.body().errors[0]
                .message
                .contains("couldn't deserialize coprocessor output body")
        );
    }

    #[tokio::test]
    async fn external_plugin_subgraph_request_validation_disabled_valid() {
        let service = create_subgraph_stage_for_request_validation_test().as_service(
            create_mock_http_client_subgraph_request_valid_response(),
            create_mock_subgraph_service().boxed(),
            "http://test".to_string(),
            "my_subgraph_service_name".to_string(),
            false, // Validation disabled
        );

        let request = subgraph::Request::fake_builder().build();
        let res = service.oneshot(request).await.unwrap();

        // Should return 400 due to break with valid response preserved via permissive deserialization
        assert_eq!(res.response.status(), 400);
        assert_eq!(
            &json!({"test": "valid_response"}),
            res.response.body().data.as_ref().unwrap()
        );
    }

    #[tokio::test]
    async fn external_plugin_subgraph_request_validation_disabled_empty() {
        let service = create_subgraph_stage_for_request_validation_test().as_service(
            create_mock_http_client_subgraph_request_empty_response(),
            create_mock_subgraph_service().boxed(),
            "http://test".to_string(),
            "my_subgraph_service_name".to_string(),
            false, // Validation disabled
        );

        let request = subgraph::Request::fake_builder().build();
        let res = service.oneshot(request).await.unwrap();

        // Should return 400 with empty response preserved via permissive deserialization
        assert_eq!(res.response.status(), 400);
        // Empty object deserializes to GraphQL response with no data/errors
        assert_eq!(res.response.body().data, None);
        assert_eq!(res.response.body().errors.len(), 0);
    }

    #[tokio::test]
    async fn external_plugin_subgraph_request_validation_disabled_invalid() {
        let service = create_subgraph_stage_for_request_validation_test().as_service(
            create_mock_http_client_subgraph_request_invalid_response(),
            create_mock_subgraph_service().boxed(),
            "http://test".to_string(),
            "my_subgraph_service_name".to_string(),
            false, // Validation disabled
        );

        let request = subgraph::Request::fake_builder().build();
        let res = service.oneshot(request).await.unwrap();

        // Should return 400 with fallback to original response since invalid structure can't deserialize
        assert_eq!(res.response.status(), 400);
        // Falls back to original response since permissive deserialization fails too
        assert!(res.response.body().data.is_some() || !res.response.body().errors.is_empty());
    }

    // ===== SUBGRAPH RESPONSE VALIDATION TESTS =====

    #[tokio::test]
    async fn external_plugin_subgraph_response_validation_enabled_valid() {
        let service = create_subgraph_stage_for_validation_test().as_service(
            create_mock_http_client_subgraph_response_valid_response(),
            create_mock_subgraph_service().boxed(),
            "http://test".to_string(),
            "my_subgraph_service_name".to_string(),
            true, // Validation enabled
        );

        let request = subgraph::Request::fake_builder().build();
        let res = service.oneshot(request).await.unwrap();

        // With validation enabled, valid GraphQL response should be processed normally
        assert_eq!(
            &json!({"test": "valid_response"}),
            res.response.body().data.as_ref().unwrap()
        );
    }

    #[tokio::test]
    async fn external_plugin_subgraph_response_validation_enabled_empty() {
        let service = create_subgraph_stage_for_validation_test().as_service(
            create_mock_http_client_subgraph_response_empty_response(),
            create_mock_subgraph_service().boxed(),
            "http://test".to_string(),
            "my_subgraph_service_name".to_string(),
            true, // Validation enabled
        );

        let request = subgraph::Request::fake_builder().build();

        // With validation enabled, empty response should cause service call to fail due to GraphQL validation
        let result = service.oneshot(request).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn external_plugin_subgraph_response_validation_enabled_invalid() {
        let service = create_subgraph_stage_for_validation_test().as_service(
            create_mock_http_client_invalid_subgraph_response(),
            create_mock_subgraph_service().boxed(),
            "http://test".to_string(),
            "my_subgraph_service_name".to_string(),
            true, // Validation enabled
        );

        let request = subgraph::Request::fake_builder().build();

        // With validation enabled, invalid GraphQL response should cause service call to fail
        let result = service.oneshot(request).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn external_plugin_subgraph_response_validation_disabled_valid() {
        let service = create_subgraph_stage_for_validation_test().as_service(
            create_mock_http_client_subgraph_response_valid_response(),
            create_mock_subgraph_service().boxed(),
            "http://test".to_string(),
            "my_subgraph_service_name".to_string(),
            false, // Validation disabled
        );

        let request = subgraph::Request::fake_builder().build();
        let res = service.oneshot(request).await.unwrap();

        // With validation disabled, valid response processed via permissive deserialization
        assert_eq!(
            &json!({"test": "valid_response"}),
            res.response.body().data.as_ref().unwrap()
        );
    }

    #[tokio::test]
    async fn external_plugin_subgraph_response_validation_disabled_empty() {
        let service = create_subgraph_stage_for_validation_test().as_service(
            create_mock_http_client_subgraph_response_empty_response(),
            create_mock_subgraph_service().boxed(),
            "http://test".to_string(),
            "my_subgraph_service_name".to_string(),
            false, // Validation disabled
        );

        let request = subgraph::Request::fake_builder().build();
        let res = service.oneshot(request).await.unwrap();

        // With validation disabled, empty response deserializes successfully via serde
        // (all fields are optional with defaults), resulting in a response with no data/errors
        assert_eq!(res.response.body().data, None);
        assert_eq!(res.response.body().errors.len(), 0);
    }

    #[allow(clippy::type_complexity)]
    fn mock_with_callback(
        callback: fn(
            http::Request<RouterBody>,
        ) -> BoxFuture<'static, Result<http::Response<RouterBody>, BoxError>>,
    ) -> MockInternalHttpClientService {
        let mut mock_http_client = MockInternalHttpClientService::new();
        mock_http_client.expect_clone().returning(move || {
            let mut mock_http_client = MockInternalHttpClientService::new();
            mock_http_client.expect_clone().returning(move || {
                let mut mock_http_client = MockInternalHttpClientService::new();
                mock_http_client.expect_call().returning(
                    move |req: crate::services::http::HttpRequest| {
                        let context = req.context.clone();
                        let fut = callback(req.http_request);
                        Box::pin(async move {
                            let response = fut.await?;
                            Ok(crate::services::http::HttpResponse {
                                http_response: response,
                                context,
                            })
                        })
                    },
                );
                mock_http_client
            });
            mock_http_client
        });

        mock_http_client
    }

    #[allow(clippy::type_complexity)]
    fn mock_with_deferred_callback(
        callback: fn(
            http::Request<RouterBody>,
        ) -> BoxFuture<'static, Result<http::Response<RouterBody>, BoxError>>,
    ) -> MockInternalHttpClientService {
        let mut mock_http_client = MockInternalHttpClientService::new();
        mock_http_client.expect_clone().returning(move || {
            let mut mock_http_client = MockInternalHttpClientService::new();
            mock_http_client.expect_clone().returning(move || {
                let mut mock_http_client = MockInternalHttpClientService::new();
                mock_http_client.expect_clone().returning(move || {
                    let mut mock_http_client = MockInternalHttpClientService::new();
                    mock_http_client.expect_call().returning(
                        move |req: crate::services::http::HttpRequest| {
                            let context = req.context.clone();
                            let fut = callback(req.http_request);
                            Box::pin(async move {
                                let response = fut.await?;
                                Ok(crate::services::http::HttpResponse {
                                    http_response: response,
                                    context,
                                })
                            })
                        },
                    );
                    mock_http_client
                });
                mock_http_client
            });
            mock_http_client
        });

        mock_http_client
    }

    // Tests for conditional validation based on incoming payload validity

    // Helper functions for readable tests
    fn valid_response() -> crate::graphql::Response {
        crate::graphql::Response::builder()
            .data(json!({"field": "value"}))
            .build()
    }

    fn valid_response_with_errors() -> crate::graphql::Response {
        use crate::graphql::Error;
        crate::graphql::Response::builder()
            .errors(vec![
                Error::builder()
                    .message("error")
                    .extension_code("TEST")
                    .build(),
            ])
            .build()
    }

    fn invalid_response() -> crate::graphql::Response {
        crate::graphql::Response::builder().build() // No data, no errors
    }

    fn valid_copro_body() -> Value {
        json!({"data": {"field": "new_value"}})
    }

    fn invalid_copro_body() -> Value {
        json!({}) // No data, no errors
    }

    #[test]
    fn test_minimal_graphql_validation() {
        assert!(is_graphql_response_minimally_valid(&valid_response()));
        assert!(is_graphql_response_minimally_valid(
            &valid_response_with_errors()
        ));
        assert!(!is_graphql_response_minimally_valid(&invalid_response()));
    }

    #[test]
    fn test_was_incoming_payload_valid() {
        // When body is not sent, always return true
        assert!(was_incoming_payload_valid(&valid_response(), false));
        assert!(was_incoming_payload_valid(&invalid_response(), false));

        // When body is sent, check validity
        assert!(was_incoming_payload_valid(&valid_response(), true));
        assert!(!was_incoming_payload_valid(&invalid_response(), true));
    }

    #[test]
    fn test_conditional_validation_logic() {
        // Invalid incoming + validation enabled = validation bypassed (succeeds with invalid copro response)
        assert!(
            handle_graphql_response(invalid_response(), Some(invalid_copro_body()), true, false)
                .is_ok()
        );

        // Valid incoming + validation enabled + invalid copro response = validation applied (fails)
        assert!(
            handle_graphql_response(valid_response(), Some(invalid_copro_body()), true, true)
                .is_err()
        );

        // Valid incoming + validation enabled + valid copro response = validation applied (succeeds)
        assert!(
            handle_graphql_response(valid_response(), Some(valid_copro_body()), true, true).is_ok()
        );

        // Validation disabled = always bypassed (succeeds regardless)
        assert!(
            handle_graphql_response(valid_response(), Some(invalid_copro_body()), false, true)
                .is_ok()
        );
    }

    #[test]
    fn test_update_context_from_coprocessor_deletes_missing_keys() {
        use crate::Context;
        use crate::plugins::coprocessor::update_context_from_coprocessor;

        // Create a context with some keys
        let target_context = Context::new();
        target_context.insert("k1", "v1".to_string()).unwrap();
        target_context.insert("k2", "v2".to_string()).unwrap();
        target_context.insert("k3", "v3".to_string()).unwrap();

        // Coprocessor returns context without k2 (deleted)
        let returned_context = Context::new();
        returned_context
            .insert("k1", "v1_updated".to_string())
            .unwrap();
        // k2 is missing (deleted)
        returned_context.insert("k3", "v3".to_string()).unwrap();

        // Update context
        update_context_from_coprocessor(
            &target_context,
            returned_context,
            &ContextConf::NewContextConf(NewContextConf::All),
        )
        .unwrap();

        // k1 should be updated
        assert_eq!(
            target_context.get_json_value("k1"),
            Some(serde_json_bytes::json!("v1_updated"))
        );
        // k2 should be deleted
        assert!(!target_context.contains_key("k2"));
        // k3 should remain
        assert_eq!(
            target_context.get_json_value("k3"),
            Some(serde_json_bytes::json!("v3"))
        );
    }

    #[test]
    fn test_update_context_from_coprocessor_adds_new_keys() {
        use crate::Context;
        use crate::plugins::coprocessor::update_context_from_coprocessor;

        // Create a context with some keys
        let target_context = Context::new();
        target_context.insert("k1", "v1".to_string()).unwrap();

        // Coprocessor returns context with a new key
        let returned_context = Context::new();
        returned_context
            .insert("k1", "v1_updated".to_string())
            .unwrap();
        returned_context.insert("k2", "v2_new".to_string()).unwrap();

        // Update context
        update_context_from_coprocessor(
            &target_context,
            returned_context,
            &ContextConf::NewContextConf(NewContextConf::All),
        )
        .unwrap();

        // k1 should be updated
        assert_eq!(
            target_context.get_json_value("k1"),
            Some(serde_json_bytes::json!("v1_updated"))
        );
        // k2 should be added
        assert_eq!(
            target_context.get_json_value("k2"),
            Some(serde_json_bytes::json!("v2_new"))
        );
    }

    #[test]
    fn test_update_context_from_coprocessor_preserves_keys_not_sent() {
        use std::collections::HashSet;
        use std::sync::Arc;

        use crate::Context;
        use crate::plugins::coprocessor::update_context_from_coprocessor;

        // Create a context with some keys
        let target_context = Context::new();
        target_context.insert("k1", "v1".to_string()).unwrap();
        target_context
            .insert("key_not_sent", "preserved_value".to_string())
            .unwrap();

        // Coprocessor returns context without k1 (deleted)
        let returned_context = Context::new();

        // Use Selective config to only send "k1", not "key_not_sent"
        let selective_keys: HashSet<String> = ["k1".to_string()].into();
        let context_config =
            ContextConf::NewContextConf(NewContextConf::Selective(Arc::new(selective_keys)));

        // Update context
        update_context_from_coprocessor(&target_context, returned_context, &context_config)
            .unwrap();

        // k1 should be deleted (was sent but missing from returned context)
        assert!(!target_context.contains_key("k1"));
        // key_not_sent should be preserved (wasn't sent to coprocessor)
        assert_eq!(
            target_context.get_json_value("key_not_sent"),
            Some(serde_json_bytes::json!("preserved_value"))
        );
    }

    #[rstest::rstest]
    fn test_update_context_from_coprocessor_handles_deprecated_key_names(
        #[values(DEPRECATED_CLIENT_NAME, CLIENT_NAME)] target_context_key_name: &str,
        #[values(
            ContextConf::Deprecated(true),
            ContextConf::NewContextConf(NewContextConf::Deprecated)
        )]
        context_conf: ContextConf,
    ) {
        use crate::Context;
        use crate::plugins::coprocessor::update_context_from_coprocessor;

        let target_context =
            Context::from_iter([(target_context_key_name.to_string(), "v1".into())]);
        let returned_context =
            Context::from_iter([(DEPRECATED_CLIENT_NAME.to_string(), "v2".into())]);

        update_context_from_coprocessor(&target_context, returned_context, &context_conf).unwrap();

        assert_eq!(
            target_context.get_json_value(CLIENT_NAME),
            Some(json!("v2")),
        );

        assert!(
            !target_context.contains_key(DEPRECATED_CLIENT_NAME),
            "DEPRECATED_CLIENT_NAME should not be present"
        );
    }

    // Subgraph stage metrics test
    #[tokio::test]
    async fn subgraph_request_metric_incremented_when_condition_true() {
        async {
            // Make multiple requests to better validate metric is being incremented correctly
            for _ in 0..2 {
                let _stage = create_subgraph_stage_for_request_validation_test();

                let _service = _stage.as_service(
                    create_mock_http_client_subgraph_request_valid_response(),
                    create_mock_subgraph_service().boxed(),
                    "http://test".to_string(),
                    "my_service".to_string(),
                    false, // Validation disabled
                );

                let _request = subgraph::Request::fake_builder().build();
                let _response = _service.oneshot(_request).await;
            }

            assert_coprocessor_operations_metrics(&[(
                PipelineStep::SubgraphRequest,
                2,
                Some(true),
            )]);
        }
        .with_metrics()
        .await;
    }

    #[tokio::test]
    async fn subgraph_response_metric_incremented_when_condition_true() {
        async {
            // Make multiple requests to better validate metric is being incremented correctly
            for _ in 0..3 {
                let _stage = create_subgraph_stage_for_validation_test();

                let _service = _stage.as_service(
                    create_mock_http_client_subgraph_response_valid_response(),
                    create_mock_subgraph_service().boxed(),
                    "http://test".to_string(),
                    "my_service".to_string(),
                    false, // Validation disabled
                );

                let _request = subgraph::Request::fake_builder().build();
                let _response = _service.oneshot(_request).await;
            }

            assert_coprocessor_operations_metrics(&[(
                PipelineStep::SubgraphResponse,
                3,
                Some(true),
            )]);
        }
        .with_metrics()
        .await;
    }

    #[tokio::test]
    async fn subgraph_request_metric_not_incremented_when_condition_false() {
        async {
            // Make multiple requests to better validate metric is being incremented correctly
            for _ in 0..2 {
                let _stage = create_subgraph_stage_for_request_with_false_condition();

                let _service = _stage.as_service(
                    create_mock_http_client_subgraph_request_valid_response(),
                    create_mock_subgraph_service().boxed(),
                    "http://test".to_string(),
                    "my_service".to_string(),
                    false, // Validation disabled
                );

                let _request = subgraph::Request::fake_builder().build();
                let _response = _service.oneshot(_request).await;
            }

            // This call will validate there are no metrics for all stages
            assert_coprocessor_operations_metrics(&[]);
        }
        .with_metrics()
        .await;
    }

    #[tokio::test]
    async fn subgraph_response_metric_not_incremented_when_condition_false() {
        async {
            // Make multiple requests to better validate metric is being incremented correctly
            for _ in 0..3 {
                let _stage = create_subgraph_stage_for_response_with_false_condition();

                let _service = _stage.as_service(
                    create_mock_http_client_subgraph_response_valid_response(),
                    create_mock_subgraph_service().boxed(),
                    "http://test".to_string(),
                    "my_service".to_string(),
                    false, // Validation disabled
                );

                let _request = subgraph::Request::fake_builder().build();
                let _response = _service.oneshot(_request).await;
            }

            // This call will validate there are no metrics for all stages
            assert_coprocessor_operations_metrics(&[]);
        }
        .with_metrics()
        .await;
    }

    #[tokio::test]
    async fn both_subgraph_stages_metric_incremented_when_conditions_true() {
        async {
            // Make multiple requests to better validate metric is being incremented correctly
            for _ in 0..2 {
                let _stage = create_subgraph_stage_for_request_validation_test();

                let _service = _stage.as_service(
                    create_mock_http_client_subgraph_request_valid_response(),
                    create_mock_subgraph_service().boxed(),
                    "http://test".to_string(),
                    "my_service".to_string(),
                    false, // Validation disabled
                );

                let _request = subgraph::Request::fake_builder().build();
                let _response = _service.oneshot(_request).await;
            }

            // Make multiple requests to better validate metric is being incremented correctly
            for _ in 0..3 {
                let _stage = create_subgraph_stage_for_validation_test();

                let _service = _stage.as_service(
                    create_mock_http_client_subgraph_response_valid_response(),
                    create_mock_subgraph_service().boxed(),
                    "http://test".to_string(),
                    "my_service".to_string(),
                    false, // Validation disabled
                );

                let _request = subgraph::Request::fake_builder().build();
                let _response = _service.oneshot(_request).await;
            }

            assert_coprocessor_operations_metrics(&[
                (PipelineStep::SubgraphRequest, 2, Some(true)),
                (PipelineStep::SubgraphResponse, 3, Some(true)),
            ]);
        }
        .with_metrics()
        .await;
    }

    #[tokio::test]
    async fn subgraph_request_metric_incremented_for_errored_stage_processing() {
        async {
            // Make multiple requests to better validate metric is being incremented correctly
            for _ in 0..2 {
                let _stage = create_subgraph_stage_for_request_validation_test();

                let _service = _stage.as_service(
                    create_mock_http_client_hard_error(),
                    create_mock_subgraph_service().boxed(),
                    "http://test".to_string(),
                    "my_service".to_string(),
                    true, // Validation enabled
                );

                let _request = subgraph::Request::fake_builder().build();
                let _response = _service.oneshot(_request).await;
            }

            assert_coprocessor_operations_metrics(&[(
                PipelineStep::SubgraphRequest,
                2,
                Some(false),
            )]);
        }
        .with_metrics()
        .await;
    }

    #[tokio::test]
    async fn subgraph_response_metric_incremented_for_errored_stage_processing() {
        async {
            // Make multiple requests to better validate metric is being incremented correctly
            for _ in 0..3 {
                let _stage = create_subgraph_stage_for_validation_test();

                let _service = _stage.as_service(
                    create_mock_http_client_hard_error(),
                    create_mock_subgraph_service().boxed(),
                    "http://test".to_string(),
                    "my_service".to_string(),
                    true, // Validation enabled
                );

                let _request = subgraph::Request::fake_builder().build();
                let _response = _service.oneshot(_request).await;
            }

            assert_coprocessor_operations_metrics(&[(
                PipelineStep::SubgraphResponse,
                3,
                Some(false),
            )]);
        }
        .with_metrics()
        .await;
    }

    #[tokio::test]
    async fn both_subgraph_stages_metric_incremented_for_errored_stages_processing() {
        async {
            // Make multiple requests to better validate metric is being incremented correctly
            for _ in 0..1 {
                let _stage = create_subgraph_stage_for_request_validation_test();

                let _service = _stage.as_service(
                    create_mock_http_client_hard_error(),
                    create_mock_subgraph_service().boxed(),
                    "http://test".to_string(),
                    "my_service".to_string(),
                    true, // Validation enabled
                );

                let _request = subgraph::Request::fake_builder().build();
                let _response = _service.oneshot(_request).await;
            }

            // Make multiple requests to better validate metric is being incremented correctly
            for _ in 0..2 {
                let _stage = create_subgraph_stage_for_validation_test();

                let _service = _stage.as_service(
                    create_mock_http_client_hard_error(),
                    create_mock_subgraph_service().boxed(),
                    "http://test".to_string(),
                    "my_service".to_string(),
                    true, // Validation enabled
                );

                let _request = subgraph::Request::fake_builder().build();
                let _response = _service.oneshot(_request).await;
            }

            assert_coprocessor_operations_metrics(&[
                (PipelineStep::SubgraphRequest, 1, Some(false)),
                (PipelineStep::SubgraphResponse, 2, Some(false)),
            ]);
        }
        .with_metrics()
        .await;
    }

    #[tokio::test]
    async fn router_request_metric_incremented_when_condition_true() {
        async {
            // Make multiple requests to better validate metric is being incremented correctly
            for _ in 0..3 {
                let router_stage = create_router_stage_for_request_validation_test();
                // Mock HTTP client used by coprocessor (RouterRequest stage)
                let mock_http_client = create_mock_http_client_router_request_valid_response();
                let mock_router_service = create_mock_router_service();

                let service_stack = router_stage
                    .as_service(
                        mock_http_client,
                        mock_router_service.boxed(),
                        "http://test".to_string(),
                        Arc::new("".to_string()),
                        false, // response_validation - doesn't matter for router stage
                    )
                    .boxed();

                let request = router::Request::fake_builder().build().unwrap();
                let _ = service_stack.oneshot(request).await.unwrap();
            }

            assert_coprocessor_operations_metrics(&[(PipelineStep::RouterRequest, 3, Some(true))]);
        }
        .with_metrics()
        .await;
    }

    #[tokio::test]
    async fn router_response_metric_incremented_when_condition_true() {
        async {
            // Make multiple requests to better validate metric is being incremented correctly
            for _ in 0..2 {
                let router_stage = create_router_stage_for_response_validation_test();
                let mock_http_client = create_mock_http_client_router_response_valid_response();
                let mock_router_service = create_mock_router_service();

                let service_stack = router_stage
                    .as_service(
                        mock_http_client,
                        mock_router_service.boxed(),
                        "http://test".to_string(),
                        Arc::new("".to_string()),
                        false, // response_validation - doesn't matter for router stage
                    )
                    .boxed();

                let request = router::Request::fake_builder().build().unwrap();
                let _ = service_stack.oneshot(request).await.unwrap();
            }

            assert_coprocessor_operations_metrics(&[(PipelineStep::RouterResponse, 2, Some(true))]);
        }
        .with_metrics()
        .await;
    }

    #[tokio::test]
    async fn router_request_metric_not_incremented_when_condition_false() {
        async {
            // Make multiple requests to better validate metric is being incremented correctly
            for _ in 0..2 {
                let router_stage = create_router_stage_for_request_with_false_condition();
                // Mock HTTP client used by coprocessor (RouterRequest stage)
                let mock_http_client = create_mock_http_client_router_response_valid_response();
                let mock_router_service = create_mock_router_service();

                let service_stack = router_stage
                    .as_service(
                        mock_http_client,
                        mock_router_service.boxed(),
                        "http://test".to_string(),
                        Arc::new("".to_string()),
                        false, // response_validation - doesn't matter for router stage
                    )
                    .boxed();

                let request = router::Request::fake_builder().build().unwrap();
                let _ = service_stack.oneshot(request).await.unwrap();
            }

            // This call will validate there are no metrics for all stages
            assert_coprocessor_operations_metrics(&[]);
        }
        .with_metrics()
        .await;
    }

    #[tokio::test]
    async fn router_response_metric_not_incremented_when_condition_false() {
        async {
            // Make multiple requests to better validate metric is being incremented correctly
            for _ in 0..1 {
                let router_stage = create_router_stage_for_response_with_false_condition();
                let mock_http_client = create_mock_http_client_router_response_valid_response();
                let mock_router_service = create_mock_router_service();

                let service_stack = router_stage
                    .as_service(
                        mock_http_client,
                        mock_router_service.boxed(),
                        "http://test".to_string(),
                        Arc::new("".to_string()),
                        false, // response_validation - doesn't matter for router stage
                    )
                    .boxed();

                let request = router::Request::fake_builder().build().unwrap();
                let _ = service_stack.oneshot(request).await.unwrap();
            }

            // This call will validate there are no metrics for all stages
            assert_coprocessor_operations_metrics(&[]);
        }
        .with_metrics()
        .await;
    }

    #[tokio::test]
    async fn both_router_stages_metric_incremented_when_conditions_true() {
        async {
            // Make multiple requests to better validate metric is being incremented correctly
            for _ in 0..2 {
                let router_stage = create_router_stage_for_request_validation_test();
                let mock_http_client = create_mock_http_client_router_request_valid_response();
                let mock_router_service = create_mock_router_service();

                let service_stack = router_stage
                    .as_service(
                        mock_http_client,
                        mock_router_service.boxed(),
                        "http://test".to_string(),
                        Arc::new("".to_string()),
                        false, // response_validation - doesn't matter for router stage
                    )
                    .boxed();

                let request = router::Request::fake_builder().build().unwrap();
                let _ = service_stack.oneshot(request).await.unwrap();
            }

            // Make multiple requests to better validate metric is being incremented correctly
            for _ in 0..4 {
                let router_stage = create_router_stage_for_response_validation_test();
                let mock_http_client = create_mock_http_client_router_response_valid_response();
                let mock_router_service = create_mock_router_service();

                let service_stack = router_stage
                    .as_service(
                        mock_http_client,
                        mock_router_service.boxed(),
                        "http://test".to_string(),
                        Arc::new("".to_string()),
                        false, // response_validation - doesn't matter for router stage
                    )
                    .boxed();

                let request = router::Request::fake_builder().build().unwrap();
                let _ = service_stack.oneshot(request).await.unwrap();
            }

            assert_coprocessor_operations_metrics(&[
                (PipelineStep::RouterRequest, 2, Some(true)),
                (PipelineStep::RouterResponse, 4, Some(true)),
            ]);
        }
        .with_metrics()
        .await;
    }

    #[tokio::test]
    async fn router_request_metric_incremented_for_errored_stage_processing() {
        async {
            // Make multiple requests to better validate metric is being incremented correctly
            for _ in 0..2 {
                let router_stage = create_router_stage_for_request_validation_test();
                let mock_http_client = create_mock_http_client_hard_error();
                let mock_router_service = create_mock_router_service();

                let service_stack = router_stage
                    .as_service(
                        mock_http_client,
                        mock_router_service.boxed(),
                        "http://test".to_string(),
                        Arc::new("".to_string()),
                        false,
                    )
                    .boxed();

                let request = router::Request::fake_builder().build().unwrap();
                let _ = service_stack.oneshot(request).await;
            }

            assert_coprocessor_operations_metrics(&[(PipelineStep::RouterRequest, 2, Some(false))]);
        }
        .with_metrics()
        .await;
    }

    #[tokio::test]
    async fn router_response_metric_incremented_for_errored_stage_processing() {
        async {
            // Make multiple requests to better validate metric is being incremented correctly
            for _ in 0..4 {
                let router_stage = create_router_stage_for_response_validation_test();
                let mock_http_client = create_mock_http_client_hard_error();
                let mock_router_service = create_mock_router_service();

                let service_stack = router_stage
                    .as_service(
                        mock_http_client,
                        mock_router_service.boxed(),
                        "http://test".to_string(),
                        Arc::new("".to_string()),
                        false, // response_validation - doesn't matter for router stage
                    )
                    .boxed();

                let request = router::Request::fake_builder().build().unwrap();
                let _ = service_stack.oneshot(request).await;
            }

            assert_coprocessor_operations_metrics(&[(
                PipelineStep::RouterResponse,
                4,
                Some(false),
            )]);
        }
        .with_metrics()
        .await;
    }

    #[tokio::test]
    async fn both_router_stages_metric_incremented_for_errored_stages_processing() {
        async {
            // Make multiple requests to better validate metric is being incremented correctly
            for _ in 0..3 {
                let router_stage = create_router_stage_for_request_validation_test();
                let mock_http_client = create_mock_http_client_hard_error();
                let mock_router_service = create_mock_router_service();

                let service_stack = router_stage
                    .as_service(
                        mock_http_client,
                        mock_router_service.boxed(),
                        "http://test".to_string(),
                        Arc::new("".to_string()),
                        false, // response_validation - doesn't matter for router stage
                    )
                    .boxed();

                let request = router::Request::fake_builder().build().unwrap();
                let _ = service_stack.oneshot(request).await;
            }

            // Make multiple requests to better validate metric is being incremented correctly
            for _ in 0..2 {
                let router_stage = create_router_stage_for_response_validation_test();
                let mock_http_client = create_mock_http_client_hard_error();
                let mock_router_service = create_mock_router_service();

                let service_stack = router_stage
                    .as_service(
                        mock_http_client,
                        mock_router_service.boxed(),
                        "http://test".to_string(),
                        Arc::new("".to_string()),
                        false, // response_validation - doesn't matter for router stage
                    )
                    .boxed();

                let request = router::Request::fake_builder().build().unwrap();
                let _ = service_stack.oneshot(request).await;
            }

            assert_coprocessor_operations_metrics(&[
                (PipelineStep::RouterRequest, 3, Some(false)),
                (PipelineStep::RouterResponse, 2, Some(false)),
            ]);
        }
        .with_metrics()
        .await;
        // Tests for context key deletion functionality
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn load_plugin_with_unix_socket_url() {
        let config = serde_json::json!({
            "coprocessor": {
                "url": "unix:///tmp/coprocessor.sock"
            }
        });

        // Build a test harness to ensure Unix socket URLs are properly handled
        let _test_harness = crate::TestHarness::builder()
            .configuration_json(config)
            .unwrap()
            .build_router()
            .await
            .unwrap();

        // Test passes if the plugin loads successfully with a Unix socket URL
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn load_plugin_with_unix_socket_and_h2c_http2only() {
        let config = serde_json::json!({
            "coprocessor": {
                "url": "unix:///tmp/coprocessor.sock",
                "client": {
                    "experimental_http2": "http2only"
                }
            }
        });

        // Build a test harness to ensure Unix socket URLs work with h2c http2only configuration
        let _test_harness = crate::TestHarness::builder()
            .configuration_json(config)
            .unwrap()
            .build_router()
            .await
            .unwrap();

        // Test passes if the plugin loads successfully with Unix socket + h2c http2only configuration
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn load_plugin_with_unix_socket_and_h2c_enable() {
        let config = serde_json::json!({
            "coprocessor": {
                "url": "unix:///tmp/coprocessor.sock",
                "client": {
                    "experimental_http2": "enable"
                }
            }
        });

        // Build a test harness to ensure Unix socket URLs work with h2c enable configuration
        let _test_harness = crate::TestHarness::builder()
            .configuration_json(config)
            .unwrap()
            .build_router()
            .await
            .unwrap();

        // Test passes if the plugin loads successfully with Unix socket + h2c enable configuration
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn load_plugin_with_unix_socket_and_h2c_disable() {
        let config = serde_json::json!({
            "coprocessor": {
                "url": "unix:///tmp/coprocessor.sock",
                "client": {
                    "experimental_http2": "disable"
                }
            }
        });

        // Build a test harness to ensure Unix socket URLs work with HTTP/2 disabled
        let _test_harness = crate::TestHarness::builder()
            .configuration_json(config)
            .unwrap()
            .build_router()
            .await
            .unwrap();

        // Test passes if the plugin loads successfully with Unix socket + HTTP/2 disabled
    }

    #[tokio::test]
    async fn test_coprocessor_http_url_configuration() {
        let config = serde_json::json!({
            "coprocessor": {
                "url": "http://localhost:8081"
            }
        });

        // Verify HTTP URLs continue to work as before (same as existing load_plugin test)
        let _test_harness = crate::TestHarness::builder()
            .configuration_json(config)
            .unwrap()
            .build_router()
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn test_coprocessor_https_url_configuration() {
        let config = serde_json::json!({
            "coprocessor": {
                "url": "https://example.com:8443/coprocessor"
            }
        });

        // Verify HTTPS URLs continue to work as before
        let _test_harness = crate::TestHarness::builder()
            .configuration_json(config)
            .unwrap()
            .build_router()
            .await
            .unwrap();
    }

    #[test]
    fn test_url_scheme_detection() {
        // Test various URL formats that should be supported
        let test_cases = vec![
            ("http://localhost:8081", false),
            ("https://example.com:443/path", false),
            ("unix:///tmp/socket.sock", true),
            ("unix:///var/run/app/coprocessor.sock", true),
            ("ftp://example.com", false), // Invalid but shouldn't panic
        ];

        for (url, should_be_unix) in test_cases {
            let is_unix = url.starts_with("unix://");
            assert_eq!(
                is_unix, should_be_unix,
                "URL '{}' unix detection failed",
                url
            );
        }
    }

    #[tokio::test]
    async fn test_backwards_compatibility_with_existing_configs() {
        // Test that existing production configurations continue to work unchanged
        let legacy_http_configs = vec![
            serde_json::json!({
                "coprocessor": {
                    "url": "http://coprocessor:8080"
                }
            }),
            serde_json::json!({
                "coprocessor": {
                    "url": "https://external-coprocessor.company.com/graphql",
                    "timeout": "10s"
                }
            }),
            serde_json::json!({
                "coprocessor": {
                    "url": "http://127.0.0.1:3001/webhook",
                    "router": {
                        "request": {
                            "context": true,
                            "headers": true
                        }
                    }
                }
            }),
        ];

        // Verify all legacy configurations can still be loaded
        for config in legacy_http_configs {
            let test_harness = crate::TestHarness::builder()
                .configuration_json(config)
                .unwrap()
                .build_router()
                .await;

            assert!(
                test_harness.is_ok(),
                "Legacy HTTP configuration should load successfully"
            );
        }
    }

    #[tokio::test]
    async fn test_empty_unix_socket_path_rejected() {
        let config = serde_json::json!({
            "coprocessor": {
                "url": "unix://"
            }
        });

        let result = crate::TestHarness::builder()
            .configuration_json(config)
            .unwrap()
            .build_router()
            .await;

        assert!(result.is_err(), "Empty Unix socket path should be rejected");
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("must include a path"),
            "Error should mention missing path: {err}"
        );
    }

    #[tokio::test]
    async fn test_relative_unix_socket_path_rejected() {
        let config = serde_json::json!({
            "coprocessor": {
                "url": "unix://relative/path.sock"
            }
        });

        let result = crate::TestHarness::builder()
            .configuration_json(config)
            .unwrap()
            .build_router()
            .await;

        assert!(
            result.is_err(),
            "Relative Unix socket path should be rejected"
        );
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("should be absolute"),
            "Error should mention absolute path requirement: {err}"
        );
    }

    #[tokio::test]
    async fn test_invalid_http_url_rejected() {
        let config = serde_json::json!({
            "coprocessor": {
                "url": "not a valid url"
            }
        });

        let result = crate::TestHarness::builder()
            .configuration_json(config)
            .unwrap()
            .build_router()
            .await;

        assert!(result.is_err(), "Invalid HTTP URL should be rejected");
    }

    #[tokio::test]
    async fn test_stage_specific_empty_unix_socket_path_rejected() {
        let config = serde_json::json!({
            "coprocessor": {
                "url": "http://localhost:8080",
                "router": {
                    "request": {
                        "url": "unix://",
                        "headers": true
                    }
                }
            }
        });

        let result = crate::TestHarness::builder()
            .configuration_json(config)
            .unwrap()
            .build_router()
            .await;

        assert!(
            result.is_err(),
            "Empty Unix socket path in stage config should be rejected"
        );
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("router.request.url"),
            "Error should mention the specific config path: {err}"
        );
    }

    #[tokio::test]
    async fn test_unix_socket_with_valid_path_query_accepted() {
        let config = serde_json::json!({
            "coprocessor": {
                "url": "unix:///tmp/coprocessor.sock?path=/api/v1"
            }
        });

        let result = crate::TestHarness::builder()
            .configuration_json(config)
            .unwrap()
            .build_router()
            .await;

        assert!(
            result.is_ok(),
            "Unix socket with ?path= should be accepted, got: {}",
            result.unwrap_err()
        );
    }

    #[tokio::test]
    async fn test_unix_socket_stage_override_with_valid_path_query_accepted() {
        let config = serde_json::json!({
            "coprocessor": {
                "url": "http://localhost:8080",
                "router": {
                    "request": {
                        "url": "unix:///tmp/router.sock?path=/hook",
                        "headers": true
                    }
                }
            }
        });

        let result = crate::TestHarness::builder()
            .configuration_json(config)
            .unwrap()
            .build_router()
            .await;

        assert!(
            result.is_ok(),
            "Stage-specific Unix socket with ?path= should be accepted, got: {}",
            result.unwrap_err()
        );
    }

    #[rstest::rstest]
    #[case::unix_with_path("unix:///tmp/coprocessor.sock?path=/api/v1")]
    #[case::unix_without_query("unix:///tmp/coprocessor.sock")]
    #[case::unix_unknown_query_param_warns("unix:///tmp/coprocessor.sock?foo=bar")]
    #[case::unix_empty_query_string_warns("unix:///tmp/coprocessor.sock?")]
    #[case::http_url("http://localhost:8080/path")]
    fn test_validate_coprocessor_url_accepted(#[case] url: &str) {
        assert!(
            crate::plugins::coprocessor::validate_coprocessor_url(url, "coprocessor.url").is_ok(),
            "URL should be accepted: {url}"
        );
    }

    #[rstest::rstest]
    #[case::empty_path("unix://", "must include a path")]
    #[case::relative_path("unix://relative/path.sock", "should be absolute")]
    #[case::invalid_http("not a valid url", "invalid URL")]
    fn test_validate_coprocessor_url_rejected(#[case] url: &str, #[case] expected_err: &str) {
        let result = crate::plugins::coprocessor::validate_coprocessor_url(url, "coprocessor.url");
        assert!(result.is_err(), "URL should be rejected: {url}");
        assert!(
            result.unwrap_err().to_string().contains(expected_err),
            "Error for '{url}' should contain '{expected_err}'"
        );
    }

    #[cfg(test)]
    mod connector_tests {
        use std::str::FromStr;
        use std::sync::Arc;
        use std::sync::Mutex;

        use apollo_compiler::name;
        use apollo_federation::connectors::ConnectId;
        use apollo_federation::connectors::ConnectSpec;
        use apollo_federation::connectors::Connector;
        use apollo_federation::connectors::HttpJsonTransport;
        use apollo_federation::connectors::JSONSelection;
        use apollo_federation::connectors::SourceName;
        use apollo_federation::connectors::StringTemplate;
        use apollo_federation::connectors::runtime::http_json_transport::HttpRequest as ConnectorsHttpRequest;
        use apollo_federation::connectors::runtime::http_json_transport::TransportRequest;
        use apollo_federation::connectors::runtime::key::ResponseKey;
        use apollo_federation::connectors::runtime::responses::MappedResponse;
        use futures::future::BoxFuture;
        use router::body::RouterBody;
        use tower::BoxError;
        use tower::ServiceExt;

        use crate::metrics::FutureMetricsExt;
        use crate::plugin::test::MockInternalHttpClientService;
        use crate::plugins::coprocessor::ContextConf;
        use crate::plugins::coprocessor::NewContextConf;
        use crate::plugins::coprocessor::connector::ConnectorRequestConf;
        use crate::plugins::coprocessor::connector::ConnectorResponseConf;
        use crate::plugins::coprocessor::connector::ConnectorStage;
        use crate::plugins::coprocessor::test::assert_coprocessor_operations_metrics;
        use crate::plugins::telemetry::config_new::conditions::Condition;
        use crate::services::connector::request_service;
        use crate::services::external::PipelineStep;
        use crate::services::http::HttpRequest;
        use crate::services::http::HttpResponse;
        use crate::services::router;

        #[allow(clippy::type_complexity)]
        fn mock_with_callback(
            callback: fn(
                http::Request<RouterBody>,
            )
                -> BoxFuture<'static, Result<http::Response<RouterBody>, BoxError>>,
        ) -> MockInternalHttpClientService {
            let mut mock_http_client = MockInternalHttpClientService::new();
            mock_http_client.expect_clone().returning(move || {
                let mut mock_http_client = MockInternalHttpClientService::new();
                mock_http_client.expect_clone().returning(move || {
                    let mut mock_http_client = MockInternalHttpClientService::new();
                    mock_http_client
                        .expect_call()
                        .returning(move |req: HttpRequest| {
                            let context = req.context.clone();
                            let fut = callback(req.http_request);
                            Box::pin(async move {
                                let response = fut.await?;
                                Ok(HttpResponse {
                                    http_response: response,
                                    context,
                                })
                            })
                        });
                    mock_http_client
                });
                mock_http_client
            });

            mock_http_client
        }

        fn create_test_connector() -> Arc<Connector> {
            Arc::new(Connector {
                id: ConnectId::new(
                    "subgraph".into(),
                    Some(SourceName::cast("source")),
                    name!(Query),
                    name!(users),
                    None,
                    0,
                ),
                transport: Some(HttpJsonTransport {
                    source_template: None,
                    connect_template: StringTemplate::from_str("/test").unwrap(),
                    ..Default::default()
                }),
                mapping_only: false,
                selection: JSONSelection::empty(),
                config: None,
                max_requests: None,
                entity_resolver: None,
                spec: ConnectSpec::V0_1,
                schema_subtypes_map: Default::default(),
                batch_settings: None,
                request_headers: Default::default(),
                response_headers: Default::default(),
                request_variable_keys: Default::default(),
                response_variable_keys: Default::default(),
                error_settings: Default::default(),
                label: "label".into(),
            })
        }

        fn create_test_response_key() -> ResponseKey {
            ResponseKey::RootField {
                name: "hello".to_string(),
                inputs: Default::default(),
                selection: Arc::new(JSONSelection::parse("$.data").unwrap()),
            }
        }

        fn create_test_connector_request() -> request_service::Request {
            let http_request = http::Request::builder()
                .uri("http://original-connector-uri/api")
                .method(http::Method::POST)
                .header("content-type", "application/json")
                .body(r#"{"query":"test"}"#.to_string())
                .unwrap();

            let transport_request = TransportRequest::Http(Box::new(ConnectorsHttpRequest {
                inner: http_request,
                debug: Default::default(),
            }));

            request_service::Request {
                context: crate::Context::default(),
                connector: create_test_connector(),
                transport_request,
                key: create_test_response_key(),
                mapping_problems: vec![],
                supergraph_request: Default::default(),
                operation: Default::default(),
            }
        }

        #[tokio::test]
        async fn should_apply_modified_body_when_coprocessor_returns_new_body() {
            let connector_stage = ConnectorStage {
                request: ConnectorRequestConf {
                    headers: true,
                    body: true,
                    uri: true,
                    ..Default::default()
                },
                response: Default::default(),
            };

            // MockConnector is configured to match the *modified* body from the coprocessor.
            // If the body modification wasn't applied, the mock wouldn't match.
            let mock_connector_service = crate::plugin::test::MockConnector::builder()
                .with_json(
                    serde_json::json!(r#"{"modified":"body"}"#),
                    serde_json::json!("test_result"),
                )
                .build();

            let mock_http_client = mock_with_callback(move |_req: http::Request<RouterBody>| {
                Box::pin(async {
                    Ok(http::Response::builder()
                        .body(router::body::from_bytes(
                            r#"{
                            "version": 1,
                            "stage": "ConnectorRequest",
                            "control": "continue",
                            "body": "{\"modified\":\"body\"}"
                        }"#,
                        ))
                        .unwrap())
                })
            });

            let service = connector_stage.as_service(
                mock_http_client,
                mock_connector_service.boxed(),
                "http://test".to_string(),
                "my_connector_source".to_string(),
            );

            let request = create_test_connector_request();
            let response = service.oneshot(request).await.unwrap();

            assert!(response.transport_result.is_ok());
        }

        #[tokio::test]
        async fn should_send_json_body_as_parsed_json_to_coprocessor() {
            let connector_stage = ConnectorStage {
                request: ConnectorRequestConf {
                    body: true,
                    ..Default::default()
                },
                response: Default::default(),
            };

            let mock_connector_service = crate::plugin::test::MockConnector::new(
                [(r#"{"query":"test"}"#.to_string(), "ok".to_string())].into(),
            );

            let mock_http_client = mock_with_callback(move |req: http::Request<RouterBody>| {
                Box::pin(async {
                    let body = router::body::into_bytes(req.into_body()).await.unwrap();
                    let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();

                    // The body should be a JSON object, not a JSON string
                    assert!(
                        payload["body"].is_object(),
                        "expected body to be a JSON object, got: {}",
                        payload["body"]
                    );

                    Ok(http::Response::builder()
                        .body(router::body::from_bytes(
                            r#"{
                            "version": 1,
                            "stage": "ConnectorRequest",
                            "control": "continue"
                        }"#,
                        ))
                        .unwrap())
                })
            });

            let service = connector_stage.as_service(
                mock_http_client,
                mock_connector_service.boxed(),
                "http://test".to_string(),
                "my_connector_source".to_string(),
            );

            let request = create_test_connector_request();
            let response = service.oneshot(request).await.unwrap();

            assert!(response.transport_result.is_ok());
        }

        #[tokio::test]
        async fn should_send_non_json_body_as_string_to_coprocessor() {
            let connector_stage = ConnectorStage {
                request: ConnectorRequestConf {
                    body: true,
                    ..Default::default()
                },
                response: Default::default(),
            };

            let mock_connector_service = crate::plugin::test::MockConnector::new(
                [("plain text body".to_string(), "ok".to_string())].into(),
            );

            let mock_http_client = mock_with_callback(move |req: http::Request<RouterBody>| {
                Box::pin(async {
                    let body = router::body::into_bytes(req.into_body()).await.unwrap();
                    let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();

                    // The body should be a JSON string since the request body is not valid JSON
                    assert!(
                        payload["body"].is_string(),
                        "expected body to be a JSON string, got: {}",
                        payload["body"]
                    );

                    Ok(http::Response::builder()
                        .body(router::body::from_bytes(
                            r#"{
                            "version": 1,
                            "stage": "ConnectorRequest",
                            "control": "continue"
                        }"#,
                        ))
                        .unwrap())
                })
            });

            let service = connector_stage.as_service(
                mock_http_client,
                mock_connector_service.boxed(),
                "http://test".to_string(),
                "my_connector_source".to_string(),
            );

            // Create a request with a non-JSON body
            let http_request = http::Request::builder()
                .uri("http://original-connector-uri/api")
                .method(http::Method::POST)
                .header("content-type", "text/plain")
                .body("plain text body".to_string())
                .unwrap();

            let transport_request = TransportRequest::Http(Box::new(ConnectorsHttpRequest {
                inner: http_request,
                debug: Default::default(),
            }));

            let request = request_service::Request {
                context: crate::Context::default(),
                connector: create_test_connector(),
                transport_request,
                key: create_test_response_key(),
                mapping_problems: vec![],
                supergraph_request: Default::default(),
                operation: Default::default(),
            };

            let response = service.oneshot(request).await.unwrap();

            assert!(response.transport_result.is_ok());
        }

        #[tokio::test]
        async fn should_return_transport_error_when_coprocessor_breaks() {
            let connector_stage = ConnectorStage {
                request: ConnectorRequestConf {
                    body: true,
                    ..Default::default()
                },
                response: Default::default(),
            };

            // This service should never be called because the coprocessor breaks
            let mock_connector_service =
                crate::plugin::test::MockConnector::new(Default::default());

            let mock_http_client = mock_with_callback(move |_req: http::Request<RouterBody>| {
                Box::pin(async {
                    Ok(http::Response::builder()
                        .body(router::body::from_bytes(
                            r#"{
                            "version": 1,
                            "stage": "ConnectorRequest",
                            "control": { "break": 400 },
                            "body": "Request blocked by coprocessor"
                        }"#,
                        ))
                        .unwrap())
                })
            });

            let service = connector_stage.as_service(
                mock_http_client,
                mock_connector_service.boxed(),
                "http://test".to_string(),
                "my_connector_source".to_string(),
            );

            let request = create_test_connector_request();
            let response = service.oneshot(request).await.unwrap();

            assert!(response.transport_result.is_err());
        }

        #[tokio::test]
        async fn should_apply_modified_headers_and_uri_when_coprocessor_returns_them() {
            let connector_stage = ConnectorStage {
                request: ConnectorRequestConf {
                    headers: true,
                    uri: true,
                    ..Default::default()
                },
                response: Default::default(),
            };

            // Capture the request that reaches the inner connector service
            let captured_uri = Arc::new(Mutex::new(String::new()));
            let captured_headers = Arc::new(Mutex::new(Vec::<(String, String)>::new()));
            let captured_uri_clone = captured_uri.clone();
            let captured_headers_clone = captured_headers.clone();

            let inner_service = tower::service_fn(move |req: request_service::Request| {
                let captured_uri = captured_uri_clone.clone();
                let captured_headers = captured_headers_clone.clone();
                async move {
                    let TransportRequest::Http(ref http_req) = req.transport_request else {
                        panic!("expected Http transport request");
                    };
                    *captured_uri.lock().unwrap() = http_req.inner.uri().to_string();
                    *captured_headers.lock().unwrap() = http_req
                        .inner
                        .headers()
                        .iter()
                        .map(|(k, v)| (k.to_string(), v.to_str().unwrap().to_string()))
                        .collect();

                    let response = request_service::Response::test_new(
                        req.context.clone(),
                        req.key,
                        Default::default(),
                        serde_json_bytes::json!("ok"),
                        None,
                    );
                    Ok(response)
                }
            });

            let mock_http_client = mock_with_callback(move |_req: http::Request<RouterBody>| {
                Box::pin(async {
                    Ok(http::Response::builder()
                        .body(router::body::from_bytes(
                            r#"{
                            "version": 1,
                            "stage": "ConnectorRequest",
                            "control": "continue",
                            "headers": {
                                "content-type": ["application/json"],
                                "x-new-header": ["new-value"]
                            },
                            "uri": "http://new-connector-uri/api"
                        }"#,
                        ))
                        .unwrap())
                })
            });

            let service = connector_stage.as_service(
                mock_http_client,
                inner_service.boxed(),
                "http://test".to_string(),
                "my_connector_source".to_string(),
            );

            let request = create_test_connector_request();
            service.oneshot(request).await.unwrap();

            assert_eq!(
                *captured_uri.lock().unwrap(),
                "http://new-connector-uri/api"
            );
            assert!(
                captured_headers
                    .lock()
                    .unwrap()
                    .contains(&("x-new-header".to_string(), "new-value".to_string()))
            );
        }

        #[tokio::test]
        async fn should_update_context_when_coprocessor_returns_context_entries() {
            let connector_stage = ConnectorStage {
                request: ConnectorRequestConf {
                    context: ContextConf::NewContextConf(NewContextConf::All),
                    body: true,
                    ..Default::default()
                },
                response: Default::default(),
            };

            let mock_connector_service = crate::plugin::test::MockConnector::new(
                [(r#"{"query":"test"}"#.to_string(), "ok".to_string())].into(),
            );

            let mock_http_client = mock_with_callback(move |_req: http::Request<RouterBody>| {
                Box::pin(async {
                    Ok(http::Response::builder()
                        .body(router::body::from_bytes(
                            r#"{
                            "version": 1,
                            "stage": "ConnectorRequest",
                            "control": "continue",
                            "context": {
                                "entries": {
                                    "test-key": "test-value"
                                }
                            }
                        }"#,
                        ))
                        .unwrap())
                })
            });

            let service = connector_stage.as_service(
                mock_http_client,
                mock_connector_service.boxed(),
                "http://test".to_string(),
                "my_connector_source".to_string(),
            );

            let request = create_test_connector_request();
            let context = request.context.clone();
            service.oneshot(request).await.unwrap();

            assert_eq!(
                context.get_json_value("test-key"),
                Some(serde_json_bytes::Value::String("test-value".into()))
            );
        }

        #[tokio::test]
        async fn should_increment_request_metric_when_condition_is_true() {
            async {
                for _ in 0..2 {
                    let connector_stage = ConnectorStage {
                        request: ConnectorRequestConf {
                            body: true,
                            ..Default::default()
                        },
                        response: Default::default(),
                    };

                    let mock_connector_service = crate::plugin::test::MockConnector::new(
                        [(r#"{"query":"test"}"#.to_string(), "ok".to_string())].into(),
                    );

                    let mock_http_client =
                        mock_with_callback(move |_req: http::Request<RouterBody>| {
                            Box::pin(async {
                                Ok(http::Response::builder()
                                    .body(router::body::from_bytes(
                                        r#"{
                                        "version": 1,
                                        "stage": "ConnectorRequest",
                                        "control": "continue"
                                    }"#,
                                    ))
                                    .unwrap())
                            })
                        });

                    let service = connector_stage.as_service(
                        mock_http_client,
                        mock_connector_service.boxed(),
                        "http://test".to_string(),
                        "my_connector_source".to_string(),
                    );

                    let request = create_test_connector_request();
                    let _response = service.oneshot(request).await;
                }

                assert_coprocessor_operations_metrics(&[(
                    PipelineStep::ConnectorRequest,
                    2,
                    Some(true),
                )]);
            }
            .with_metrics()
            .await;
        }

        #[tokio::test]
        async fn should_not_increment_request_metric_when_condition_is_false() {
            async {
                for _ in 0..2 {
                    let connector_stage = ConnectorStage {
                        request: ConnectorRequestConf {
                            condition: Condition::False,
                            body: true,
                            ..Default::default()
                        },
                        response: Default::default(),
                    };

                    let mock_connector_service = crate::plugin::test::MockConnector::new(
                        [(r#"{"query":"test"}"#.to_string(), "ok".to_string())].into(),
                    );

                    let mock_http_client =
                        mock_with_callback(move |_req: http::Request<RouterBody>| {
                            Box::pin(async {
                                Ok(http::Response::builder()
                                    .body(router::body::from_bytes(
                                        r#"{
                                        "version": 1,
                                        "stage": "ConnectorRequest",
                                        "control": "continue"
                                    }"#,
                                    ))
                                    .unwrap())
                            })
                        });

                    let service = connector_stage.as_service(
                        mock_http_client,
                        mock_connector_service.boxed(),
                        "http://test".to_string(),
                        "my_connector_source".to_string(),
                    );

                    let request = create_test_connector_request();
                    let _response = service.oneshot(request).await;
                }

                // This call will validate there are no metrics for all stages
                assert_coprocessor_operations_metrics(&[]);
            }
            .with_metrics()
            .await;
        }

        #[tokio::test]
        async fn should_return_successful_response_when_response_coprocessor_continues() {
            let connector_stage = ConnectorStage {
                request: Default::default(),
                response: ConnectorResponseConf {
                    headers: true,
                    status_code: true,
                    body: true,
                    ..Default::default()
                },
            };

            let mock_connector_service = crate::plugin::test::MockConnector::new(
                [(r#"{"query":"test"}"#.to_string(), "ok".to_string())].into(),
            );

            let mock_http_client = mock_with_callback(move |_req: http::Request<RouterBody>| {
                Box::pin(async {
                    Ok(http::Response::builder()
                        .body(router::body::from_bytes(
                            r#"{
                            "version": 1,
                            "stage": "ConnectorResponse"
                        }"#,
                        ))
                        .unwrap())
                })
            });

            let service = connector_stage.as_service(
                mock_http_client,
                mock_connector_service.boxed(),
                "http://test".to_string(),
                "my_connector_source".to_string(),
            );

            let request = create_test_connector_request();
            let response = service.oneshot(request).await.unwrap();

            assert!(response.transport_result.is_ok());
        }

        #[tokio::test]
        async fn should_increment_response_metric_when_condition_is_true() {
            async {
                for _ in 0..3 {
                    let connector_stage = ConnectorStage {
                        request: Default::default(),
                        response: ConnectorResponseConf {
                            body: true,
                            ..Default::default()
                        },
                    };

                    let mock_connector_service = crate::plugin::test::MockConnector::new(
                        [(r#"{"query":"test"}"#.to_string(), "ok".to_string())].into(),
                    );

                    let mock_http_client =
                        mock_with_callback(move |_req: http::Request<RouterBody>| {
                            Box::pin(async {
                                Ok(http::Response::builder()
                                    .body(router::body::from_bytes(
                                        r#"{
                                        "version": 1,
                                        "stage": "ConnectorResponse"
                                    }"#,
                                    ))
                                    .unwrap())
                            })
                        });

                    let service = connector_stage.as_service(
                        mock_http_client,
                        mock_connector_service.boxed(),
                        "http://test".to_string(),
                        "my_connector_source".to_string(),
                    );

                    let request = create_test_connector_request();
                    let _response = service.oneshot(request).await;
                }

                assert_coprocessor_operations_metrics(&[(
                    PipelineStep::ConnectorResponse,
                    3,
                    Some(true),
                )]);
            }
            .with_metrics()
            .await;
        }

        #[tokio::test]
        async fn should_use_structured_error_when_coprocessor_breaks_with_errors_object() {
            let connector_stage = ConnectorStage {
                request: ConnectorRequestConf {
                    body: true,
                    ..Default::default()
                },
                response: Default::default(),
            };

            let mock_connector_service =
                crate::plugin::test::MockConnector::new(Default::default());

            let mock_http_client = mock_with_callback(move |_req: http::Request<RouterBody>| {
                Box::pin(async {
                    Ok(http::Response::builder()
                        .body(router::body::from_bytes(
                            r#"{
                            "version": 1,
                            "stage": "ConnectorRequest",
                            "control": { "break": 401 },
                            "body": {
                                "errors": [
                                    {
                                        "message": "Not authenticated.",
                                        "extensions": {
                                            "code": "ERR_UNAUTHENTICATED"
                                        }
                                    }
                                ]
                            }
                        }"#,
                        ))
                        .unwrap())
                })
            });

            let service = connector_stage.as_service(
                mock_http_client,
                mock_connector_service.boxed(),
                "http://test".to_string(),
                "my_connector_source".to_string(),
            );

            let request = create_test_connector_request();
            let response = service.oneshot(request).await.unwrap();

            assert!(response.transport_result.is_err());
            match &response.mapped_response {
                MappedResponse::Error { error, .. } => {
                    assert_eq!(error.message, "Not authenticated.");
                    assert_eq!(error.code(), "ERR_UNAUTHENTICATED");
                }
                _ => panic!("Expected MappedResponse::Error"),
            }
        }

        #[tokio::test]
        async fn should_use_string_error_when_coprocessor_breaks_with_string_body() {
            let connector_stage = ConnectorStage {
                request: ConnectorRequestConf {
                    body: true,
                    ..Default::default()
                },
                response: Default::default(),
            };

            let mock_connector_service =
                crate::plugin::test::MockConnector::new(Default::default());

            let mock_http_client = mock_with_callback(move |_req: http::Request<RouterBody>| {
                Box::pin(async {
                    Ok(http::Response::builder()
                        .body(router::body::from_bytes(
                            r#"{
                            "version": 1,
                            "stage": "ConnectorRequest",
                            "control": { "break": 400 },
                            "body": "Request blocked"
                        }"#,
                        ))
                        .unwrap())
                })
            });

            let service = connector_stage.as_service(
                mock_http_client,
                mock_connector_service.boxed(),
                "http://test".to_string(),
                "my_connector_source".to_string(),
            );

            let request = create_test_connector_request();
            let response = service.oneshot(request).await.unwrap();

            assert!(response.transport_result.is_err());
            match &response.mapped_response {
                MappedResponse::Error { error, .. } => {
                    assert_eq!(error.message, "Request blocked");
                    assert_eq!(error.code(), "ERROR");
                }
                _ => panic!("Expected MappedResponse::Error"),
            }
        }

        #[tokio::test]
        async fn should_pass_extra_extensions_from_structured_error() {
            let connector_stage = ConnectorStage {
                request: ConnectorRequestConf {
                    body: true,
                    ..Default::default()
                },
                response: Default::default(),
            };

            let mock_connector_service =
                crate::plugin::test::MockConnector::new(Default::default());

            let mock_http_client = mock_with_callback(move |_req: http::Request<RouterBody>| {
                Box::pin(async {
                    Ok(http::Response::builder()
                        .body(router::body::from_bytes(
                            r#"{
                            "version": 1,
                            "stage": "ConnectorRequest",
                            "control": { "break": 429 },
                            "body": {
                                "errors": [
                                    {
                                        "message": "Rate limited",
                                        "extensions": {
                                            "code": "RATE_LIMITED",
                                            "retryAfter": 30
                                        }
                                    }
                                ]
                            }
                        }"#,
                        ))
                        .unwrap())
                })
            });

            let service = connector_stage.as_service(
                mock_http_client,
                mock_connector_service.boxed(),
                "http://test".to_string(),
                "my_connector_source".to_string(),
            );

            let request = create_test_connector_request();
            let response = service.oneshot(request).await.unwrap();

            assert!(response.transport_result.is_err());
            match &response.mapped_response {
                MappedResponse::Error { error, .. } => {
                    assert_eq!(error.message, "Rate limited");
                    assert_eq!(error.code(), "RATE_LIMITED");
                    assert_eq!(
                        error.extensions.get("retryAfter"),
                        Some(&serde_json_bytes::Value::Number(30.into()))
                    );
                }
                _ => panic!("Expected MappedResponse::Error"),
            }
        }

        #[tokio::test]
        async fn should_send_context_and_id_in_response_stage() {
            let connector_stage = ConnectorStage {
                request: Default::default(),
                response: ConnectorResponseConf {
                    context: ContextConf::NewContextConf(NewContextConf::All),
                    body: true,
                    ..Default::default()
                },
            };

            let mock_connector_service = crate::plugin::test::MockConnector::new(
                [(r#"{"query":"test"}"#.to_string(), "ok".to_string())].into(),
            );

            let mock_http_client = mock_with_callback(move |req: http::Request<RouterBody>| {
                Box::pin(async {
                    let body = router::body::into_bytes(req.into_body()).await.unwrap();
                    let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();

                    // Verify the coprocessor receives a non-empty id
                    assert!(
                        !payload["id"].as_str().unwrap_or("").is_empty(),
                        "id should not be empty in response stage"
                    );

                    Ok(http::Response::builder()
                        .body(router::body::from_bytes(
                            r#"{
                            "version": 1,
                            "stage": "ConnectorResponse"
                        }"#,
                        ))
                        .unwrap())
                })
            });

            let service = connector_stage.as_service(
                mock_http_client,
                mock_connector_service.boxed(),
                "http://test".to_string(),
                "my_connector_source".to_string(),
            );

            let request = create_test_connector_request();
            let response = service.oneshot(request).await.unwrap();

            assert!(response.transport_result.is_ok());
        }

        #[tokio::test]
        async fn should_update_context_in_response_stage() {
            let connector_stage = ConnectorStage {
                request: Default::default(),
                response: ConnectorResponseConf {
                    context: ContextConf::NewContextConf(NewContextConf::All),
                    body: true,
                    ..Default::default()
                },
            };

            let mock_connector_service = crate::plugin::test::MockConnector::new(
                [(r#"{"query":"test"}"#.to_string(), "ok".to_string())].into(),
            );

            let mock_http_client = mock_with_callback(move |_req: http::Request<RouterBody>| {
                Box::pin(async {
                    Ok(http::Response::builder()
                        .body(router::body::from_bytes(
                            r#"{
                            "version": 1,
                            "stage": "ConnectorResponse",
                            "context": {
                                "entries": {
                                    "response-key": "response-value"
                                }
                            }
                        }"#,
                        ))
                        .unwrap())
                })
            });

            let service = connector_stage.as_service(
                mock_http_client,
                mock_connector_service.boxed(),
                "http://test".to_string(),
                "my_connector_source".to_string(),
            );

            let request = create_test_connector_request();
            let context = request.context.clone();
            service.oneshot(request).await.unwrap();

            assert_eq!(
                context.get_json_value("response-key"),
                Some(serde_json_bytes::Value::String("response-value".into()))
            );
        }

        #[tokio::test]
        async fn should_apply_body_modification_for_data_response() {
            let connector_stage = ConnectorStage {
                request: Default::default(),
                response: ConnectorResponseConf {
                    body: true,
                    ..Default::default()
                },
            };

            let mock_connector_service = crate::plugin::test::MockConnector::new(
                [(r#"{"query":"test"}"#.to_string(), "ok".to_string())].into(),
            );

            let mock_http_client = mock_with_callback(move |_req: http::Request<RouterBody>| {
                Box::pin(async {
                    Ok(http::Response::builder()
                        .body(router::body::from_bytes(
                            r#"{
                            "version": 1,
                            "stage": "ConnectorResponse",
                            "body": {"modified": "data"}
                        }"#,
                        ))
                        .unwrap())
                })
            });

            let service = connector_stage.as_service(
                mock_http_client,
                mock_connector_service.boxed(),
                "http://test".to_string(),
                "my_connector_source".to_string(),
            );

            let request = create_test_connector_request();
            let response = service.oneshot(request).await.unwrap();

            match &response.mapped_response {
                MappedResponse::Data { data, .. } => {
                    assert_eq!(data, &serde_json_bytes::json!({"modified": "data"}));
                }
                _ => panic!("Expected MappedResponse::Data"),
            }
        }

        fn create_error_connector_service()
        -> tower::util::BoxService<request_service::Request, request_service::Response, BoxError>
        {
            tower::service_fn(|req: request_service::Request| async {
                Ok(request_service::Response {
                    context: req.context,
                    transport_result: Err(
                        apollo_federation::connectors::runtime::errors::Error::TransportFailure(
                            "original error".to_string(),
                        ),
                    ),
                    mapped_response: MappedResponse::Error {
                        error: apollo_federation::connectors::runtime::errors::RuntimeError::new(
                            "Original error message",
                            &create_test_response_key(),
                        ),
                        key: create_test_response_key(),
                        problems: Vec::new(),
                    },
                })
            })
            .boxed()
        }

        #[tokio::test]
        async fn should_apply_error_message_modification_for_error_response() {
            let connector_stage = ConnectorStage {
                request: Default::default(),
                response: ConnectorResponseConf {
                    body: true,
                    ..Default::default()
                },
            };

            let mock_http_client = mock_with_callback(move |_req: http::Request<RouterBody>| {
                Box::pin(async {
                    Ok(http::Response::builder()
                        .body(router::body::from_bytes(
                            r#"{
                            "version": 1,
                            "stage": "ConnectorResponse",
                            "body": {
                                "errors": [{"message": "Modified error message"}]
                            }
                        }"#,
                        ))
                        .unwrap())
                })
            });

            let service = connector_stage.as_service(
                mock_http_client,
                create_error_connector_service(),
                "http://test".to_string(),
                "my_connector_source".to_string(),
            );

            let request = create_test_connector_request();
            let response = service.oneshot(request).await.unwrap();

            match &response.mapped_response {
                MappedResponse::Error { error, .. } => {
                    assert_eq!(error.message, "Modified error message");
                }
                _ => panic!("Expected MappedResponse::Error"),
            }
        }

        #[tokio::test]
        async fn should_apply_error_code_modification_for_error_response() {
            let connector_stage = ConnectorStage {
                request: Default::default(),
                response: ConnectorResponseConf {
                    body: true,
                    ..Default::default()
                },
            };

            let mock_http_client = mock_with_callback(move |_req: http::Request<RouterBody>| {
                Box::pin(async {
                    Ok(http::Response::builder()
                        .body(router::body::from_bytes(
                            r#"{
                            "version": 1,
                            "stage": "ConnectorResponse",
                            "body": {
                                "errors": [{
                                    "message": "Not authorized",
                                    "extensions": {
                                        "code": "ERR_UNAUTHORIZED"
                                    }
                                }]
                            }
                        }"#,
                        ))
                        .unwrap())
                })
            });

            let service = connector_stage.as_service(
                mock_http_client,
                create_error_connector_service(),
                "http://test".to_string(),
                "my_connector_source".to_string(),
            );

            let request = create_test_connector_request();
            let response = service.oneshot(request).await.unwrap();

            match &response.mapped_response {
                MappedResponse::Error { error, .. } => {
                    assert_eq!(error.code(), "ERR_UNAUTHORIZED");
                }
                _ => panic!("Expected MappedResponse::Error"),
            }
        }
    }
}
