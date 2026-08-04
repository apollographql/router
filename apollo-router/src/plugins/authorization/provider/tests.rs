use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use http::HeaderValue;
use serde_json::json;
use tower::ServiceExt;
use wiremock::Mock;
use wiremock::MockServer;
use wiremock::ResponseTemplate;
use wiremock::matchers::body_json;
use wiremock::matchers::method;
use wiremock::matchers::path;

use super::opa::CONTRACT_VERSION;
use super::*;

fn config(yaml: &str) -> PolicyConfig {
    serde_yaml::from_str(yaml).unwrap()
}

fn request() -> supergraph::Request {
    supergraph::Request::fake_builder()
        .query("query Test($accountId: ID!) { private { id } }")
        .operation_name("Test")
        .variable("accountId", "account-1")
        .header("x-tenant-id", "tenant-1")
        .build()
        .unwrap()
}

#[test]
fn native_decisions_remain_authoritative_for_cache_keys() {
    use crate::plugins::authorization::AuthorizationPlugin;
    use crate::plugins::authorization::CacheKeyMetadata;
    use crate::plugins::authorization::REQUIRED_POLICIES_KEY;

    let context = Context::new();
    // A later context mutation must not replace decisions made by the native provider.
    context
        .insert(
            REQUIRED_POLICIES_KEY,
            BTreeMap::from([
                ("native_allowed".to_string(), false),
                ("context_allowed".to_string(), true),
            ]),
        )
        .unwrap();
    context.extensions().with_lock(|lock| {
        lock.insert(ProviderPolicyDecisions(BTreeMap::from([
            ("native_allowed".to_string(), true),
            ("context_allowed".to_string(), false),
        ])))
    });

    AuthorizationPlugin::update_cache_key(&context);

    let metadata = context
        .extensions()
        .with_lock(|lock| lock.get::<CacheKeyMetadata>().cloned())
        .unwrap();
    assert_eq!(metadata.policies, vec!["native_allowed"]);
}

#[test]
fn legacy_context_decisions_supply_cache_keys_without_native_provider() {
    use crate::plugins::authorization::AuthorizationPlugin;
    use crate::plugins::authorization::CacheKeyMetadata;
    use crate::plugins::authorization::REQUIRED_POLICIES_KEY;

    let context = Context::new();
    context
        .insert(
            REQUIRED_POLICIES_KEY,
            BTreeMap::from([("allowed".to_string(), true), ("denied".to_string(), false)]),
        )
        .unwrap();
    AuthorizationPlugin::update_cache_key(&context);

    let metadata = context
        .extensions()
        .with_lock(|lock| lock.get::<CacheKeyMetadata>().cloned())
        .unwrap();
    assert_eq!(metadata.policies, vec!["allowed"]);
}

#[tokio::test]
async fn disabled_policy_provider_skips_registry_construction() {
    use crate::plugin::Plugin;
    use crate::plugin::PluginInit;
    use crate::plugins::authorization::AuthorizationPlugin;
    use crate::plugins::authorization::Conf;

    let config = serde_json::from_value::<Conf>(json!({
        "directives": {"enabled": false},
        "policy": {
            "enabled": false,
            "providers": {
                "primary": {
                    "type": "opa",
                    "api": {"decision": "apollo/router/authorize"},
                    "endpoints": [{"url": "http://127.0.0.1:8181"}]
                }
            },
            "routing": {"default": {"provider": "primary"}}
        }
    }))
    .unwrap();
    let plugin = AuthorizationPlugin::new(PluginInit::fake_new(
        config,
        std::sync::Arc::new(String::new()),
    ))
    .await
    .unwrap();
    assert!(plugin.policy_providers.is_none());
}

fn single_provider_yaml(endpoints: &[String]) -> String {
    let endpoints = endpoints
        .iter()
        .map(|url| format!("          - url: {url}"))
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        r#"
providers:
  primary:
    type: opa
    api:
      decision: apollo/router/authorize
    endpoints:
{endpoints}
    input:
      claims:
        include: []
      headers:
        include: [x-tenant-id]
      variables:
        include: [accountId]
routing:
  default:
    provider: primary
"#
    )
}

