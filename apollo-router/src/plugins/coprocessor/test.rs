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
    use crate::graphql::Response;
    use crate::json_ext::Object;
    use crate::json_ext::Value;
    use crate::plugin::test::MockInternalHttpClientService;
    use crate::plugin::test::MockRouterService;
    use crate::plugin::test::MockSubgraphService;
    use crate::plugin::test::MockSupergraphService;
    use crate::plugins::coprocessor::handle_graphql_response;
    use crate::plugins::coprocessor::is_graphql_response_minimally_valid;
    use crate::plugins::coprocessor::supergraph::SupergraphResponseConf;
    use crate::plugins::coprocessor::supergraph::SupergraphStage;
    use crate::plugins::coprocessor::was_incoming_payload_valid;
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
    fn create_router_stage_for_validation_test() -> RouterStage {
        RouterStage {
            request: RouterRequestConf {
                body: true,
                ..Default::default()
            },
            ..Default::default()
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

    #[tokio::test]
    async fn external_plugin_router_request_validation_disabled_empty() {
        let service_stack = create_router_stage_for_validation_test()
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
        let service_stack = create_router_stage_for_validation_test()
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

        let actual = externalize_header_map(&external_form).expect("externalized header map");

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
                mock_http_client.expect_call().returning(callback);
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
                    mock_http_client.expect_call().returning(callback);
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
}
