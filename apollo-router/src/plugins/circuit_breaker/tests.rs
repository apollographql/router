use std::sync::Arc;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::time::Duration;

use apollo_compiler::name;
use apollo_federation::connectors::ConnectId;
use apollo_federation::connectors::ConnectSpec;
use apollo_federation::connectors::Connector;
use apollo_federation::connectors::HttpJsonTransport;
use apollo_federation::connectors::JSONSelection;
use apollo_federation::connectors::SourceName;
use apollo_federation::connectors::runtime::http_json_transport::HttpRequest;
use apollo_federation::connectors::runtime::http_json_transport::HttpResponse;
use apollo_federation::connectors::runtime::key::ResponseKey;
use apollo_federation::connectors::runtime::responses::MappedResponse;
use http::StatusCode;
use tower::Service;
use tower::ServiceExt as _;

use super::*;
use crate::Context;
use crate::metrics::FutureMetricsExt as _;
use crate::plugins::test::PluginTestHarness;
use crate::services::connector::request_service::Request as ConnectorRequest;
use crate::services::connector::request_service::Response as ConnectorResponse;
use crate::services::subgraph;

// --- helpers -------------------------------------------------------------------------------

async fn harness(config: &str) -> PluginTestHarness<CircuitBreaker> {
    PluginTestHarness::builder()
        .config(config)
        .build()
        .await
        .expect("plugin should be configured")
}

async fn plugin_error(config: &str) -> String {
    PluginTestHarness::<CircuitBreaker>::builder()
        .config(config)
        .build()
        .await
        .err()
        .expect("configuration should have been rejected")
        .to_string()
}

/// The error the router's configuration validation gives for `yaml`.
///
/// Used for the blocks the configuration schema rejects, which never reach the plugin: the plugin
/// harness requires a configuration the schema already accepts. `yaml` starts at column zero,
/// like a real `router.yaml`.
fn config_error(yaml: &str) -> String {
    crate::configuration::validate_yaml_configuration(
        yaml,
        crate::configuration::expansion::Expansion::default().expect("default expansion"),
        crate::configuration::schema::Mode::NoUpgrade,
    )
    .expect_err("configuration should have been rejected")
    .to_string()
}

fn subgraph_response(status: StatusCode, request: &subgraph::Request) -> subgraph::Response {
    subgraph::Response::fake_builder()
        .status_code(status)
        .context(request.context.clone())
        .subgraph_name(request.subgraph_name.clone())
        .id(request.id.clone())
        .build()
}

fn connector_request() -> ConnectorRequest {
    let connector = Arc::new(Connector {
        spec: ConnectSpec::V0_1,
        schema_subtypes_map: Default::default(),
        id: ConnectId::new(
            "products".into(),
            Some(SourceName::cast("api")),
            name!(Query),
            name!(hello),
            None,
            0,
        ),
        transport: Some(HttpJsonTransport {
            source_template: "http://localhost/api".parse().ok(),
            connect_template: "/path".parse().unwrap(),
            ..Default::default()
        }),
        selection: JSONSelection::parse("$.data").unwrap(),
        entity_resolver: None,
        config: Default::default(),
        max_requests: None,
        batch_settings: None,
        request_headers: Default::default(),
        response_headers: Default::default(),
        request_variable_keys: Default::default(),
        response_variable_keys: Default::default(),
        error_settings: Default::default(),
        output_type: None,
        label: "test label".into(),
    });

    let http_request = HttpRequest {
        inner: http::Request::builder().body("{}".to_string()).unwrap(),
        debug: Default::default(),
    };

    ConnectorRequest {
        context: Context::default(),
        connector,
        transport_request: http_request.into(),
        key: response_key(),
        mapping_problems: Default::default(),
        supergraph_request: Default::default(),
        operation: Default::default(),
    }
}

fn response_key() -> ResponseKey {
    ResponseKey::RootField {
        name: "hello".to_string(),
        inputs: Default::default(),
        selection: Arc::new(JSONSelection::parse("$.data").unwrap()),
    }
}

fn connector_response(status: StatusCode, request: &ConnectorRequest) -> ConnectorResponse {
    let (parts, _) = http::Response::builder()
        .status(status)
        .body(())
        .unwrap()
        .into_parts();

    ConnectorResponse {
        context: request.context.clone(),
        subgraph_name: request.connector.id.subgraph_name.to_string(),
        transport_result: Ok(HttpResponse { inner: parts }.into()),
        mapped_response: MappedResponse::Data {
            data: Default::default(),
            problems: Vec::new(),
            key: response_key(),
        },
    }
}