fn retrying_registry(endpoints: &[String]) -> PolicyProviderRegistry {
    let endpoints = endpoints
        .iter()
        .map(|url| format!("      - {{ url: {url} }}"))
        .collect::<Vec<_>>()
        .join("\n");
    PolicyProviderRegistry::new(config(&format!(
        r#"
providers:
  primary:
    type: opa
    api: {{ decision: apollo/router/authorize }}
    endpoints:
{endpoints}
    transport:
      retry: {{ max_attempts: 2 }}
routing:
  default: {{ provider: primary }}
"#
    )))
    .unwrap()
}

#[tokio::test]
async fn round_robins_across_equivalent_endpoints() {
    let first = MockServer::start().await;
    let second = MockServer::start().await;
    for server in [&first, &second] {
        Mock::given(method("POST"))
            .and(path("/v1/data/apollo/router/authorize"))
            .and(body_json(json!({
                "input": {
                    "contract": CONTRACT_VERSION,
                    "policies": ["read_profile"],
                    "operation": {
                        "name": "Test",
                        "kind": null,
                        "variables": {"accountId": "account-1"}
                    },
                    "request": {"headers": {"x-tenant-id": ["tenant-1"]}},
                    "context": {}
                }
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "decision_id": "decision-1",
                "result": {
                    "contract": CONTRACT_VERSION,
                    "decisions": {"read_profile": true},
                    "future_field": "ignored"
                },
                "future_top_level": true
            })))
            .expect(1)
            .mount(server)
            .await;
    }

    let registry =
        PolicyProviderRegistry::new(config(&single_provider_yaml(&[first.uri(), second.uri()])))
            .unwrap();
    let policies = BTreeSet::from(["read_profile".to_string()]);

    for _ in 0..2 {
        let decisions = registry
            .evaluate(&request(), policies.clone())
            .await
            .unwrap();
        assert_eq!(decisions.0.get("read_profile"), Some(&true));
    }
}

#[tokio::test]
async fn retries_server_errors_on_the_next_replica() {
    let unavailable = MockServer::start().await;
    let healthy = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(503))
        .expect(1)
        .mount(&unavailable)
        .await;
    Mock::given(method("POST"))
        .and(path("/opa/v1/data/apollo/router/authorize"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "result": {
                "contract": CONTRACT_VERSION,
                "decisions": {"read_profile": true}
            }
        })))
        .expect(1)
        .mount(&healthy)
        .await;
    let healthy_endpoint = format!("{}/opa", healthy.uri());

    let registry = retrying_registry(&[unavailable.uri(), healthy_endpoint]);

    let decisions = registry
        .evaluate(&request(), BTreeSet::from(["read_profile".to_string()]))
        .await
        .unwrap();
    assert_eq!(decisions.0.get("read_profile"), Some(&true));
}

#[tokio::test]
async fn retries_response_decode_errors_on_the_next_replica() {
    let malformed = MockServer::start().await;
    let healthy = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_raw("{", "application/json"))
        .expect(1)
        .mount(&malformed)
        .await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "result": {
                "contract": CONTRACT_VERSION,
                "decisions": {"read_profile": true}
            }
        })))
        .expect(1)
        .mount(&healthy)
        .await;

    let registry = retrying_registry(&[malformed.uri(), healthy.uri()]);

    let decisions = registry
        .evaluate(&request(), BTreeSet::from(["read_profile".to_string()]))
        .await
        .unwrap();
    assert_eq!(decisions.0.get("read_profile"), Some(&true));
}

#[tokio::test]
async fn retries_a_single_endpoint() {
    let opa = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(503))
        .up_to_n_times(1)
        .mount(&opa)
        .await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "result": {
                "contract": CONTRACT_VERSION,
                "decisions": {"read_profile": true}
            }
        })))
        .expect(1)
        .mount(&opa)
        .await;

    let registry = retrying_registry(std::slice::from_ref(&opa.uri()));
    let decisions = registry
        .evaluate(&request(), BTreeSet::from(["read_profile".to_string()]))
        .await
        .unwrap();
    assert_eq!(decisions.0.get("read_profile"), Some(&true));
}

