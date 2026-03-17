use apollo_router::Context;
use apollo_router::MockedSubgraphs;
use apollo_router::TestHarness;
use apollo_router::plugin::test::MockSubgraph;
use apollo_router::services::supergraph;
use http::Method;
use serde_json::json;
use tower::BoxError;
use tower::ServiceExt;
use wiremock::Mock;
use wiremock::MockServer;
use wiremock::ResponseTemplate;
use wiremock::matchers::method;
use wiremock::matchers::path;

// NOTE: the mocked coprocessor servers aren't useful (yet)! They're in place with the TestHarness
// to avoid connection errors, but the behavior is actually governed by what we inject into the
// context. They're left here for the connection errors and to act as an example if we ever use the
// IntegrationTest builder

#[tokio::test(flavor = "multi_thread")]
async fn policy_directive_should_pass_if_coproc_allowed() -> Result<(), BoxError> {
    // GIVEN
    //   * a schema with @policy
    //   * a mock coprocessor that marks the admin policy as true (unused, see note above)
    //   * a mock subgraph serving both public and private data
    //   * a context object with the admin policy set to true
    //   * the supergraph service layer
    //   * a request for a policy-gated field

    let mock_coprocessor = MockServer::start().await;
    let coprocessor_address = mock_coprocessor.uri();

    Mock::given(method("POST"))
        .and(path("/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "version": 1,
            "stage": "SupergraphRequest",
            "control": "continue",
            "context": {
                "entries": {
                    "apollo::authorization::required_policies": {
                        "admin": true
                    }
                }
            }
        })))
        .mount(&mock_coprocessor)
        .await;

    let mut subgraphs = MockedSubgraphs::default();
    subgraphs.insert(
        "subgraph_a",
        MockSubgraph::builder()
            .with_json(
                serde_json::json!({"query": "{private{id}}"}),
                serde_json::json!({"data": {"private": {"id": "123"}}}),
            )
            .with_json(
                serde_json::json!({"query": "{public{id}}"}),
                serde_json::json!({"data": {"public": {"id": "456"}}}),
            )
            .build(),
    );

    let supergraph_harness = TestHarness::builder()
        .configuration_json(serde_json::json!({
            "coprocessor": {
                "url": coprocessor_address,
                "supergraph": {
                    "request": {
                        "context": "all"
                    }
                }
            },
            "include_subgraph_errors": {
                "all": true
            }
        }))
        .unwrap()
        .schema(include_str!(
            "../../fixtures/directives/policy/policy_basic_schema.graphql"
        ))
        .extra_plugin(subgraphs)
        .build_supergraph()
        .await
        .unwrap();

    let context = Context::new();
    context
        .insert(
            "apollo::authorization::required_policies",
            json! { ["admin"] },
        )
        .unwrap();

    // WHEN
    //   * we make a request

    let request = supergraph::Request::fake_builder()
        .query(r#"{ private { id } }"#)
        .context(context)
        .method(Method::POST)
        .build()
        .unwrap();

    // THEN
    //   * we get data back in the response for the private field!

    let response = supergraph_harness
        .oneshot(request)
        .await
        .unwrap()
        .next_response()
        .await
        .unwrap()
        .data
        .unwrap();

    let response = response
        .as_object()
        .unwrap()
        .get_key_value("private")
        .unwrap()
        .1
        .as_object()
        .unwrap()
        .get_key_value("id")
        .unwrap()
        .1;

    assert_eq!(response, "123");

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn policy_directive_should_not_pass_if_coproc_disallowed() -> Result<(), BoxError> {
    // GIVEN
    //   * a schema with @policy
    //   * a mock coprocessor that marks the admin policy as false (unused, see note above)
    //   * a mock subgraph serving both public and private data
    //   * a context object with the admin policy set to false
    //   * the supergraph service layer
    //   * a request for a policy-gated field

    let mock_coprocessor = MockServer::start().await;
    let coprocessor_address = mock_coprocessor.uri();

    Mock::given(method("POST"))
        .and(path("/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "version": 1,
            "stage": "SupergraphRequest",
            "control": "continue",
            "context": {
                "entries": {
                    "apollo::authorization::required_policies": {
                        // NOTE: see the note above, but this shouldn't govern how the test
                        // behaves; it's the context object that does the dirt for the TestHarness.
                        // Change this value if you use it with the IntegrationTest builder
                        "admin": false
                    }
                }
            }
        })))
        .mount(&mock_coprocessor)
        .await;

    let mut subgraphs = MockedSubgraphs::default();
    subgraphs.insert(
        "subgraph_a",
        MockSubgraph::builder()
            .with_json(
                serde_json::json!({"query": "{private{id}}"}),
                serde_json::json!({"data": {"private": {"id": "123"}}}),
            )
            .with_json(
                serde_json::json!({"query": "{public{id}}"}),
                serde_json::json!({"data": {"public": {"id": "456"}}}),
            )
            .build(),
    );

    let supergraph_harness = TestHarness::builder()
        .configuration_json(serde_json::json!({
            "coprocessor": {
                "url": coprocessor_address,
                "supergraph": {
                    "request": {
                        "context": "all"
                    }
                }
            },
            "include_subgraph_errors": {
                "all": true
            }
        }))
        .unwrap()
        .schema(include_str!(
            "../../fixtures/directives/policy/policy_basic_schema.graphql"
        ))
        .extra_plugin(subgraphs)
        .build_supergraph()
        .await
        .unwrap();

    let context = Context::new();
    context
        .insert(
            "apollo::authorization::required_policies",
            // NOTE: the difference between this test and the one above is that `"admin"` is not in
            // the context as an allowed policy
            json! { [] },
        )
        .unwrap();

    // WHEN
    //   * we make a request

    let request = supergraph::Request::fake_builder()
        .query(r#"{ private { id } }"#)
        .context(context)
        .method(Method::POST)
        .build()
        .unwrap();

    // THEN
    //   * we get NO data back for the private field!
    let data = supergraph_harness
        .oneshot(request)
        .await
        .unwrap()
        .next_response()
        .await
        .unwrap()
        .data
        .unwrap();
    assert!(data.is_null());

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn implementations_without_policy_should_return_data() {
    // GIVEN
    //   * a schema with @policy
    //   * an interface using the @policy with implementors that have different policies
    //     * see the fixture for notes
    //   * a mock coprocessor that marks the admin policy as false (unused, see note above)
    //   * a mock subgraph serving both public and private data
    //   * a context object with the admin policy set to false
    //   * the supergraph service layer

    let mock_coprocessor = MockServer::start().await;
    let coprocessor_address = mock_coprocessor.uri();

    Mock::given(method("POST"))
        .and(path("/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "version": 1,
            "stage": "SupergraphRequest",
            "control": "continue",
            "context": {
                "entries": {
                    "apollo::authorization::required_policies": {
                        // NOTE: see the note above, but this shouldn't govern how the test
                        // behaves; it's the context object that does the dirt for the TestHarness.
                        // Change this value if you use it with the IntegrationTest builder
                        "admin": false
                    }
                }
            }
        })))
        .mount(&mock_coprocessor)
        .await;

    let mut subgraphs = MockedSubgraphs::default();
    subgraphs.insert(
        "subgraph_a",
        MockSubgraph::builder()
            .with_json(
                serde_json::json!({"query": "{private{id}}"}),
                serde_json::json!({"data": {"private": {"id": "123"}}}),
            )
            .with_json(
                serde_json::json!({"query": "{public{id}}"}),
                serde_json::json!({"data": {"public": {"id": "456"}}}),
            )
            .with_json(
                serde_json::json!({"query": "{secure{id}}"}),
                serde_json::json!({"data": {"secure": {"id": "789"}}}),
            )
            .build(),
    );

    let supergraph_harness = TestHarness::builder()
        .configuration_json(serde_json::json!({
            "coprocessor": {
                "url": coprocessor_address,
                "supergraph": {
                    "request": {
                        "context": "all"
                    }
                }
            },
            "include_subgraph_errors": {
                "all": true
            }
        }))
        .unwrap()
        .schema(include_str!(
            "../../fixtures/directives/policy/policy_schema_with_interfaces.graphql"
        ))
        .extra_plugin(subgraphs)
        .build_supergraph()
        .await
        .unwrap();

    // WHEN
    //   * we make a request to an implementation without the policy directive
    let context = Context::new();
    context
        // NOTE: there is no `admin` policy in the context
        .insert("apollo::authorization::required_policies", json! { [] })
        .unwrap();

    let request = supergraph::Request::fake_builder()
        .query(r#"{ public { id } }"#)
        .context(context)
        .method(Method::POST)
        .build()
        .unwrap();

    // THEN
    //   * we get the data
    let response = supergraph_harness
        .oneshot(request)
        .await
        .unwrap()
        .next_response()
        .await
        .unwrap();

    let error = response.errors.first();
    assert!(error.is_none());
    let binding = response.data.unwrap();
    let response = binding
        .get("public")
        .unwrap()
        .get("id")
        .unwrap()
        .as_str()
        .unwrap();

    assert_eq!(response, "456");
}