/// A connector response that never reached the source, carrying `error` as its transport result
/// the way the connector request service and the plugins under it do.
fn connector_transport_error(error: Error, request: &ConnectorRequest) -> ConnectorResponse {
    ConnectorResponse::error_new(
        request.context.clone(),
        request.connector.id.subgraph_name.to_string(),
        error,
        "the router turned this request away",
        response_key(),
    )
}

/// The error code the plugin reports on a connector response it rejected, or `None` when the
/// response carries no error.
fn connector_error_code(response: &ConnectorResponse) -> Option<&str> {
    match &response.mapped_response {
        MappedResponse::Error { error, .. } => Some(error.code()),
        MappedResponse::Data { .. } => None,
    }
}

/// A connector service wrapped by the plugin for `source_name`, counting the requests that reach
/// it and answering each with `response_fn`.
fn connector_service(
    plugin: &PluginTestHarness<CircuitBreaker>,
    source_name: &str,
    response_fn: impl Fn(&ConnectorRequest) -> ConnectorResponse + Clone + Send + Sync + 'static,
) -> (
    connector::request_service::BoxCloneService,
    Arc<AtomicUsize>,
) {
    let calls = Arc::new(AtomicUsize::new(0));
    let service = plugin.connector_request_service(
        {
            let calls = calls.clone();
            tower::service_fn(move |req: ConnectorRequest| {
                let calls = calls.clone();
                let response_fn = response_fn.clone();
                async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    Ok(response_fn(&req))
                }
            })
            .boxed_clone()
        },
        source_name.to_string(),
    );
    (service, calls)
}

// --- configuration -------------------------------------------------------------------------

#[tokio::test]
async fn a_per_subgraph_block_stands_in_for_all_rather_than_layering_over_it() {
    let plugin = harness(
        r#"
        circuit_breaker:
          all:
            window_size: 200
            min_requests: 20
            consecutive_failures: 5
          subgraphs:
            products:
              consecutive_failures: 2
        "#,
    )
    .await;

    let products = plugin.subgraphs.named.get("products").expect("configured");
    let products = products.as_ref().expect("enabled");
    assert_eq!(products.consecutive_failures.get(), 2);

    // Every option `products` left out is at the apollo-qos default, not at the value `all` gave
    // it: a subgraph's own block replaces `all` instead of overriding it option by option.
    assert_eq!(products.window_size.get(), 100);
    assert_eq!(products.min_requests.get(), 10);
    assert_eq!(products.failure_rate_threshold, 0.5);
    assert_eq!(*products.open_duration, Duration::from_secs(30));

    // A subgraph with no entry of its own gets `all` untouched.
    let all = plugin.subgraphs.all.as_ref().expect("configured");
    assert_eq!(all.window_size.get(), 200);
    assert_eq!(all.consecutive_failures.get(), 5);
}

#[tokio::test]
async fn a_subgraph_can_opt_out_of_an_all_block() {
    let plugin = harness(
        r#"
        circuit_breaker:
          all:
            consecutive_failures: 1
          subgraphs:
            products:
              enabled: false
        "#,
    )
    .await;

    assert!(
        plugin.subgraphs.layer("products").is_none(),
        "products opted out, so it should not be wrapped"
    );
    assert!(
        plugin.subgraphs.layer("reviews").is_some(),
        "reviews has no entry of its own, so `all` should apply to it"
    );
}

#[tokio::test]
async fn a_disabled_all_block_wraps_nothing() {
    let plugin = harness(
        r#"
        circuit_breaker:
          all:
            enabled: false
        "#,
    )
    .await;

    assert!(
        plugin.subgraphs.layer("products").is_none(),
        "`all` is disabled, so no subgraph should be wrapped"
    );
}

/// The options share a block with `enabled`, so each one has to survive being flattened out into
/// JSON and deserialized from there by `parse_options` rather than from the configuration
/// directly. `open_duration` is the one to watch: a value parsed by a `Deserialize` impl of its
/// own — `1m` into a duration, here — is the kind that goes wrong on the way through.
#[tokio::test]
async fn every_option_can_be_set_on_a_single_target() {
    let plugin = harness(
        r#"
        circuit_breaker:
          subgraphs:
            products:
              failure_rate_threshold: 0.8
              window_size: 50
              min_requests: 20
              open_duration: 1m
              consecutive_failures: 3
        "#,
    )
    .await;

    let products = plugin
        .subgraphs
        .named
        .get("products")
        .expect("configured")
        .as_ref()
        .expect("enabled");
    assert_eq!(products.failure_rate_threshold, 0.8);
    assert_eq!(products.window_size.get(), 50);
    assert_eq!(products.min_requests.get(), 20);
    assert_eq!(*products.open_duration, Duration::from_secs(60));
    assert_eq!(products.consecutive_failures.get(), 3);
}