#[tokio::test]
async fn retries_request_timeout_and_rate_limit_statuses() {
    for status in [
        http::StatusCode::REQUEST_TIMEOUT,
        http::StatusCode::TOO_MANY_REQUESTS,
    ] {
        let opa = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(status.as_u16()))
            .up_to_n_times(1)
            .mount(&opa)
            .await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "result": {
                    "contract": CONTRACT_VERSION,
                    "decisions": {"read_profile": true}
                }
            })))
            .expect(1)
            .mount(&opa)
            .await;

        let registry = retrying_registry(std::slice::from_ref(&opa.uri()));
        let decisions = registry
            .evaluate(&request(), BTreeSet::from(["read_profile".to_string()]))
            .await
            .unwrap();
        assert_eq!(decisions.0.get("read_profile"), Some(&true));
    }
}

#[tokio::test]
async fn total_timeout_is_fail_closed() {
    let opa = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_millis(100)))
        .expect(1)
        .mount(&opa)
        .await;
    let registry = PolicyProviderRegistry::new(config(&format!(
        r#"
providers:
  primary:
    type: opa
    api: {{ decision: apollo/router/authorize }}
    endpoints: [{{ url: {} }}]
    transport:
      timeouts: {{ total: 20ms }}
      retry: {{ max_attempts: 1 }}
routing:
  default: {{ provider: primary }}
"#,
        opa.uri()
    )))
    .unwrap();
    let error = registry
        .evaluate(&request(), BTreeSet::from(["read_profile".to_string()]))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("timed out"));
}

#[tokio::test]
async fn one_total_timeout_budget_covers_multiple_delayed_attempts() {
    let opa = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(503).set_delay(Duration::from_millis(40)))
        .mount(&opa)
        .await;
    let registry = PolicyProviderRegistry::new(config(&format!(
        r#"
providers:
  primary:
    type: opa
    api: {{ decision: apollo/router/authorize }}
    endpoints: [{{ url: {} }}]
    transport:
      timeouts: {{ total: 70ms }}
      retry: {{ max_attempts: 3 }}
routing:
  default: {{ provider: primary }}
"#,
        opa.uri()
    )))
    .unwrap();

    let started = tokio::time::Instant::now();
    let error = registry
        .evaluate(&request(), BTreeSet::from(["read_profile".to_string()]))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("timed out"));
    assert!(
        started.elapsed() < Duration::from_millis(110),
        "attempts exceeded one total timeout budget: {:?}",
        started.elapsed()
    );
}

#[test]
fn validates_external_endpoint_urls_strictly() {
    for endpoint in [
        "unix://relative/path.sock",
        "unix:///tmp/opa.sock?path=relative",
        "unix:///tmp/opa.sock?path=/opa&extra=value",
        "unix:///tmp/opa.sock?path=/opa&path=/other",
        "http://localhost:8181?extra=value",
    ] {
        let error =
            PolicyProviderRegistry::new(config(&single_provider_yaml(&[endpoint.to_string()])))
                .err()
                .unwrap();
        assert!(
            error
                .to_string()
                .contains("OPA provider `primary` endpoint"),
            "unexpected error for {endpoint}: {error}"
        );
    }
}