#[tokio::test(flavor = "multi_thread")]
async fn interface_with_different_implementation_policies_should_require_auth() {
    // GIVEN
    //   * a schema with @policy
    //   * an interface using the @policy with implementors that have different policies
    //     * see the fixture for notes
    //   * a mock coprocessor that marks the admin policy as false (unused, see note above)
    //   * a mock subgraph serving both public and private data
    //   * a context object with the admin policy set to false
    //   * the supergraph service layer

    let mock_coprocessor = MockServer::start().await;
    let coprocessor_address = mock_coprocessor.uri();

    Mock::given(method("POST"))
        .and(path("/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "version": 1,
            "stage": "SupergraphRequest",
            "control": "continue",
            "context": {
                "entries": {
                    "apollo::authorization::required_policies": {
                        // NOTE: see the note above, but this shouldn't govern how the test
                        // behaves; it's the context object that does the dirt for the TestHarness.
                        // Change this value if you use it with the IntegrationTest builder
                        "admin": false
                    }
                }
            }
        })))
        .mount(&mock_coprocessor)
        .await;

    let mut subgraphs = MockedSubgraphs::default();
    subgraphs.insert(
        "subgraph_a",
        MockSubgraph::builder()
            .with_json(
                serde_json::json!({"query": "{private{id}}"}),
                serde_json::json!({"data": {"private": {"id": "123"}}}),
            )
            .with_json(
                serde_json::json!({"query": "{public{id}}"}),
                serde_json::json!({"data": {"public": {"id": "456"}}}),
            )
            .with_json(
                serde_json::json!({"query": "{secure{id}}"}),
                serde_json::json!({"data": {"secure": {"id": "789"}}}),
            )
            .build(),
    );

    let supergraph_harness = TestHarness::builder()
        .configuration_json(serde_json::json!({
            "coprocessor": {
                "url": coprocessor_address,
                "supergraph": {
                    "request": {
                        "context": "all"
                    }
                }
            },
            "include_subgraph_errors": {
                "all": true
            }
        }))
        .unwrap()
        .schema(include_str!(
            "../../fixtures/directives/policy/policy_schema_with_interfaces.graphql"
        ))
        .extra_plugin(subgraphs)
        .build_supergraph()
        .await
        .unwrap();

    // WHEN
    //   * we make a request to an interface with the policy directive
    let context = Context::new();
    context
        // NOTE: there is no `admin` policy in the context
        .insert("apollo::authorization::required_policies", json! { [] })
        .unwrap();

    let request = supergraph::Request::fake_builder()
        .query(r#"{ secure { id } }"#)
        .context(context)
        .method(Method::POST)
        .build()
        .unwrap();

    // THEN
    //   * we don't get the data and get UNAUTHORIZED_FIELD_OR_TYPE error
    let response = supergraph_harness
        .oneshot(request)
        .await
        .unwrap()
        .next_response()
        .await
        .unwrap();

    let data = response.data.unwrap();
    let error = response.errors.first().unwrap();

    assert!(data.is_null());
    assert_eq!(
        error.extension_code().unwrap(),
        "UNAUTHORIZED_FIELD_OR_TYPE".to_string()
    );
}