/// `enabled` shares a block with the options, so a target can be switched off with its options
/// left in place — and switched back on without restating them.
#[tokio::test]
async fn enabled_sits_alongside_the_options_it_switches() {
    for (switch, expected) in [("false", None), ("true", Some(2))] {
        let plugin = harness(&format!(
            r#"
            circuit_breaker:
              subgraphs:
                products:
                  enabled: {switch}
                  consecutive_failures: 2
            "#
        ))
        .await;

        let products = plugin
            .subgraphs
            .named
            .get("products")
            .expect("configured")
            .as_ref()
            .map(|config| config.consecutive_failures.get());
        assert_eq!(products, expected, "for enabled: {switch}");
    }
}

/// `enabled` beside a flattened set of options is the one shape serde cannot deny unknown fields
/// for, so the plugin puts the denial in its JSON schema by hand. It is the only check that can
/// point at the key in the document the user wrote, which is why it is worth having on top of the
/// one `parse_options` makes.
#[test]
fn a_misspelled_option_is_rejected_by_name() {
    let error = config_error(
        r#"
circuit_breaker:
  subgraphs:
    products:
      window_sze: 50
"#,
    );

    assert!(
        error.contains("window_sze") && error.contains("not allowed"),
        "error should name the option it did not recognise: {error}"
    );
}

/// A configuration built in code never meets the schema the router validates `router.yaml`
/// against, so the check `parse_options` makes over the options as JSON is the only one standing
/// between a misspelled option and being silently ignored.
#[test]
fn a_misspelled_option_is_rejected_without_the_schema() {
    let target: TargetConfig = serde_json::from_value(serde_json::json!({ "window_sze": 50 }))
        .expect("serde keeps an unknown key, having no way to deny one beside a flattened field");

    let errors = Circuits::new(
        SUBGRAPH_PATHS,
        Some(target),
        HashMap::new(),
        subgraph_response_is_failure,
    )
    .err()
    .expect("the unknown option should have been rejected");

    assert!(
        errors.iter().any(|error| error.contains("window_sze")),
        "error should name the option it did not recognise: {errors:?}"
    );
}

#[tokio::test]
async fn connector_sources_are_configured_independently_of_subgraphs() {
    let plugin = harness(
        r#"
        circuit_breaker:
          connector:
            all:
              consecutive_failures: 4
            sources:
              products.api:
                consecutive_failures: 2
        "#,
    )
    .await;

    assert!(
        plugin.subgraphs.layer("products").is_none(),
        "no subgraph configuration was given"
    );
    let source = plugin
        .connectors
        .named
        .get("products.api")
        .expect("configured")
        .as_ref()
        .expect("enabled");
    assert_eq!(source.consecutive_failures.get(), 2);
    assert_eq!(
        plugin
            .connectors
            .all
            .as_ref()
            .expect("configured")
            .consecutive_failures
            .get(),
        4
    );
}

#[tokio::test]
async fn an_empty_configuration_wraps_nothing() {
    let plugin = harness("circuit_breaker: {}").await;

    assert!(plugin.subgraphs.layer("products").is_none());
    assert!(plugin.connectors.layer("products.api").is_none());
}

#[tokio::test]
async fn min_requests_above_the_window_size_is_rejected() {
    let error = plugin_error(
        r#"
        circuit_breaker:
          subgraphs:
            products:
              window_size: 10
              min_requests: 11
        "#,
    )
    .await;

    assert!(
        error.contains("products") && error.contains("min_requests"),
        "error should name the subgraph and the option: {error}"
    );
}

#[tokio::test]
async fn an_out_of_range_failure_rate_threshold_is_rejected() {
    let error = plugin_error(
        r#"
        circuit_breaker:
          all:
            failure_rate_threshold: 1.5
        "#,
    )
    .await;

    assert!(
        error.contains("failure_rate_threshold"),
        "error should name the option: {error}"
    );
}

/// A NaN never reaches the plugin as a number: the router's configuration is a
/// `serde_json::Value`, which cannot hold one, so it arrives as a `null` the schema has no
/// threshold to match it against. It must not be silently read as a threshold either, so this
/// goes through the router's configuration validation rather than the plugin harness, which
/// requires a configuration the schema already accepts.
#[test]
fn a_not_a_number_failure_rate_threshold_is_rejected() {
    let error = config_error(
        r#"
circuit_breaker:
  all:
    failure_rate_threshold: .nan
"#,
    );

    assert!(
        error.contains("failure_rate_threshold"),
        "error should name the option: {error}"
    );
}