#[cfg(unix)]
#[tokio::test]
async fn evaluates_opa_over_a_unix_socket_with_a_base_path() {
    use std::convert::Infallible;

    use bytes::Bytes;
    use http_body_util::Full;
    use hyper::body::Incoming;
    use hyper_util::rt::TokioExecutor;
    use hyper_util::rt::TokioIo;

    let directory = tempfile::tempdir().unwrap();
    let socket_path = directory.path().join("opa.sock");
    let listener = tokio::net::UnixListener::bind(&socket_path).unwrap();
    let (path_sender, path_receiver) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let path_sender = Arc::new(std::sync::Mutex::new(Some(path_sender)));
        let service = hyper::service::service_fn(move |request: http::Request<Incoming>| {
            if let Some(sender) = path_sender.lock().unwrap().take() {
                let _ = sender.send(request.uri().path().to_string());
            }
            async move {
                Ok::<_, Infallible>(http::Response::new(Full::new(Bytes::from_static(
                    br#"{"result":{"contract":"apollo.router.policy/v1","decisions":{"read_profile":true}}}"#,
                ))))
            }
        });
        hyper_util::server::conn::auto::Builder::new(TokioExecutor::new())
            .serve_connection(TokioIo::new(stream), service)
            .await
            .unwrap();
    });
    let endpoint = format!("unix://{}?path=/opa", socket_path.display());
    let registry = PolicyProviderRegistry::new(config(&single_provider_yaml(&[endpoint]))).unwrap();

    let decisions = registry
        .evaluate(&request(), BTreeSet::from(["read_profile".to_string()]))
        .await
        .unwrap();
    assert_eq!(decisions.0.get("read_profile"), Some(&true));
    assert_eq!(
        path_receiver.await.unwrap(),
        "/opa/v1/data/apollo/router/authorize"
    );
    server.abort();
}

#[tokio::test]
async fn routes_by_exact_match_then_longest_prefix() {
    let primary = MockServer::start().await;
    let finance = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "result": {
                "contract": CONTRACT_VERSION,
                "decisions": {
                    "read_profile": true,
                    "finance:special": true
                }
            }
        })))
        .expect(1)
        .mount(&primary)
        .await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "result": {
                "contract": CONTRACT_VERSION,
                "decisions": {"finance:approve_refund": false}
            }
        })))
        .expect(1)
        .mount(&finance)
        .await;

    let registry = PolicyProviderRegistry::new(config(&format!(
        r#"
providers:
  primary:
    type: opa
    api: {{ decision: apollo/router/authorize }}
    endpoints: [{{ url: {} }}]
  finance:
    type: opa
    api: {{ decision: finance/router/authorize }}
    endpoints: [{{ url: {} }}]
routing:
  default: {{ provider: primary }}
  rules:
    - match:
        prefix: ["fin"]
      target: {{ provider: primary }}
    - match:
        prefix: ["finance:"]
      target: {{ provider: finance }}
    - match:
        exact: ["finance:special"]
      target: {{ provider: primary }}
"#,
        primary.uri(),
        finance.uri()
    )))
    .unwrap();

    let decisions = registry
        .evaluate(
            &request(),
            BTreeSet::from([
                "read_profile".to_string(),
                "finance:approve_refund".to_string(),
                "finance:special".to_string(),
            ]),
        )
        .await
        .unwrap();
    assert_eq!(decisions.0.get("read_profile"), Some(&true));
    assert_eq!(decisions.0.get("finance:approve_refund"), Some(&false));
    assert_eq!(decisions.0.get("finance:special"), Some(&true));
}

#[tokio::test]
async fn undefined_and_omitted_decisions_fail_closed() {
    let undefined = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .expect(1)
        .mount(&undefined)
        .await;
    let registry =
        PolicyProviderRegistry::new(config(&single_provider_yaml(&[undefined.uri()]))).unwrap();
    let decisions = registry
        .evaluate(&request(), BTreeSet::from(["read_profile".to_string()]))
        .await
        .unwrap();
    assert_eq!(decisions.0.get("read_profile"), Some(&false));

    let omitted = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "result": {
                "contract": CONTRACT_VERSION,
                "decisions": {}
            }
        })))
        .expect(1)
        .mount(&omitted)
        .await;
    let registry =
        PolicyProviderRegistry::new(config(&single_provider_yaml(&[omitted.uri()]))).unwrap();
    let decisions = registry
        .evaluate(&request(), BTreeSet::from(["read_profile".to_string()]))
        .await
        .unwrap();
    assert_eq!(decisions.0.get("read_profile"), Some(&false));
}

