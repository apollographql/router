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
    //   * we get NO data back forthe private field!

    let response = supergraph_harness
        .oneshot(request)
        .await
        .unwrap()
        .next_response()
        .await
        .unwrap()
        .data
        .unwrap();

    let response = response.as_object().unwrap();

    assert!(response.is_empty());

    Ok(())
}

// FIXME: this expresses the wrong behavior despite being the current behavior; in the future,
// after a fix in composition, no data should be returned but errors for an unauthorized field
// TODO: fix during FED-790; make the return type `Result<(), BoxError>` and return `Ok(())`
#[tokio::test(flavor = "multi_thread")]
#[should_panic(expected = "called `Option::unwrap()` on a `None` value")]
async fn policy_directive_interfaces_with_different_implementors_without_policy_should_return_data()
{
    // GIVEN
    //   * a schema with @policy
    //   * an interface using the @policy with implementors that have different policies
    //     * see the fixture for notes
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
            .with_json(
                serde_json::json!({"query": "{opensecret{id}}"}),
                serde_json::json!({"data": {"opensecret": {"id": "789"}}}),
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

    let context = Context::new();
    context
        // NOTE: there is no `admin` policy in the context
        .insert("apollo::authorization::required_policies", json! { [] })
        .unwrap();

    // WHEN
    //   * we make a request to an implementor without the policy directive
    let request = supergraph::Request::fake_builder()
        .query(r#"{ opensecret { id } }"#)
        .context(context)
        .method(Method::POST)
        .build()
        .unwrap();

    // THEN
    //   * we don't get any data, but we do get errors

    let response = supergraph_harness
        .oneshot(request)
        .await
        .unwrap()
        .next_response()
        .await
        .unwrap();

    let data = response.data.unwrap();
    let error = response.errors.first().unwrap();

    assert!(data.as_object().unwrap().is_empty());
    assert_eq!(
        error.extension_code().unwrap(),
        "UNAUTHORIZED_FIELD_OR_TYPE".to_string()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn policy_directive_interfaces_with_different_implementors_disallowed() -> Result<(), BoxError>
{
    // GIVEN
    //   * a schema with @policy
    //   * an interface using the @policy with implementors that have different policies
    //     * see the fixture for notes
    //   * requesting the interface directly, not one of its implementors
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
            .with_json(
                serde_json::json!({"query": "{opensecret{id}}"}),
                serde_json::json!({"data": {"opensecret": {"id": "789"}}}),
            )
            .with_json(
                serde_json::json!({"query": "{secure{id}}"}),
                serde_json::json!({"data": {"secure": {"id": "000"}}}),
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

    let context = Context::new();
    context
        // NOTE: there is no `admin` policy in the context
        .insert("apollo::authorization::required_policies", json! { [] })
        .unwrap();

    // WHEN
    //   * we make a request with the interface itself off of Query
    let request = supergraph::Request::fake_builder()
        .query(r#"{ secure { id } }"#)
        .context(context)
        .method(Method::POST)
        .build()
        .unwrap();

    // THEN
    //   * we don't get data

    let response = supergraph_harness
        .oneshot(request)
        .await
        .unwrap()
        .next_response()
        .await
        .unwrap()
        .data
        .unwrap();

    let response = response.as_object().unwrap();

    assert!(response.is_empty());
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn policy_directive_interfaces_with_different_implementors_open_question()
-> Result<(), BoxError> {
    // GIVEN
    //   * a schema with @policy
    //   * an interface using the @policy with implementors that have different policies
    //     * see the fixture for notes
    //   * requesting the interface directly, not one of its implementors
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
                        // NOTE: see the note above, but this shouldn't govern how the test
                        // behaves; it's the context object that does the dirt for the TestHarness.
                        // Change this value if you use it with the IntegrationTest builder
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
            .with_json(
                serde_json::json!({"query": "{opensecret{id}}"}),
                serde_json::json!({"data": {"opensecret": {"id": "789"}}}),
            )
            .with_json(
                serde_json::json!({"query": "{secure{id}}"}),
                serde_json::json!({"data": {"secure": {"id": "000"}}}),
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

    let context = Context::new();
    context
        .insert(
            "apollo::authorization::required_policies",
            json! {[ "admin" ]},
        )
        .unwrap();

    // WHEN
    //   * we make a request with the interface itself off of Query
    let request = supergraph::Request::fake_builder()
        .query(r#"{ secure { id } }"#)
        .context(context)
        .method(Method::POST)
        .build()
        .unwrap();

    // THEN
    //   * we don't get any data, but we do get errors

    let response = supergraph_harness
        .oneshot(request)
        .await
        .unwrap()
        .next_response()
        .await
        .unwrap();
    let data = response.data.unwrap();
    let error = response.errors.first().unwrap();

    assert!(data.as_object().unwrap().is_empty());
    assert_eq!(
        error.extension_code().unwrap(),
        "UNAUTHORIZED_FIELD_OR_TYPE".to_string()
    );

    Ok(())
}