#[tokio::test]
async fn a_window_size_above_the_maximum_is_rejected() {
    let error = plugin_error(
        r#"
        circuit_breaker:
          connector:
            sources:
              products.api:
                window_size: 1000001
        "#,
    )
    .await;

    assert!(
        error.contains("products.api") && error.contains("window_size"),
        "error should name the source and the option: {error}"
    );
}

#[tokio::test]
async fn a_zero_open_duration_is_rejected() {
    let error = plugin_error(
        r#"
        circuit_breaker:
          all:
            open_duration: 0s
        "#,
    )
    .await;

    assert!(
        error.contains("circuit_breaker.all") && error.contains("open_duration"),
        "error should name the block and the option: {error}"
    );
}

/// `all` and `connector.all` are different blocks, so an error in one has to say which one it is
/// in — a user sent to the wrong block edits configuration that was already valid.
#[tokio::test]
async fn a_connector_all_block_is_named_apart_from_the_subgraph_one() {
    let error = plugin_error(
        r#"
        circuit_breaker:
          all:
            window_size: 200
          connector:
            all:
              window_size: 5
        "#,
    )
    .await;

    assert!(
        error.contains("circuit_breaker.connector.all"),
        "error should name the connector block: {error}"
    );
    assert!(
        !error.contains("circuit_breaker.all"),
        "the subgraph block is valid and should not be named: {error}"
    );
}

/// Startup reports every invalid block, so a user with several of them fixes them in one pass
/// instead of one per restart — and reports them in a fixed order, which `HashMap` iteration is
/// not.
#[tokio::test]
async fn every_invalid_block_is_reported_in_a_fixed_order() {
    let error = plugin_error(
        r#"
        circuit_breaker:
          subgraphs:
            reviews:
              window_size: 1000001
            products:
              window_size: 10
              min_requests: 11
          connector:
            sources:
              products.api:
                failure_rate_threshold: 1.5
        "#,
    )
    .await;

    let position = |needle: &str| {
        error
            .find(needle)
            .unwrap_or_else(|| panic!("error should name `{needle}`: {error}"))
    };

    // Subgraphs before connector sources, and each set in name order.
    assert!(
        position("circuit_breaker.subgraphs.products")
            < position("circuit_breaker.subgraphs.reviews")
    );
    assert!(
        position("circuit_breaker.subgraphs.reviews")
            < position("circuit_breaker.connector.sources.products.api")
    );
}

// --- subgraphs -----------------------------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn a_subgraph_circuit_opens_after_consecutive_failures_and_recovers_on_a_probe() {
    let plugin = harness(
        r#"
        circuit_breaker:
          all:
            consecutive_failures: 2
            open_duration: 10s
        "#,
    )
    .await;

    let calls = Arc::new(AtomicUsize::new(0));
    let status = Arc::new(std::sync::Mutex::new(StatusCode::INTERNAL_SERVER_ERROR));
    let service = plugin.subgraph_service("products", {
        let calls = calls.clone();
        let status = status.clone();
        move |req: subgraph::Request| {
            let calls = calls.clone();
            let status = status.clone();
            async move {
                calls.fetch_add(1, Ordering::SeqCst);
                let status = *status.lock().expect("not poisoned");
                Ok(subgraph_response(status, &req))
            }
        }
    });

    // Two 5xx responses in a row reach the subgraph and open the circuit.
    for _ in 0..2 {
        let response = service
            .call(subgraph::Request::fake_builder().build())
            .await
            .expect("the subgraph answered");
        assert_eq!(
            response.response.status(),
            StatusCode::INTERNAL_SERVER_ERROR
        );
        assert!(response.response.body().errors.is_empty());
    }
    assert_eq!(calls.load(Ordering::SeqCst), 2);

    // The next request is rejected without reaching the subgraph.
    let response = service
        .call(subgraph::Request::fake_builder().build())
        .await
        .expect("the circuit answered");
    assert_eq!(response.response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        response.response.body().errors[0].extensions["code"],
        Error::CircuitBreakerOpen.code()
    );
    assert_eq!(
        calls.load(Ordering::SeqCst),
        2,
        "the rejected request must not reach the subgraph"
    );

    // Once the circuit has been open for `open_duration`, a single probe is let through. A
    // healthy probe closes the circuit again.
    *status.lock().expect("not poisoned") = StatusCode::OK;
    tokio::time::advance(Duration::from_secs(11)).await;

    let response = service
        .call(subgraph::Request::fake_builder().build())
        .await
        .expect("the probe was answered");
    assert_eq!(response.response.status(), StatusCode::OK);
    assert_eq!(
        calls.load(Ordering::SeqCst),
        3,
        "the probe reached the subgraph"
    );

    let response = service
        .call(subgraph::Request::fake_builder().build())
        .await
        .expect("the subgraph answered");
    assert_eq!(response.response.status(), StatusCode::OK);
    assert_eq!(calls.load(Ordering::SeqCst), 4);
}