#[tokio::test]
async fn forwards_repeated_headers_as_arrays() {
    let opa = MockServer::start().await;
    Mock::given(method("POST"))
        .and(body_json(json!({
            "input": {
                "contract": CONTRACT_VERSION,
                "policies": ["read_profile"],
                "operation": {
                    "name": "Test",
                    "kind": null,
                    "variables": {"accountId": "account-1"}
                },
                "request": {"headers": {"x-tenant-id": ["tenant-1", "tenant-2"]}},
                "context": {}
            }
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "result": {
                "contract": CONTRACT_VERSION,
                "decisions": {"read_profile": true}
            }
        })))
        .expect(1)
        .mount(&opa)
        .await;
    let mut request = request();
    request
        .supergraph_request
        .headers_mut()
        .append("x-tenant-id", HeaderValue::from_static("tenant-2"));
    let registry =
        PolicyProviderRegistry::new(config(&single_provider_yaml(&[opa.uri()]))).unwrap();
    registry
        .evaluate(&request, BTreeSet::from(["read_profile".to_string()]))
        .await
        .unwrap();
}

#[tokio::test]
async fn missing_result_emits_diagnostic_counter() {
    use crate::metrics::FutureMetricsExt;

    async {
        let opa = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
            .expect(1)
            .mount(&opa)
            .await;
        let registry =
            PolicyProviderRegistry::new(config(&single_provider_yaml(&[opa.uri()]))).unwrap();
        registry
            .evaluate(&request(), BTreeSet::from(["read_profile".to_string()]))
            .await
            .unwrap();

        assert_counter!(
            "apollo.router.operations.policy_provider.missing_result",
            1,
            "policy.provider.name" = "primary"
        );
    }
    .with_metrics()
    .await;
}

#[tokio::test]
async fn records_success_decision_retry_duration_and_connection_metrics() {
    use crate::metrics::FutureMetricsExt;

    async {
        let unavailable = MockServer::start().await;
        let healthy = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(503))
            .expect(1)
            .mount(&unavailable)
            .await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "result": {
                    "contract": CONTRACT_VERSION,
                    "decisions": {"allow": true, "deny": false}
                }
            })))
            .expect(1)
            .mount(&healthy)
            .await;
        let registry = retrying_registry(&[unavailable.uri(), healthy.uri()]);

        registry
            .evaluate(
                &request(),
                BTreeSet::from(["allow".to_string(), "deny".to_string()]),
            )
            .await
            .unwrap();

        assert_counter!(
            "apollo.router.operations.policy_provider.retry",
            1,
            "policy.provider.name" = "primary",
            "policy.provider.endpoint.index" = 0i64
        );
        assert_counter!(
            "apollo.router.operations.policy_provider.allow",
            1,
            "policy.provider.name" = "primary"
        );
        assert_counter!(
            "apollo.router.operations.policy_provider.deny",
            1,
            "policy.provider.name" = "primary"
        );
        assert_histogram_count!(
            "apollo.router.operations.policy_provider.duration",
            1,
            "policy.provider.type" = "opa",
            "policy.provider.name" = "primary",
            "policy.provider.outcome" = "success"
        );
        assert_histogram_count!(
            "apollo.router.connection.acquire.duration",
            2,
            "network.transport" = "tcp",
            "policy.provider.name" = "primary"
        );
    }
    .with_metrics()
    .await;
}

#[tokio::test]
async fn records_exhaustion_and_endpoint_ejection_without_counting_a_final_retry() {
    use crate::metrics::FutureMetricsExt;

    async {
        let opa = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(503))
            .expect(2)
            .mount(&opa)
            .await;
        let registry = retrying_registry(std::slice::from_ref(&opa.uri()));

        registry
            .evaluate(&request(), BTreeSet::from(["read_profile".to_string()]))
            .await
            .unwrap_err();

        assert_counter!(
            "apollo.router.operations.policy_provider.retry",
            1,
            "policy.provider.name" = "primary",
            "policy.provider.endpoint.index" = 0i64
        );
        assert_counter!(
            "apollo.router.operations.policy_provider.retry_exhausted",
            1,
            "policy.provider.name" = "primary"
        );
        assert_counter!(
            "apollo.router.operations.policy_provider.endpoint_ejected",
            1,
            "policy.provider.name" = "primary",
            "policy.provider.endpoint.index" = 0i64
        );
    }
    .with_metrics()
    .await;
}