mod all_unauthorized_paths {
    use apollo_router::graphql;
    use serde_json::Value;

    use super::*;

    /// Sends a request to a router configured with:
    ///   * a schema with @policy
    ///   * an interface where all implementations require the `admin` policy
    ///   * a context with no matching policies (all paths unauthorized)
    async fn send_request(authorization_conf: Value) -> Result<graphql::Response, BoxError> {
        let mock_coprocessor = MockServer::start().await;
        let coprocessor_address = mock_coprocessor.uri();

        Mock::given(method("POST"))
            .and(path("/"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "version": 1,
                "stage": "SupergraphRequest",
                "control": "continue",
                "context": {
                    "entries": {
                        "apollo::authorization::required_policies": {
                            "admin": false
                        }
                    }
                }
            })))
            .mount(&mock_coprocessor)
            .await;

        let mut subgraphs = MockedSubgraphs::default();
        subgraphs.insert(
            "subgraph_a",
            MockSubgraph::builder()
                .with_json(
                    json!({"query": "{secure{id}}"}),
                    json!({"data": {"secure": {"id": "789"}}}),
                )
                .build(),
        );

        let supergraph_harness = TestHarness::builder()
            .configuration_json(json!({
                "coprocessor": {
                    "url": coprocessor_address,
                    "supergraph": {
                        "request": {
                            "context": "all"
                        }
                    }
                },
                "include_subgraph_errors": {
                    "all": true
                },
                "authorization": authorization_conf
            }))?
            .schema(include_str!(
                "../../fixtures/directives/policy/policy_schema_with_interfaces.graphql"
            ))
            .extra_plugin(subgraphs)
            .build_supergraph()
            .await?;

        let context = Context::new();
        context.insert("apollo::authorization::required_policies", json! { [] })?;

        let request = supergraph::Request::fake_builder()
            .query(r#"{ secure { id } }"#)
            .context(context)
            .method(Method::POST)
            .build()?;

        let response = supergraph_harness
            .oneshot(request)
            .await?
            .next_response()
            .await
            .unwrap();
        Ok(response)
    }

    /// Given:
    ///   * the router configuration described in `send_request`
    ///   * authorization configured to put errors into extensions rather than the errors array
    /// Then:
    ///   * data is null
    ///   * errors array is empty
    ///   * the authorization error appears in extensions["authorizationErrors"]
    #[tokio::test(flavor = "multi_thread")]
    async fn errors_in_extensions() {
        let authorization_conf = json!({
            "directives": {
                "errors": { "response": "extensions" }
            }
        });
        let response = send_request(authorization_conf).await.unwrap();

        assert!(response.data.unwrap().is_null());
        assert!(response.errors.is_empty());
        assert!(!response.extensions.is_empty());

        let auth_error = &response.extensions["authorizationErrors"][0];
        let code = auth_error["extensions"]["code"].as_str().unwrap();
        assert_eq!(code, "UNAUTHORIZED_FIELD_OR_TYPE");
    }

    /// Given:
    ///   * the router configuration described in `send_request`
    ///   * authorization configured to put errors into the errors array
    /// Then:
    ///   * data is null
    ///   * the authorization error appears in errors
    ///   * extensions has no `authorizationErrors`
    #[tokio::test(flavor = "multi_thread")]
    async fn errors_in_errors() {
        let authorization_conf = json!({
            "directives": {
                "errors": { "response": "errors" }
            }
        });
        let response = send_request(authorization_conf).await.unwrap();

        assert!(response.data.unwrap().is_null());
        assert!(!response.errors.is_empty());
        assert!(!response.extensions.contains_key("authorizationErrors"));

        let auth_error = &response.errors[0];
        let code = auth_error.extension_code().unwrap();
        assert_eq!(&code, "UNAUTHORIZED_FIELD_OR_TYPE");
    }

    /// Given:
    ///   * the router configuration described in `send_request`
    ///   * authorization configured to suppress errors entirely
    /// Then:
    ///   * data is null
    ///   * errors array is empty
    ///   * extensions has no `authorizationErrors`
    #[tokio::test(flavor = "multi_thread")]
    async fn errors_disabled() {
        let authorization_conf = json!({
            "directives": {
                "errors": { "response": "disabled" }
            }
        });
        let response = send_request(authorization_conf).await.unwrap();

        assert!(response.data.unwrap().is_null());
        assert!(response.errors.is_empty());
        assert!(!response.extensions.contains_key("authorizationErrors"));
    }
}