#[tokio::test]
async fn a_subgraph_error_counts_against_the_circuit() {
    let plugin = harness(
        r#"
        circuit_breaker:
          all:
            consecutive_failures: 1
        "#,
    )
    .await;

    let service = plugin.subgraph_service("products", |_req: subgraph::Request| async {
        Err::<subgraph::Response, BoxError>("the subgraph is unreachable".into())
    });

    let error = service
        .call(subgraph::Request::fake_builder().build())
        .await
        .expect_err("the subgraph failed");
    assert_eq!(error.to_string(), "the subgraph is unreachable");

    // That single failure was enough to open the circuit.
    let response = service
        .call(subgraph::Request::fake_builder().build())
        .await
        .expect("the circuit answered");
    assert_eq!(response.response.status(), StatusCode::SERVICE_UNAVAILABLE);
}

#[tokio::test]
async fn an_unconfigured_subgraph_keeps_failing_through() {
    let plugin = harness(
        r#"
        circuit_breaker:
          subgraphs:
            products:
              consecutive_failures: 1
        "#,
    )
    .await;

    let service = plugin.subgraph_service("reviews", |req: subgraph::Request| async move {
        Ok(subgraph_response(StatusCode::INTERNAL_SERVER_ERROR, &req))
    });

    for _ in 0..5 {
        let response = service
            .call(subgraph::Request::fake_builder().build())
            .await
            .expect("the subgraph answered");
        assert_eq!(
            response.response.status(),
            StatusCode::INTERNAL_SERVER_ERROR,
            "reviews has no circuit, so nothing should be rejected"
        );
    }
}

/// A 4xx says something about the request, not about the subgraph's health — a subgraph answering
/// `401` for every request is answering. Counting those would take a working subgraph out of
/// service over the router's own callers.
#[tokio::test]
async fn a_4xx_subgraph_response_is_not_a_failure() {
    let plugin = harness(
        r#"
        circuit_breaker:
          all:
            consecutive_failures: 1
        "#,
    )
    .await;

    let service = plugin.subgraph_service("products", |req: subgraph::Request| async move {
        Ok(subgraph_response(StatusCode::UNAUTHORIZED, &req))
    });

    for _ in 0..5 {
        let response = service
            .call(subgraph::Request::fake_builder().build())
            .await
            .expect("the subgraph answered");
        assert_eq!(
            response.response.status(),
            StatusCode::UNAUTHORIZED,
            "a 4xx should not open the circuit"
        );
    }
}

#[tokio::test]
async fn a_subgraph_circuit_is_shared_by_every_service_built_for_it() {
    let plugin = harness(
        r#"
        circuit_breaker:
          all:
            consecutive_failures: 1
        "#,
    )
    .await;

    // Two services for the same subgraph, as the router builds on each schema reload.
    let failing = plugin.subgraph_service("products", |req: subgraph::Request| async move {
        Ok(subgraph_response(StatusCode::INTERNAL_SERVER_ERROR, &req))
    });
    let healthy = plugin.subgraph_service("products", |req: subgraph::Request| async move {
        Ok(subgraph_response(StatusCode::OK, &req))
    });

    failing
        .call(subgraph::Request::fake_builder().build())
        .await
        .expect("the subgraph answered");

    let response = healthy
        .call(subgraph::Request::fake_builder().build())
        .await
        .expect("the circuit answered");
    assert_eq!(
        response.response.status(),
        StatusCode::SERVICE_UNAVAILABLE,
        "the failure recorded by one service should open the circuit for the other"
    );
}

// --- connectors ----------------------------------------------------------------------------

