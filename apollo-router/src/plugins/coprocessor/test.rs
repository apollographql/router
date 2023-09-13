#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Arc;

    use futures::future::BoxFuture;
    use http::header::ACCEPT;
    use http::header::CONTENT_TYPE;
    use http::HeaderMap;
    use http::HeaderValue;
    use http::Method;
    use http::StatusCode;
    use hyper::Body;
    use mime::APPLICATION_JSON;
    use mime::TEXT_HTML;
    use serde_json::json;
    use tower::BoxError;
    use tower::ServiceExt;

    use super::super::*;
    use crate::plugin::test::MockHttpClientService;
    use crate::plugin::test::MockRouterService;
    use crate::plugin::test::MockSubgraphService;
    use crate::services::external::Externalizable;
    use crate::services::external::PipelineStep;
    use crate::services::external::EXTERNALIZABLE_VERSION;
    use crate::services::router_service;
    use crate::services::subgraph;
    use crate::services::supergraph;

    #[tokio::test]
    async fn load_plugin() {
        let config = json!({
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
    async fn unknown_fields_are_denied() {
        let config = json!({
            "coprocessor": {
                "url": "http://127.0.0.1:8081",
                "thisFieldDoesntExist": true
            }
        });
        // Build a test harness. Usually we'd use this and send requests to
        // it, but in this case it's enough to build the harness to see our
        // output when our service registers.
        assert!(crate::TestHarness::builder()
            .configuration_json(config)
            .unwrap()
            .build_router()
            .await
            .is_err());
    }

    #[tokio::test]
    async fn external_plugin_with_stages_wont_load_without_graph_ref() {
        let config = json!({
            "coprocessor": {
                "url": "http://127.0.0.1:8081",
                "stages": {
                    "subgraph": {
                        "request": {
                            "uri": true
                        }
                    }
                },
            }
        });
        // Build a test harness. Usually we'd use this and send requests to
        // it, but in this case it's enough to build the harness to see our
        // output when our service registers.
        assert!(crate::TestHarness::builder()
            .configuration_json(config)
            .unwrap()
            .build_router()
            .await
            .is_err());
    }

    #[tokio::test]
    async fn coprocessor_returning_the_wrong_version_should_fail() {
        let router_stage = RouterStage {
            request: RouterRequestConf {
                headers: true,
                context: true,
                body: true,
                sdl: true,
                path: false,
                method: false,
            },
            response: Default::default(),
        };

        // This will never be called because we will fail at the coprocessor.
        let mock_router_service = MockRouterService::new();

        let mock_http_client = mock_with_callback(move |_: hyper::Request<Body>| {
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
                Ok(hyper::Response::builder()
                    .body(Body::from(serde_json::to_string(&input).unwrap()))
                    .unwrap())
            })
        });

        let service = router_stage.as_service(
            mock_http_client,
            mock_router_service.boxed(),
            "http://test".to_string(),
            Arc::new("".to_string()),
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
                headers: true,
                context: true,
                body: true,
                sdl: true,
                path: false,
                method: false,
            },
            response: Default::default(),
        };

        // This will never be called because we will fail at the coprocessor.
        let mock_router_service = MockRouterService::new();

        let mock_http_client = mock_with_callback(move |_: hyper::Request<Body>| {
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
                Ok(hyper::Response::builder()
                    .body(Body::from(serde_json::to_string(&input).unwrap()))
                    .unwrap())
            })
        });

        let service = router_stage.as_service(
            mock_http_client,
            mock_router_service.boxed(),
            "http://test".to_string(),
            Arc::new("".to_string()),
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
                headers: true,
                context: true,
                body: true,
                sdl: true,
                path: false,
                method: false,
            },
            response: Default::default(),
        };

        // This will never be called because we will fail at the coprocessor.
        let mock_router_service = MockRouterService::new();

        let mock_http_client = mock_with_callback(move |_: hyper::Request<Body>| {
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
                Ok(hyper::Response::builder()
                    .body(Body::from(serde_json::to_string(&input).unwrap()))
                    .unwrap())
            })
        });

        let service = router_stage.as_service(
            mock_http_client,
            mock_router_service.boxed(),
            "http://test".to_string(),
            Arc::new("".to_string()),
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
                headers: false,
                context: false,
                body: true,
                uri: false,
                method: false,
                service_name: false,
            },
            response: Default::default(),
        };

        // This will never be called because we will fail at the coprocessor.
        let mock_subgraph_service = MockSubgraphService::new();

        let mock_http_client = mock_with_callback(move |_: hyper::Request<Body>| {
            Box::pin(async {
                Ok(hyper::Response::builder()
                    .body(Body::from(
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
        );

        let request = subgraph::Request::fake_builder().build();

        assert_eq!(
            "couldn't deserialize coprocessor output body: missing field `message`",
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
                headers: false,
                context: false,
                body: true,
                uri: false,
                method: false,
                service_name: false,
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

                Ok(subgraph::Response::builder()
                    .data(json!({ "test": 1234_u32 }))
                    .errors(Vec::new())
                    .extensions(crate::json_ext::Object::new())
                    .context(req.context)
                    .build())
            });

        let mock_http_client = mock_with_callback(move |_: hyper::Request<Body>| {
            Box::pin(async {
                Ok(hyper::Response::builder()
                    .body(Body::from(
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
        );

        let request = subgraph::Request::fake_builder().build();

        assert_eq!(
            serde_json_bytes::json!({ "test": 1234_u32 }),
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
                headers: false,
                context: false,
                body: true,
                uri: false,
                method: false,
                service_name: false,
            },
            response: Default::default(),
        };

        // This will never be called because we will fail at the coprocessor.
        let mock_subgraph_service = MockSubgraphService::new();

        let mock_http_client = mock_with_callback(move |_: hyper::Request<Body>| {
            Box::pin(async {
                Ok(hyper::Response::builder()
                    .body(Body::from(
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
        );

        let request = subgraph::Request::fake_builder().build();

        let crate::services::subgraph::Response { response, context } =
            service.oneshot(request).await.unwrap();

        assert!(context.get::<_, bool>("testKey").unwrap().unwrap());

        let value = response.headers().get("aheader").unwrap();

        assert_eq!("a value", value);

        assert_eq!(
            "my error message",
            response.into_body().errors[0].message.as_str()
        );
    }

    #[tokio::test]
    async fn external_plugin_subgraph_response() {
        let subgraph_stage = SubgraphStage {
            request: Default::default(),
            response: SubgraphResponseConf {
                headers: false,
                context: false,
                body: true,
                service_name: false,
                status_code: false,
            },
        };

        // This will never be called because we will fail at the coprocessor.
        let mut mock_subgraph_service = MockSubgraphService::new();

        mock_subgraph_service
            .expect_call()
            .returning(|req: subgraph::Request| {
                Ok(subgraph::Response::builder()
                    .data(json!({ "test": 1234_u32 }))
                    .errors(Vec::new())
                    .extensions(crate::json_ext::Object::new())
                    .context(req.context)
                    .build())
            });

        let mock_http_client = mock_with_callback(move |_: hyper::Request<Body>| {
            Box::pin(async {
                Ok(hyper::Response::builder()
                    .body(Body::from(
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
            serde_json_bytes::json!({ "test": 5678_u32 }),
            response.response.into_body().data.unwrap()
        );
    }

    #[tokio::test]
    async fn external_plugin_router_request() {
        let router_stage = RouterStage {
            request: RouterRequestConf {
                headers: true,
                context: true,
                body: true,
                sdl: true,
                path: true,
                method: true,
            },
            response: Default::default(),
        };

        let mock_router_service = router_service::from_supergraph_mock_callback(move |req| {
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

        let mock_http_client = mock_with_callback(move |req: hyper::Request<Body>| {
            Box::pin(async {
                let deserialized_request: Externalizable<serde_json::Value> =
                    serde_json::from_slice(&hyper::body::to_bytes(req.into_body()).await.unwrap())
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
                Ok(hyper::Response::builder()
                    .body(Body::from(serde_json::to_string(&input).unwrap()))
                    .unwrap())
            })
        });

        let service = router_stage.as_service(
            mock_http_client,
            mock_router_service.boxed(),
            "http://test".to_string(),
            Arc::new("".to_string()),
        );

        let request = supergraph::Request::canned_builder().build().unwrap();

        service.oneshot(request.try_into().unwrap()).await.unwrap();
    }

    #[tokio::test]
    async fn external_plugin_router_request_http_get() {
        let router_stage = RouterStage {
            request: RouterRequestConf {
                headers: true,
                context: true,
                body: true,
                sdl: true,
                path: true,
                method: true,
            },
            response: Default::default(),
        };

        let mock_router_service = router_service::from_supergraph_mock_callback(move |req| {
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

        let mock_http_client = mock_with_callback(move |req: hyper::Request<Body>| {
            Box::pin(async {
                let deserialized_request: Externalizable<serde_json::Value> =
                    serde_json::from_slice(&hyper::body::to_bytes(req.into_body()).await.unwrap())
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
                Ok(hyper::Response::builder()
                    .body(Body::from(serde_json::to_string(&input).unwrap()))
                    .unwrap())
            })
        });

        let service = router_stage.as_service(
            mock_http_client,
            mock_router_service.boxed(),
            "http://test".to_string(),
            Arc::new("".to_string()),
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
                headers: true,
                context: true,
                body: true,
                sdl: true,
                path: true,
                method: true,
            },
            response: Default::default(),
        };

        let mock_router_service = MockRouterService::new();

        let mock_http_client = mock_with_callback(move |req: hyper::Request<Body>| {
            Box::pin(async {
                let deserialized_request: Externalizable<serde_json::Value> =
                    serde_json::from_slice(&hyper::body::to_bytes(req.into_body()).await.unwrap())
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
                Ok(hyper::Response::builder()
                    .body(Body::from(serde_json::to_string(&input).unwrap()))
                    .unwrap())
            })
        });

        let service = router_stage.as_service(
            mock_http_client,
            mock_router_service.boxed(),
            "http://test".to_string(),
            Arc::new("".to_string()),
        );

        let request = supergraph::Request::canned_builder().build().unwrap();

        let crate::services::router::Response { response, context } =
            service.oneshot(request.try_into().unwrap()).await.unwrap();

        assert!(context.get::<_, bool>("testKey").unwrap().unwrap());

        let value = response.headers().get("aheader").unwrap();

        assert_eq!("a value", value);

        let actual_response = serde_json::from_slice::<serde_json::Value>(
            &hyper::body::to_bytes(response.into_body()).await.unwrap(),
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
    async fn external_plugin_router_response() {
        let router_stage = RouterStage {
            response: RouterResponseConf {
                headers: true,
                context: true,
                body: true,
                sdl: true,
                status_code: false,
            },
            request: Default::default(),
        };

        let mock_router_service = router_service::from_supergraph_mock_callback(move |req| {
            Ok(supergraph::Response::builder()
                .data(json!("{ \"test\": 1234_u32 }"))
                .context(req.context)
                .build()
                .unwrap())
        })
        .await;

        let mock_http_client = mock_with_deferred_callback(move |res: hyper::Request<Body>| {
            Box::pin(async {
                let deserialized_response: Externalizable<serde_json::Value> =
                    serde_json::from_slice(&hyper::body::to_bytes(res.into_body()).await.unwrap())
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
                Ok(hyper::Response::builder()
                    .body(Body::from(serde_json::to_string(&input).unwrap()))
                    .unwrap())
            })
        });

        let service = router_stage.as_service(
            mock_http_client,
            mock_router_service.boxed(),
            "http://test".to_string(),
            Arc::new("".to_string()),
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
            serde_json::from_slice::<serde_json::Value>(
                &hyper::body::to_bytes(res.response.into_body())
                    .await
                    .unwrap()
            )
            .unwrap()
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

    #[allow(clippy::type_complexity)]
    fn mock_with_callback(
        callback: fn(
            hyper::Request<Body>,
        ) -> BoxFuture<'static, Result<hyper::Response<Body>, BoxError>>,
    ) -> MockHttpClientService {
        let mut mock_http_client = MockHttpClientService::new();
        mock_http_client.expect_clone().returning(move || {
            let mut mock_http_client = MockHttpClientService::new();
            mock_http_client.expect_clone().returning(move || {
                let mut mock_http_client = MockHttpClientService::new();
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
            hyper::Request<Body>,
        ) -> BoxFuture<'static, Result<hyper::Response<Body>, BoxError>>,
    ) -> MockHttpClientService {
        let mut mock_http_client = MockHttpClientService::new();
        mock_http_client.expect_clone().returning(move || {
            let mut mock_http_client = MockHttpClientService::new();
            mock_http_client.expect_clone().returning(move || {
                let mut mock_http_client = MockHttpClientService::new();
                mock_http_client.expect_clone().returning(move || {
                    let mut mock_http_client = MockHttpClientService::new();
                    mock_http_client.expect_call().returning(callback);
                    mock_http_client
                });
                mock_http_client
            });
            mock_http_client
        });

        mock_http_client
    }
}