#[tokio::test]
async fn rejects_non_utf8_selected_headers() {
    let opa = MockServer::start().await;
    let mut request = request();
    request
        .supergraph_request
        .headers_mut()
        .insert("x-tenant-id", HeaderValue::from_bytes(&[0xff]).unwrap());
    let registry =
        PolicyProviderRegistry::new(config(&single_provider_yaml(&[opa.uri()]))).unwrap();
    let error = registry
        .evaluate(&request, BTreeSet::from(["read_profile".to_string()]))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("non-UTF-8 header"));
}

#[tokio::test(flavor = "multi_thread")]
async fn native_provider_drives_router_policy_filtering() {
    use crate::MockedSubgraphs;
    use crate::TestHarness;
    use crate::plugin::test::MockSubgraph;

    let opa = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/data/apollo/router/authorize"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "result": {
                "contract": CONTRACT_VERSION,
                "decisions": {"admin": false}
            }
        })))
        .expect(1)
        .mount(&opa)
        .await;

    let mut subgraphs = MockedSubgraphs::default();
    subgraphs.insert(
        "subgraph_a",
        MockSubgraph::builder()
            .with_json(
                json!({"query": "{private{id}}"}),
                json!({"data": {"private": {"id": "123"}}}),
            )
            .build(),
    );
    let router = TestHarness::builder()
        .configuration_json(json!({
            "authorization": {
                "policy": {
                    "providers": {
                        "primary": {
                            "type": "opa",
                            "api": {"decision": "apollo/router/authorize"},
                            "endpoints": [{"url": opa.uri()}]
                        }
                    },
                    "routing": {"default": {"provider": "primary"}}
                }
            }
        }))
        .unwrap()
        .schema(include_str!(
            "../../../../tests/fixtures/directives/policy/policy_basic_schema.graphql"
        ))
        .extra_plugin(subgraphs)
        .build_router()
        .await
        .unwrap();
    let request = crate::services::router::Request::fake_builder()
        .method(http::Method::POST)
        .header("content-type", "application/json")
        .header("accept", "application/json")
        .body(json!({"query": "{ private { id } }"}).to_string())
        .build()
        .unwrap();

    let response = router
        .oneshot(request)
        .await
        .unwrap()
        .next_response()
        .await
        .unwrap()
        .unwrap();
    let response: serde_json::Value = serde_json::from_slice(&response).unwrap();
    assert!(response["data"]["private"].is_null());
    assert_eq!(response["errors"][0]["path"], json!(["private"]));
    assert_eq!(
        response["errors"][0]["extensions"]["code"],
        "UNAUTHORIZED_FIELD_OR_TYPE"
    );
}