#[tokio::test]
async fn a_connector_circuit_opens_after_consecutive_failures() {
    let plugin = harness(
        r#"
        circuit_breaker:
          connector:
            sources:
              products.api:
                consecutive_failures: 2
        "#,
    )
    .await;

    let calls = Arc::new(AtomicUsize::new(0));
    let mut service = plugin.connector_request_service(
        {
            let calls = calls.clone();
            tower::service_fn(move |req: ConnectorRequest| {
                let calls = calls.clone();
                async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    Ok(connector_response(StatusCode::BAD_GATEWAY, &req))
                }
            })
            .boxed_clone()
        },
        "products.api".to_string(),
    );

    for _ in 0..2 {
        let response = service
            .ready()
            .await
            .expect("ready")
            .call(connector_request())
            .await
            .expect("the source answered");
        assert!(response.transport_result.is_ok());
        assert_eq!(connector_error_code(&response), None);
    }
    assert_eq!(calls.load(Ordering::SeqCst), 2);

    let response = service
        .ready()
        .await
        .expect("ready")
        .call(connector_request())
        .await
        .expect("the circuit answered");
    assert!(matches!(
        response.transport_result,
        Err(Error::CircuitBreakerOpen)
    ));
    assert_eq!(
        connector_error_code(&response),
        Some(Error::CircuitBreakerOpen.code())
    );
    assert_eq!(
        calls.load(Ordering::SeqCst),
        2,
        "the rejected request must not reach the source"
    );
}

#[tokio::test]
async fn an_unconfigured_connector_source_keeps_failing_through() {
    let plugin = harness(
        r#"
        circuit_breaker:
          connector:
            sources:
              products.api:
                consecutive_failures: 1
        "#,
    )
    .await;

    let mut service = plugin.connector_request_service(
        tower::service_fn(|req: ConnectorRequest| async move {
            Ok(connector_response(StatusCode::BAD_GATEWAY, &req))
        })
        .boxed_clone(),
        "reviews.api".to_string(),
    );

    for _ in 0..5 {
        let response = service
            .ready()
            .await
            .expect("ready")
            .call(connector_request())
            .await
            .expect("the source answered");
        assert!(
            response.transport_result.is_ok(),
            "reviews.api has no circuit, so nothing should be rejected"
        );
    }
}

/// The connector counterpart of the subgraph recovery test: the probe and the close behind it are
/// the connector hook's own code path, not one the subgraph tests reach.
#[tokio::test(start_paused = true)]
async fn a_connector_circuit_opens_and_recovers_on_a_probe() {
    let plugin = harness(
        r#"
        circuit_breaker:
          connector:
            all:
              consecutive_failures: 2
              open_duration: 10s
        "#,
    )
    .await;

    let status = Arc::new(std::sync::Mutex::new(StatusCode::BAD_GATEWAY));
    let (mut service, calls) = connector_service(&plugin, "products.api", {
        let status = status.clone();
        move |req: &ConnectorRequest| {
            let status = *status.lock().expect("not poisoned");
            connector_response(status, req)
        }
    });

    for _ in 0..2 {
        let response = service
            .ready()
            .await
            .expect("ready")
            .call(connector_request())
            .await
            .expect("the source answered");
        assert!(response.transport_result.is_ok());
    }
    assert_eq!(calls.load(Ordering::SeqCst), 2);

    let response = service
        .ready()
        .await
        .expect("ready")
        .call(connector_request())
        .await
        .expect("the circuit answered");
    assert!(matches!(
        response.transport_result,
        Err(Error::CircuitBreakerOpen)
    ));
    assert_eq!(
        calls.load(Ordering::SeqCst),
        2,
        "the rejected request must not reach the source"
    );

    // Once the circuit has been open for `open_duration`, a single probe is let through, and a
    // healthy probe closes the circuit again.
    *status.lock().expect("not poisoned") = StatusCode::OK;
    tokio::time::advance(Duration::from_secs(11)).await;

    for expected_calls in [3, 4] {
        let response = service
            .ready()
            .await
            .expect("ready")
            .call(connector_request())
            .await
            .expect("the source answered");
        assert!(response.transport_result.is_ok());
        assert_eq!(connector_error_code(&response), None);
        assert_eq!(calls.load(Ordering::SeqCst), expected_calls);
    }
}

#[tokio::test]
async fn a_connector_circuit_is_shared_by_every_service_built_for_it() {
    let plugin = harness(
        r#"
        circuit_breaker:
          connector:
            all:
              consecutive_failures: 1
        "#,
    )
    .await;

    // Two services for the same source, as the router builds on each schema reload.
    let (mut failing, _) = connector_service(&plugin, "products.api", |req: &ConnectorRequest| {
        connector_response(StatusCode::BAD_GATEWAY, req)
    });
    let (mut healthy, _) = connector_service(&plugin, "products.api", |req: &ConnectorRequest| {
        connector_response(StatusCode::OK, req)
    });

    failing
        .ready()
        .await
        .expect("ready")
        .call(connector_request())
        .await
        .expect("the source answered");

    let response = healthy
        .ready()
        .await
        .expect("ready")
        .call(connector_request())
        .await
        .expect("the circuit answered");
    assert!(
        matches!(response.transport_result, Err(Error::CircuitBreakerOpen)),
        "the failure recorded by one service should open the circuit for the other"
    );
}

#[tokio::test]
async fn a_4xx_connector_response_is_not_a_failure() {
    let plugin = harness(
        r#"
        circuit_breaker:
          connector:
            all:
              consecutive_failures: 1
        "#,
    )
    .await;

    let (mut service, _) = connector_service(&plugin, "products.api", |req: &ConnectorRequest| {
        connector_response(StatusCode::NOT_FOUND, req)
    });

    for _ in 0..5 {
        let response = service
            .ready()
            .await
            .expect("ready")
            .call(connector_request())
            .await
            .expect("the source answered");
        assert!(
            response.transport_result.is_ok(),
            "a 4xx should not open the circuit"
        );
    }
}

/// A request the router turned away never gave the source a chance to answer, so it says nothing
/// about the source's health. Counting `RequestLimitExceeded` would let one oversized operation
/// hitting a `max_requests` cap open the circuit for every other operation using the source —
/// and the connector request service raises it under this plugin, so the classifier is the only
/// thing standing in the way.
#[tokio::test]
async fn a_request_the_router_turned_away_is_not_a_source_failure() {
    for error in [
        Error::RequestLimitExceeded,
        Error::RateLimited,
        Error::GatewayTimeout,
    ] {
        let plugin = harness(
            r#"
            circuit_breaker:
              connector:
                all:
                  consecutive_failures: 1
            "#,
        )
        .await;

        let (mut service, calls) = connector_service(&plugin, "products.api", {
            let error = error.clone();
            move |req: &ConnectorRequest| connector_transport_error(error.clone(), req)
        });

        for _ in 0..3 {
            let response = service
                .ready()
                .await
                .expect("ready")
                .call(connector_request())
                .await
                .expect("the service answered");
            assert_ne!(
                connector_error_code(&response),
                Some(Error::CircuitBreakerOpen.code()),
                "{error:?} should not open the circuit"
            );
        }
        assert_eq!(
            calls.load(Ordering::SeqCst),
            3,
            "no request should have been rejected for {error:?}"
        );
    }
}

#[tokio::test]
async fn a_connector_transport_failure_is_a_source_failure() {
    let plugin = harness(
        r#"
        circuit_breaker:
          connector:
            all:
              consecutive_failures: 1
        "#,
    )
    .await;

    let (mut service, calls) =
        connector_service(&plugin, "products.api", |req: &ConnectorRequest| {
            connector_transport_error(Error::TransportFailure("connection refused".into()), req)
        });

    service
        .ready()
        .await
        .expect("ready")
        .call(connector_request())
        .await
        .expect("the service answered");

    let response = service
        .ready()
        .await
        .expect("ready")
        .call(connector_request())
        .await
        .expect("the circuit answered");
    assert_eq!(
        connector_error_code(&response),
        Some(Error::CircuitBreakerOpen.code())
    );
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "the rejected request must not reach the source"
    );
}

#[tokio::test]
async fn a_mapping_only_connector_response_is_not_a_failure() {
    let plugin = harness(
        r#"
        circuit_breaker:
          connector:
            all:
              consecutive_failures: 1
        "#,
    )
    .await;

    let mut service = plugin.connector_request_service(
        tower::service_fn(|req: ConnectorRequest| async move {
            let mut response = connector_response(StatusCode::OK, &req);
            response.transport_result = Ok(TransportResponse::MappingOnly);
            Ok(response)
        })
        .boxed_clone(),
        "products.api".to_string(),
    );

    for _ in 0..3 {
        let response = service
            .ready()
            .await
            .expect("ready")
            .call(connector_request())
            .await
            .expect("the connector answered");
        assert!(matches!(
            response.transport_result,
            Ok(TransportResponse::MappingOnly)
        ));
    }
}

// --- through the whole router --------------------------------------------------------------