#[test]
fn rejects_duplicate_routes() {
    let error = PolicyProviderRegistry::new(config(
        r#"
providers:
  primary:
    type: opa
    api: { decision: test }
    endpoints: [{ url: http://localhost:8181 }]
routing:
  default: { provider: primary }
  rules:
    - match: { exact: [admin] }
      target: { provider: primary }
    - match: { exact: [admin] }
      target: { provider: primary }
"#,
    ))
    .err()
    .unwrap();
    assert!(error.to_string().contains("routed more than once"));
}

/// Local validation against the three OPA processes documented in
/// `examples/opa-policy-provider/README.md`.
#[tokio::test]
#[ignore = "requires local OPA services on ports 18181, 18182, and 18281"]
async fn validates_multiple_real_opa_services() {
    let registry = PolicyProviderRegistry::new(config(
        r#"
providers:
  primary:
    type: opa
    api: { decision: apollo/router/authorize }
    endpoints:
      - { url: http://127.0.0.1:18181 }
      - { url: http://127.0.0.1:18182 }
  finance:
    type: opa
    api: { decision: finance/router/authorize }
    endpoints:
      - { url: http://127.0.0.1:18281 }
routing:
  default: { provider: primary }
  rules:
    - match: { prefix: ["finance:"] }
      target: { provider: finance }
"#,
    ))
    .unwrap();
    let requested = BTreeSet::from([
        "read_profile".to_string(),
        "unknown".to_string(),
        "finance:approve_refund".to_string(),
    ]);

    // Run twice so round-robin reaches both equivalent primary replicas.
    for _ in 0..2 {
        let decisions = registry
            .evaluate(&request(), requested.clone())
            .await
            .unwrap();
        assert_eq!(decisions.0.get("read_profile"), Some(&true));
        assert_eq!(decisions.0.get("unknown"), Some(&false));
        assert_eq!(decisions.0.get("finance:approve_refund"), Some(&true));
    }
}

mod service_layer {
    use std::collections::BTreeMap;
    use std::sync::Arc;
    use std::sync::atomic::AtomicUsize;
    use std::sync::atomic::Ordering;

    use http::StatusCode;
    use serde_json::json;
    use tower::Layer;
    use tower::Service;
    use tower::ServiceExt as _;
    use wiremock::Mock;
    use wiremock::MockServer;
    use wiremock::ResponseTemplate;
    use wiremock::matchers::method;

    use super::*;
    use crate::Context;
    use crate::plugins::authorization::REQUIRED_POLICIES_KEY;
    use crate::plugins::authorization::provider::PolicyConfig;

    fn registry(endpoint: &str, failure_mode: FailureMode) -> Arc<PolicyProviderRegistry> {
        let failure_mode = match failure_mode {
            FailureMode::Reject => "reject",
            FailureMode::Deny => "deny",
        };
        let config = serde_yaml::from_str::<PolicyConfig>(&format!(
            r#"
providers:
  primary:
    type: opa
    api: {{ decision: apollo/router/authorize }}
    endpoints: [{{ url: {endpoint} }}]
    transport:
      retry: {{ max_attempts: 1 }}
routing:
  default: {{ provider: primary }}
failure:
  mode: {failure_mode}
"#
        ))
        .unwrap();
        Arc::new(PolicyProviderRegistry::new(config).unwrap())
    }

    fn request(policies: &[&str]) -> supergraph::Request {
        let context = Context::new();
        if !policies.is_empty() {
            context
                .insert(
                    REQUIRED_POLICIES_KEY,
                    policies
                        .iter()
                        .map(|policy| (policy.to_string(), None::<bool>))
                        .collect::<BTreeMap<_, _>>(),
                )
                .unwrap();
        }
        supergraph::Request::fake_builder()
            .context(context)
            .build()
            .unwrap()
    }

    fn inner_service(calls: Arc<AtomicUsize>) -> supergraph::BoxCloneService {
        supergraph::BoxCloneService::new(tower::service_fn(move |request: supergraph::Request| {
            let calls = calls.clone();
            async move {
                calls.fetch_add(1, Ordering::Relaxed);
                Ok(supergraph::Response::fake_builder()
                    .context(request.context)
                    .build()
                    .unwrap())
            }
        }))
    }

    #[tokio::test]
    async fn passes_through_requests_without_required_policies() {
        let calls = Arc::new(AtomicUsize::new(0));
        let mut service = PolicyProviderLayer::with_dry_run(
            registry("http://127.0.0.1:1", FailureMode::Reject),
            false,
        )
        .layer(inner_service(calls.clone()));

        service
            .ready()
            .await
            .unwrap()
            .call(request(&[]))
            .await
            .unwrap();

        assert_eq!(calls.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn stores_provider_decisions_before_calling_inner_service() {
        let opa = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "result": {
                    "contract": super::super::opa::CONTRACT_VERSION,
                    "decisions": {"allow": true, "deny": false}
                }
            })))
            .expect(1)
            .mount(&opa)
            .await;
        let calls = Arc::new(AtomicUsize::new(0));
        let mut service =
            PolicyProviderLayer::with_dry_run(registry(&opa.uri(), FailureMode::Reject), false)
                .layer(inner_service(calls.clone()));

        let response = service
            .ready()
            .await
            .unwrap()
            .call(request(&["allow", "deny"]))
            .await
            .unwrap();

        assert_eq!(calls.load(Ordering::Relaxed), 1);
        assert_eq!(
            response
                .context
                .get_json_value(REQUIRED_POLICIES_KEY)
                .unwrap(),
            serde_json_bytes::json!({"allow": true, "deny": false})
        );
        assert_eq!(
            response.context.extensions().with_lock(|lock| lock
                .get::<ProviderPolicyDecisions>()
                .unwrap()
                .allowed_policies()),
            vec!["allow"]
        );
    }

    #[tokio::test]
    async fn deny_failure_mode_stores_denials_and_calls_inner_service() {
        let opa = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(503))
            .expect(1)
            .mount(&opa)
            .await;
        let calls = Arc::new(AtomicUsize::new(0));
        let mut service =
            PolicyProviderLayer::with_dry_run(registry(&opa.uri(), FailureMode::Deny), false)
                .layer(inner_service(calls.clone()));

        let response = service
            .ready()
            .await
            .unwrap()
            .call(request(&["first", "second"]))
            .await
            .unwrap();

        assert_eq!(calls.load(Ordering::Relaxed), 1);
        assert_eq!(
            response
                .context
                .get_json_value(REQUIRED_POLICIES_KEY)
                .unwrap(),
            serde_json_bytes::json!({"first": false, "second": false})
        );
    }

    #[tokio::test]
    async fn reject_failure_mode_returns_503_without_calling_inner_service() {
        let opa = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(503))
            .expect(1)
            .mount(&opa)
            .await;
        let calls = Arc::new(AtomicUsize::new(0));
        let mut service =
            PolicyProviderLayer::with_dry_run(registry(&opa.uri(), FailureMode::Reject), false)
                .layer(inner_service(calls.clone()));

        let response = service
            .ready()
            .await
            .unwrap()
            .call(request(&["admin"]))
            .await
            .unwrap();

        assert_eq!(calls.load(Ordering::Relaxed), 0);
        assert_eq!(response.response.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn dry_run_provider_failure_continues_without_503() {
        let opa = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(503))
            .expect(1)
            .mount(&opa)
            .await;
        let calls = Arc::new(AtomicUsize::new(0));
        let mut service =
            PolicyProviderLayer::with_dry_run(registry(&opa.uri(), FailureMode::Reject), true)
                .layer(inner_service(calls.clone()));

        let response = service
            .ready()
            .await
            .unwrap()
            .call(request(&["admin"]))
            .await
            .unwrap();

        assert_eq!(calls.load(Ordering::Relaxed), 1);
        assert_eq!(response.response.status(), StatusCode::OK);
        assert_eq!(
            response
                .context
                .get_json_value(REQUIRED_POLICIES_KEY)
                .unwrap(),
            serde_json_bytes::json!({"admin": false})
        );
    }

    #[tokio::test]
    async fn failure_metric_includes_provider_identity_and_handling() {
        use crate::metrics::FutureMetricsExt;

        async {
            let opa = MockServer::start().await;
            Mock::given(method("POST"))
                .respond_with(ResponseTemplate::new(503))
                .expect(1)
                .mount(&opa)
                .await;
            let calls = Arc::new(AtomicUsize::new(0));
            let mut service =
                PolicyProviderLayer::with_dry_run(registry(&opa.uri(), FailureMode::Reject), false)
                    .layer(inner_service(calls));

            service
                .ready()
                .await
                .unwrap()
                .call(request(&["admin"]))
                .await
                .unwrap();

            assert_counter!(
                "apollo.router.operations.policy_provider.failure",
                1,
                "policy.provider.name" = "primary",
                "policy.provider.outcome" = "reject_failure"
            );
        }
        .with_metrics()
        .await;
    }
}