/// Exercises the plugin the way the router builds it: registered by `create_plugins`, ordered
/// against the other plugins, and reached through a real supergraph service. The unit tests above
/// call the plugin's hooks directly, so nothing there would notice the plugin failing to be
/// registered or ending up on the wrong side of another plugin.
#[tokio::test]
async fn a_circuit_opens_through_a_real_supergraph_service() -> Result<(), BoxError> {
    let service = crate::TestHarness::builder()
        .configuration_json(serde_json::json!({
            "circuit_breaker": {
                "subgraphs": {
                    "products": {
                        "consecutive_failures": 1,
                    },
                },
            },
            // The circuit breaker attaches its error to the subgraph response, so without this
            // the error is redacted like any other subgraph error.
            "include_subgraph_errors": { "all": true },
        }))?
        .subgraph_hook(|name, service| {
            if name != "products" {
                return service;
            }
            tower::service_fn(|request: subgraph::Request| async move {
                Ok(subgraph_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    &request,
                ))
            })
            .boxed_clone()
        })
        .build_supergraph()
        .await?;

    let query = "{ topProducts { name } }";
    let error_codes = |response: &crate::graphql::Response| -> Vec<String> {
        response
            .errors
            .iter()
            .filter_map(|error| error.extensions.get("code")?.as_str().map(str::to_string))
            .collect()
    };

    // The first request reaches the failing subgraph and opens its circuit.
    let response = service
        .clone()
        .oneshot(
            crate::services::supergraph::Request::fake_builder()
                .query(query)
                .build()?,
        )
        .await?
        .next_response()
        .await
        .expect("a response");
    assert!(
        !error_codes(&response).contains(&Error::CircuitBreakerOpen.code().to_string()),
        "the first request should have reached the subgraph: {response:?}"
    );

    // The next one is rejected by the open circuit.
    let response = service
        .oneshot(
            crate::services::supergraph::Request::fake_builder()
                .query(query)
                .build()?,
        )
        .await?
        .next_response()
        .await
        .expect("a response");
    assert!(
        error_codes(&response).contains(&Error::CircuitBreakerOpen.code().to_string()),
        "the second request should have been rejected by the open circuit: {response:?}"
    );

    Ok(())
}

// --- telemetry -----------------------------------------------------------------------------

#[tokio::test]
async fn a_subgraph_circuit_records_the_requests_it_accepts_and_rejects() {
    async {
        let plugin = harness(
            r#"
            circuit_breaker:
              all:
                consecutive_failures: 1
            "#,
        )
        .await;

        let service = plugin.subgraph_service("products", |req: subgraph::Request| async move {
            Ok(subgraph_response(StatusCode::INTERNAL_SERVER_ERROR, &req))
        });

        // The first request is accepted and fails, opening the circuit; the second is rejected.
        for _ in 0..2 {
            let _ = service
                .call(subgraph::Request::fake_builder().build())
                .await;
        }

        assert_counter!(
            "apollo.qos.circuit_breaker.requests",
            1,
            "apollo.qos.circuit_breaker.name" = "products",
            "apollo.qos.circuit_breaker.status" = "accepted"
        );
        assert_counter!(
            "apollo.qos.circuit_breaker.requests",
            1,
            "apollo.qos.circuit_breaker.name" = "products",
            "apollo.qos.circuit_breaker.status" = "rejected"
        );
        assert_counter!(
            "apollo.qos.circuit_breaker.state.transitions",
            1,
            "apollo.qos.circuit_breaker.name" = "products",
            "apollo.qos.circuit_breaker.state.from" = "closed",
            "apollo.qos.circuit_breaker.state.to" = "open"
        );
    }
    .with_metrics()
    .await;
}

/// A connector source's circuit is named by its source key, so an operator can tell which source
/// opened. Recorded by apollo-qos, which only sees the name the plugin gave the layer.
#[tokio::test]
async fn a_connector_circuit_records_the_requests_it_accepts_and_rejects() {
    async {
        let plugin = harness(
            r#"
            circuit_breaker:
              connector:
                all:
                  consecutive_failures: 1
            "#,
        )
        .await;

        let (mut service, _) =
            connector_service(&plugin, "products.api", |req: &ConnectorRequest| {
                connector_response(StatusCode::BAD_GATEWAY, req)
            });

        // The first request is accepted and fails, opening the circuit; the second is rejected.
        for _ in 0..2 {
            let _ = service
                .ready()
                .await
                .expect("ready")
                .call(connector_request())
                .await;
        }

        assert_counter!(
            "apollo.qos.circuit_breaker.requests",
            1,
            "apollo.qos.circuit_breaker.name" = "products.api",
            "apollo.qos.circuit_breaker.status" = "accepted"
        );
        assert_counter!(
            "apollo.qos.circuit_breaker.requests",
            1,
            "apollo.qos.circuit_breaker.name" = "products.api",
            "apollo.qos.circuit_breaker.status" = "rejected"
        );
        assert_counter!(
            "apollo.qos.circuit_breaker.state.transitions",
            1,
            "apollo.qos.circuit_breaker.name" = "products.api",
            "apollo.qos.circuit_breaker.state.from" = "closed",
            "apollo.qos.circuit_breaker.state.to" = "open"
        );
    }
    .with_metrics()
    .await;
}
