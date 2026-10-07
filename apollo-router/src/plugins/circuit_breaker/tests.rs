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
use futures::future::BoxFuture;
use http::StatusCode;
use tower::Layer;
use tower::Service;
use tower::ServiceExt;

use super::*;
use crate::Context;
use crate::metrics::FutureMetricsExt as _;
use crate::plugins::connectors::request_limit::RequestLimits;
use crate::plugins::test::PluginTestHarness;
use crate::plugins::test::ServiceHandle;
use crate::services::connector::request_service::Request as ConnectorRequest;
use crate::services::connector::request_service::Response as ConnectorResponse;
use crate::services::connector::request_service::TransportOutcome;
use crate::services::http::HttpRequest as SourceHttpRequest;
use crate::services::http::HttpResponse as SourceHttpResponse;
use crate::services::router;
use crate::services::subgraph;

// --- helpers -------------------------------------------------------------------------------

async fn harness(config: &str) -> PluginTestHarness<CircuitBreaker> {
    PluginTestHarness::builder()
        .config(config)
        .build()
        .await
        .expect("plugin should be configured")
}

/// `service` behind the circuit for subgraph `name`, as `pipeline::stages` puts it there.
fn behind_subgraph_circuit(
    plugin: &CircuitBreaker,
    name: &str,
    service: subgraph::BoxCloneService,
) -> subgraph::BoxCloneService {
    plugin
        .subgraph_circuit_layer(name)
        .layer(service)
        .boxed_clone()
}

/// `service` behind the circuit for connector source `source`, as `pipeline::stages` puts it
/// there.
fn behind_source_circuit(
    plugin: &CircuitBreaker,
    source: &str,
    service: connector::request_service::BoxCloneService,
) -> connector::request_service::BoxCloneService {
    plugin
        .connector_source_circuit_layer(source)
        .layer(service)
        .boxed_clone()
}

/// A subgraph answering each request with `response_fn`, behind the circuit for subgraph `name`.
fn protected_subgraph<F>(
    plugin: &CircuitBreaker,
    name: &str,
    response_fn: impl Fn(subgraph::Request) -> F + Send + Sync + Clone + 'static,
) -> ServiceHandle<subgraph::Request, subgraph::BoxCloneService>
where
    F: Future<Output = Result<subgraph::Response, BoxError>> + Send + 'static,
{
    ServiceHandle::new(behind_subgraph_circuit(
        plugin,
        name,
        tower::service_fn(response_fn).boxed_clone(),
    ))
}

/// Answers each request the mock behind `handle` receives with `answer`, as the target would.
/// Each answer runs on its own task, so a slow or hanging answer doesn't hold up the next request.
fn serve<Req, Res>(
    mut handle: tower_test::mock::Handle<Req, Res>,
    answer: impl Fn(Req) -> BoxFuture<'static, Result<Res, BoxError>> + Send + 'static,
) where
    Req: Send + 'static,
    Res: Send + 'static,
{
    tokio::spawn(async move {
        while let Some((request, response)) = handle.next_request().await {
            let answer = answer(request);
            tokio::spawn(async move {
                match answer.await {
                    Ok(answer) => response.send_response(answer),
                    Err(error) => response.send_error(error),
                }
            });
        }
    });
}

/// The placed service stack for subgraph `products`, built by the pipeline's own stage builders
/// with `plugins` configured. The mock beneath every plugin hook stands in for the subgraph, and
/// `answer` answers what it receives.
async fn subgraph_stack(
    plugins: &[(&str, serde_json::Value)],
    answer: impl Fn(subgraph::Request) -> BoxFuture<'static, Result<subgraph::Response, BoxError>>
    + Send
    + Sync
    + 'static,
) -> subgraph::BoxCloneService {
    let (services, handle) =
        crate::pipeline::tests::stage_stack::subgraph_services("products", plugins).await;
    serve(handle, answer);
    services
        .get("products")
        .expect("the subgraph has a service")
}

/// The connector counterpart of [`subgraph_stack`], for source `products.api`. The mock is the
/// source's HTTP client, so `answer` answers the HTTP requests the source receives.
async fn source_stack(
    plugins: &[(&str, serde_json::Value)],
    answer: impl Fn(SourceHttpRequest) -> BoxFuture<'static, Result<SourceHttpResponse, BoxError>>
    + Send
    + Sync
    + 'static,
) -> connector::request_service::BoxCloneService {
    let (service, handle) =
        crate::pipeline::tests::stage_stack::source_services("products.api", plugins).await;
    serve(handle, answer);
    service()
}

/// [`subgraph_stack`] with circuit breaking and traffic shaping configured: `circuit_breaker` and
/// `traffic_shaping` are the bodies of their blocks.
async fn placed_subgraph(
    circuit_breaker: serde_json::Value,
    traffic_shaping: serde_json::Value,
    answer: impl Fn(subgraph::Request) -> BoxFuture<'static, Result<subgraph::Response, BoxError>>
    + Send
    + Sync
    + 'static,
) -> subgraph::BoxCloneService {
    subgraph_stack(
        &[
            ("apollo.traffic_shaping", traffic_shaping),
            ("apollo.circuit_breaker", circuit_breaker),
        ],
        answer,
    )
    .await
}

/// The connector counterpart of [`placed_subgraph`], for source `products.api`.
async fn placed_source(
    circuit_breaker: serde_json::Value,
    traffic_shaping: serde_json::Value,
    answer: impl Fn(SourceHttpRequest) -> BoxFuture<'static, Result<SourceHttpResponse, BoxError>>
    + Send
    + Sync
    + 'static,
) -> connector::request_service::BoxCloneService {
    source_stack(
        &[
            ("apollo.traffic_shaping", traffic_shaping),
            ("apollo.circuit_breaker", circuit_breaker),
        ],
        answer,
    )
    .await
}

/// A stand-in target for the placed stacks: counts the requests reaching it and answers each
/// with the status it is currently set to.
#[derive(Clone)]
struct Target {
    calls: Arc<AtomicUsize>,
    status: Arc<std::sync::Mutex<StatusCode>>,
}

impl Target {
    fn answering(status: StatusCode) -> Self {
        Self {
            calls: Default::default(),
            status: Arc::new(std::sync::Mutex::new(status)),
        }
    }

    fn set(&self, status: StatusCode) {
        *self.status.lock().expect("not poisoned") = status;
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }

    /// Answers as the subgraph, after `delay`.
    fn subgraph(
        &self,
        delay: Duration,
    ) -> impl Fn(subgraph::Request) -> BoxFuture<'static, Result<subgraph::Response, BoxError>>
    + Send
    + Sync
    + 'static {
        let target = self.clone();
        move |request| {
            target.calls.fetch_add(1, Ordering::SeqCst);
            let status = *target.status.lock().expect("not poisoned");
            Box::pin(async move {
                tokio::time::sleep(delay).await;
                Ok(subgraph_response(status, &request))
            })
        }
    }

    /// Answers as the connector source.
    fn source(
        &self,
    ) -> impl Fn(SourceHttpRequest) -> BoxFuture<'static, Result<SourceHttpResponse, BoxError>>
    + Send
    + Sync
    + 'static {
        let target = self.clone();
        move |request| {
            target.calls.fetch_add(1, Ordering::SeqCst);
            let status = *target.status.lock().expect("not poisoned");
            let http_response = http::Response::builder()
                .status(status)
                .body(router::body::empty())
                .unwrap();
            Box::pin(async move {
                Ok(SourceHttpResponse {
                    http_response,
                    context: request.context,
                })
            })
        }
    }
}

/// The code of the first error on `response`, if it has one.
fn subgraph_error_code(response: &subgraph::Response) -> Option<String> {
    response.response.body().errors.first()?.extension_code()
}

/// A subgraph service that counts the requests reaching it and never answers any of them.
fn hanging_subgraph() -> (subgraph::BoxCloneService, Arc<AtomicUsize>) {
    let calls = Arc::new(AtomicUsize::new(0));
    let service = {
        let calls = calls.clone();
        tower::service_fn(move |_req: subgraph::Request| {
            calls.fetch_add(1, Ordering::SeqCst);
            std::future::pending::<Result<subgraph::Response, BoxError>>()
        })
        .boxed_clone()
    };
    (service, calls)
}

/// The lines of the configuration a rendered validation error points at, one per diagnostic, in
/// the order they were reported.
///
/// Each diagnostic carries a span into the document the operator wrote, which miette renders as a
/// `[line:column]` header above the source snippet. Reading the line back out of the report is
/// what lets a test assert which block a message sends the operator to, rather than only that the
/// message mentions an option name — a message naming `min_requests` is no use if it points at
/// the wrong subgraph's block.
///
/// Both of miette's themes are matched. It renders that header with box-drawing characters or
/// with ASCII depending on what the terminal it is printed to looks like it can show — a
/// decision made from `LC_ALL`/`LC_CTYPE`/`LANG` and `TERM` at the moment the report is
/// formatted — so a test that knows only one of them passes or fails on the locale of whoever
/// runs it.
fn error_lines(error: &str, yaml: &str) -> Vec<String> {
    /// The header miette renders above each source snippet: `╭─[` under its unicode theme,
    /// `,-[` under the ASCII one.
    const SNIPPET_HEADERS: [&str; 2] = ["\u{256d}\u{2500}[", ",-["];

    let lines: Vec<&str> = yaml.lines().collect();

    error
        .lines()
        .filter_map(|line| {
            let (_, rest) = SNIPPET_HEADERS
                .iter()
                .find_map(|header| line.split_once(*header))?;
            let (position, _) = rest.split_once(']')?;
            let (number, _) = position.split_once(':')?;
            let number: usize = number.parse().ok()?;
            // miette numbers lines from one.
            Some(lines[number - 1].trim().to_string())
        })
        .collect()
}

/// The error the router's configuration parse gives for `yaml`: the one startup, a reload and
/// `router config validate` all go through, so this is what an operator is shown.
///
/// It covers the rules the configuration schema expresses and the ones apollo-qos declares on
/// [`CircuitBreakerConfig`] as `#[config(validate = …)]` functions, such as `min_requests`
/// against `window_size`.
/// `yaml` starts at column zero, like a real `router.yaml`.
fn config_error(yaml: &str) -> String {
    yaml.parse::<crate::Configuration>()
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
        transport_outcome: TransportOutcome::Response(HttpResponse { inner: parts }),
        mapped_response: MappedResponse::Data {
            data: Default::default(),
            problems: Vec::new(),
            declared_errors: vec![],
            key: response_key(),
        },
        answered_by_router: false,
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

/// A connector service behind the circuit for `source_name`, counting the requests that reach it
/// and answering each with `response_fn`.
fn connector_service(
    plugin: &CircuitBreaker,
    source_name: &str,
    response_fn: impl Fn(&ConnectorRequest) -> ConnectorResponse + Clone + Send + Sync + 'static,
) -> (
    connector::request_service::BoxCloneService,
    Arc<AtomicUsize>,
) {
    let calls = Arc::new(AtomicUsize::new(0));
    let service = behind_source_circuit(plugin, source_name, {
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
    });
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
    assert_eq!(products.consecutive_failures.get(), 2);

    // Every option `products` left out is at the apollo-qos default, not at the value `all` gave
    // it: a subgraph's own block replaces `all` instead of overriding it option by option.
    assert_eq!(products.window_size.get(), 100);
    assert_eq!(products.min_requests.get(), 10);
    assert_eq!(products.failure_rate_threshold, 0.5);
    assert_eq!(*products.open_duration, Duration::from_secs(30));

    // A subgraph with no entry of its own gets `all` untouched.
    assert_eq!(plugin.subgraphs.all.window_size.get(), 200);
    assert_eq!(plugin.subgraphs.all.consecutive_failures.get(), 5);
}

/// Every target is protected once the plugin is configured at all, so a configuration that names
/// no target still wraps each of them with the apollo-qos defaults. There is no per-target switch:
/// a deployment that wants no circuit breaking leaves the `circuit_breaker` block out entirely,
/// which is what keeps the plugin from being created in the first place.
#[tokio::test]
async fn an_empty_configuration_protects_every_target_with_the_defaults() {
    let plugin = harness("circuit_breaker: {}").await;

    assert_eq!(plugin.subgraphs.all.window_size.get(), 100);
    assert_eq!(plugin.connectors.all.window_size.get(), 100);

    // Asking for a target with no entry of its own still yields a circuit.
    let _ = plugin.subgraphs.layer("products");
    let _ = plugin.connectors.layer("products.api");
}

/// `open_duration` is the option to watch when the block's shape changes: a value parsed by a
/// `Deserialize` impl of its own — `1m` into a duration, here — is the kind that goes wrong on the
/// way from the document into [`CircuitBreakerConfig`].
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

    let products = plugin.subgraphs.named.get("products").expect("configured");
    assert_eq!(products.failure_rate_threshold, 0.8);
    assert_eq!(products.window_size.get(), 50);
    assert_eq!(products.min_requests.get(), 20);
    assert_eq!(*products.open_duration, Duration::from_secs(60));
    assert_eq!(products.consecutive_failures.get(), 3);
}

/// The router's own configuration schema carries apollo-qos's option names, so a misspelling is
/// caught against the operator's file — the one check that can point at the offending key in the
/// document rather than name it in a message.
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

    // No subgraph configuration was given, so every subgraph is at the apollo-qos defaults rather
    // than at anything the connector block said.
    assert_eq!(plugin.subgraphs.all.consecutive_failures.get(), 5);

    let source = plugin
        .connectors
        .named
        .get("products.api")
        .expect("configured");
    assert_eq!(source.consecutive_failures.get(), 2);
    assert_eq!(plugin.connectors.all.consecutive_failures.get(), 4);
}

/// The rule apollo-qos declares between two options stops startup, and the error sends the
/// operator to the offending key: `circuit_breaker.subgraphs.products.min_requests`, rather than
/// `reviews`' block, which is valid.
#[test]
fn min_requests_above_the_window_size_stops_startup_at_the_offending_key() {
    let yaml = r#"
circuit_breaker:
  subgraphs:
    reviews:
      window_size: 100
      min_requests: 10
    products:
      window_size: 10
      min_requests: 11
"#;
    let error = config_error(yaml);

    assert!(
        error.contains("min_requests (11) must not exceed window_size (10)"),
        "error should carry apollo-qos's message: {error}"
    );
    assert_eq!(error_lines(&error, yaml), ["min_requests: 11"]);
}

/// The bound is inclusive: a block asking for as many requests as its window holds is valid.
#[test]
fn min_requests_equal_to_the_window_size_is_accepted() {
    let yaml = r#"
circuit_breaker:
  subgraphs:
    products:
      window_size: 10
      min_requests: 10
"#;
    yaml.parse::<crate::Configuration>()
        .expect("min_requests may equal window_size");
}

#[test]
fn an_out_of_range_failure_rate_threshold_is_rejected() {
    let yaml = r#"
circuit_breaker:
  all:
    failure_rate_threshold: 1.5
"#;
    let error = config_error(yaml);

    assert!(
        error.contains("failure_rate_threshold"),
        "error should name the option: {error}"
    );
    assert_eq!(error_lines(&error, yaml), ["failure_rate_threshold: 1.5"]);
}

/// A NaN never reaches the plugin as a number: the router's configuration is a
/// `serde_json::Value`, which cannot hold one, so it arrives as a `null` the schema has no
/// threshold to match it against. It must not be silently read as a threshold either.
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

#[test]
fn a_window_size_above_the_maximum_is_rejected() {
    let yaml = r#"
circuit_breaker:
  connector:
    sources:
      products.api:
        window_size: 1000001
"#;
    let error = config_error(yaml);

    assert!(
        error.contains("window_size"),
        "error should name the option: {error}"
    );
    assert_eq!(error_lines(&error, yaml), ["window_size: 1000001"]);
}

#[test]
fn a_zero_open_duration_is_rejected() {
    let yaml = r#"
circuit_breaker:
  all:
    open_duration: 0s
"#;
    let error = config_error(yaml);

    assert!(
        error.contains("open_duration"),
        "error should name the option: {error}"
    );
    assert_eq!(error_lines(&error, yaml), ["open_duration: 0s"]);
}

/// `all` and `connector.all` are different blocks, so an error in one has to point at that one — a
/// user sent to the wrong block edits configuration that was already valid.
#[test]
fn a_connector_all_block_is_reported_apart_from_the_subgraph_one() {
    let yaml = r#"
circuit_breaker:
  all:
    window_size: 200
    min_requests: 20
  connector:
    all:
      window_size: 5
      min_requests: 10
"#;
    let error = config_error(yaml);

    // The subgraph `all` block is valid, so the only line pointed at is in the connector one.
    assert_eq!(error_lines(&error, yaml), ["min_requests: 10"]);
}

/// Every invalid block is reported, so a user with several of them fixes them in one pass instead
/// of one per restart, each pointing at its own target. Blocks within one map are reported in
/// the map's order, which is not fixed, so only the set is asserted.
#[test]
fn every_invalid_block_is_reported() {
    let yaml = r#"
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
"#;
    let error = config_error(yaml);

    let mut lines = error_lines(&error, yaml);
    lines.sort();
    assert_eq!(
        lines,
        [
            "failure_rate_threshold: 1.5",
            "min_requests: 11",
            "window_size: 1000001",
        ]
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
    let service = protected_subgraph(&plugin, "products", {
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

    let service = protected_subgraph(&plugin, "products", |_req: subgraph::Request| async {
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

/// A subgraph with no entry of its own is governed by `all`, not by another subgraph's block: the
/// two circuits have to be separate pieces of state, or one subgraph's failures would be counted
/// against the other's.
#[tokio::test]
async fn a_subgraph_with_no_entry_of_its_own_follows_all() {
    let plugin = harness(
        r#"
        circuit_breaker:
          all:
            consecutive_failures: 4
          subgraphs:
            products:
              consecutive_failures: 1
        "#,
    )
    .await;

    let service = protected_subgraph(&plugin, "reviews", |req: subgraph::Request| async move {
        Ok(subgraph_response(StatusCode::INTERNAL_SERVER_ERROR, &req))
    });

    // `products` would have opened on its first failure; `reviews` takes `all`'s four.
    for failure in 1..=4 {
        let response = service
            .call(subgraph::Request::fake_builder().build())
            .await
            .expect("the subgraph answered");
        assert_eq!(
            response.response.status(),
            StatusCode::INTERNAL_SERVER_ERROR,
            "reviews should still be reaching the subgraph after {failure} failures"
        );
    }

    let response = service
        .call(subgraph::Request::fake_builder().build())
        .await
        .expect("the circuit answered");
    assert_eq!(
        response.response.status(),
        StatusCode::SERVICE_UNAVAILABLE,
        "the fifth request should meet the circuit `all`'s four failures opened"
    );
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

    let service = protected_subgraph(&plugin, "products", |req: subgraph::Request| async move {
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

    // Two services for the same subgraph from the same plugin instance: whatever asks for them,
    // they have to share one circuit. A reload builds a new plugin instance, and fresh circuits.
    let failing = protected_subgraph(&plugin, "products", |req: subgraph::Request| async move {
        Ok(subgraph_response(StatusCode::INTERNAL_SERVER_ERROR, &req))
    });
    let healthy = protected_subgraph(&plugin, "products", |req: subgraph::Request| async move {
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

/// A subgraph that stops answering never returns an error of its own: the only thing that ends
/// the request is the subgraph's timeout, beneath the circuit, and the `504` it is rendered into
/// is what the circuit counts.
#[tokio::test(start_paused = true)]
async fn a_subgraph_that_stops_answering_opens_its_circuit_at_its_timeout() {
    let target = Target::answering(StatusCode::OK);
    let service = placed_subgraph(
        serde_json::json!({ "all": { "consecutive_failures": 1 } }),
        serde_json::json!({ "all": { "timeout": "1s" } }),
        target.subgraph(Duration::from_secs(3600)),
    )
    .await;

    let response = service
        .clone()
        .oneshot(subgraph::Request::fake_builder().build())
        .await
        .expect("the timeout answered");
    assert_eq!(response.response.status(), StatusCode::GATEWAY_TIMEOUT);

    // That timeout was the circuit's one failure.
    let response = service
        .oneshot(subgraph::Request::fake_builder().build())
        .await
        .expect("the circuit answered");
    assert_eq!(response.response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        subgraph_error_code(&response).as_deref(),
        Some(Error::CircuitBreakerOpen.code())
    );
    assert_eq!(
        target.calls(),
        1,
        "the rejected request must not reach the subgraph"
    );
}

/// A `429` says the subgraph is overloaded, which is what a circuit is for.
#[tokio::test]
async fn a_429_from_the_subgraph_is_a_failure() {
    let plugin = harness(
        r#"
        circuit_breaker:
          all:
            consecutive_failures: 1
        "#,
    )
    .await;

    let service = protected_subgraph(&plugin, "products", |req: subgraph::Request| async move {
        Ok(subgraph_response(StatusCode::TOO_MANY_REQUESTS, &req))
    });

    let response = service
        .call(subgraph::Request::fake_builder().build())
        .await
        .expect("the subgraph answered");
    assert_eq!(response.response.status(), StatusCode::TOO_MANY_REQUESTS);

    let response = service
        .call(subgraph::Request::fake_builder().build())
        .await
        .expect("the circuit answered");
    assert_eq!(response.response.status(), StatusCode::SERVICE_UNAVAILABLE);
}

/// Traffic shaping's rate limit is admission: it sits above the circuit, so a request it sheds
/// never reaches the circuit and is recorded as neither a failure nor a success. Between two
/// failures, a shed request must not reset the count of consecutive failures, as a success would,
/// or open the circuit on its own, as a failure would.
#[tokio::test(start_paused = true)]
async fn a_request_the_rate_limit_sheds_is_not_recorded() {
    let target = Target::answering(StatusCode::INTERNAL_SERVER_ERROR);
    let service = placed_subgraph(
        serde_json::json!({ "all": { "consecutive_failures": 2, "open_duration": "1h" } }),
        serde_json::json!({
            "all": { "global_rate_limit": { "capacity": 1, "interval": "10s" } }
        }),
        target.subgraph(Duration::ZERO),
    )
    .await;
    let call = || {
        service
            .clone()
            .oneshot(subgraph::Request::fake_builder().build())
    };

    let response = call().await.expect("the subgraph answered");
    assert_eq!(
        response.response.status(),
        StatusCode::INTERNAL_SERVER_ERROR
    );

    let response = call().await.expect("the rate limit answered");
    assert_eq!(
        subgraph_error_code(&response).as_deref(),
        Some("REQUEST_RATE_LIMITED"),
        "a failure counted for the shed request would have opened the circuit"
    );

    tokio::time::advance(Duration::from_secs(10)).await;
    let response = call().await.expect("the subgraph answered");
    assert_eq!(
        response.response.status(),
        StatusCode::INTERNAL_SERVER_ERROR
    );
    assert_eq!(target.calls(), 2);

    // The two failures were consecutive, so the circuit is open: a success recorded for the shed
    // request in between would have kept it closed.
    tokio::time::advance(Duration::from_secs(10)).await;
    let response = call().await.expect("the circuit answered");
    assert_eq!(
        subgraph_error_code(&response).as_deref(),
        Some(Error::CircuitBreakerOpen.code())
    );
    assert_eq!(target.calls(), 2);
}

/// A request shed while the circuit waits for its recovery probe is not the probe: it neither
/// reopens the circuit nor closes it, so the next admitted request still probes the subgraph.
#[tokio::test(start_paused = true)]
async fn a_request_the_rate_limit_sheds_does_not_take_the_recovery_probe() {
    let target = Target::answering(StatusCode::INTERNAL_SERVER_ERROR);
    let service = placed_subgraph(
        serde_json::json!({ "all": { "consecutive_failures": 1, "open_duration": "10s" } }),
        serde_json::json!({
            "all": { "global_rate_limit": { "capacity": 1, "interval": "15s" } }
        }),
        target.subgraph(Duration::ZERO),
    )
    .await;
    let call = || {
        service
            .clone()
            .oneshot(subgraph::Request::fake_builder().build())
    };

    // Fails, and opens the circuit for ten seconds.
    call().await.expect("the subgraph answered");

    // The circuit is ready for a probe, but the rate limit is still spent until fifteen seconds.
    tokio::time::advance(Duration::from_secs(11)).await;
    let response = call().await.expect("the rate limit answered");
    assert_eq!(
        subgraph_error_code(&response).as_deref(),
        Some("REQUEST_RATE_LIMITED")
    );

    // Had the shed request counted as a failed probe, the circuit would be open again until
    // twenty-one seconds.
    target.set(StatusCode::OK);
    tokio::time::advance(Duration::from_secs(5)).await;
    let response = call().await.expect("the probe was answered");
    assert_eq!(response.response.status(), StatusCode::OK);
    assert_eq!(target.calls(), 2, "the probe reached the subgraph");
}

/// Deduplication sits above the circuit, so identical requests in flight together are one call to
/// the subgraph and one sample, and every one of them receives the open circuit's rejection.
#[tokio::test(start_paused = true)]
async fn requests_joined_by_deduplication_are_one_sample_and_share_the_rejection() {
    let target = Target::answering(StatusCode::INTERNAL_SERVER_ERROR);
    let service = placed_subgraph(
        serde_json::json!({ "all": { "consecutive_failures": 2, "open_duration": "1h" } }),
        serde_json::json!({ "all": { "deduplicate_query": true } }),
        target.subgraph(Duration::from_millis(100)),
    )
    .await;
    let call = || {
        service
            .clone()
            .oneshot(subgraph::Request::fake_builder().build())
    };

    let (first, second) = tokio::join!(call(), call());
    for response in [first, second] {
        assert_eq!(
            response.expect("answered").response.status(),
            StatusCode::INTERNAL_SERVER_ERROR
        );
    }
    assert_eq!(target.calls(), 1, "the two requests were joined");

    // One sample so far, so the circuit is still closed; this failure is the second.
    call().await.expect("the subgraph answered");
    assert_eq!(target.calls(), 2);

    let (first, second) = tokio::join!(call(), call());
    for response in [first, second] {
        let response = response.expect("the circuit answered");
        assert_eq!(response.response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(
            subgraph_error_code(&response).as_deref(),
            Some(Error::CircuitBreakerOpen.code())
        );
    }
    assert_eq!(target.calls(), 2);
}

/// Plugins are part of fulfilling a fetch, so a coprocessor that fails on the way to the subgraph
/// counts against the subgraph's circuit, like the subgraph failing itself would.
#[tokio::test]
async fn a_coprocessor_failure_counts_against_the_subgraph() {
    let coprocessor = wiremock::MockServer::start().await;
    wiremock::Mock::given(wiremock::matchers::method("POST"))
        .respond_with(wiremock::ResponseTemplate::new(500))
        .mount(&coprocessor)
        .await;

    let target = Target::answering(StatusCode::OK);
    let service = subgraph_stack(
        &[
            ("apollo.traffic_shaping", serde_json::json!({})),
            (
                "apollo.circuit_breaker",
                serde_json::json!({ "all": { "consecutive_failures": 1 } }),
            ),
            (
                "apollo.coprocessor",
                serde_json::json!({
                    "url": coprocessor.uri(),
                    "subgraph": { "all": { "request": { "body": true } } },
                }),
            ),
        ],
        target.subgraph(Duration::ZERO),
    )
    .await;

    let failed = service
        .clone()
        .oneshot(subgraph::Request::fake_builder().build())
        .await;
    assert!(
        failed.as_ref().map_or(true, |response| response
            .response
            .status()
            .is_server_error()),
        "the coprocessor should have failed the request"
    );
    assert_eq!(target.calls(), 0);

    let response = service
        .oneshot(subgraph::Request::fake_builder().build())
        .await
        .expect("the circuit answered");
    assert_eq!(
        subgraph_error_code(&response).as_deref(),
        Some(Error::CircuitBreakerOpen.code()),
        "the coprocessor's failure should have opened the circuit"
    );
}

/// A coprocessor at `stage` that lets the first request through, breaks the next `breaks` with
/// `status`, and lets every request after them through.
async fn coprocessor_breaking_between(
    stage: &str,
    breaks: u64,
    status: u16,
) -> wiremock::MockServer {
    let coprocessor = wiremock::MockServer::start().await;
    let proceed = wiremock::ResponseTemplate::new(200).set_body_json(serde_json::json!({
        "version": 1,
        "stage": stage,
        "control": "continue",
    }));
    let turn_away = wiremock::ResponseTemplate::new(200).set_body_json(serde_json::json!({
        "version": 1,
        "stage": stage,
        "control": { "break": status },
        "body": "the coprocessor turned this request away",
    }));
    for (priority, response, times) in [
        (1, proceed.clone(), Some(1)),
        (2, turn_away, Some(breaks)),
        (3, proceed, None),
    ] {
        let mock = wiremock::Mock::given(wiremock::matchers::method("POST"))
            .respond_with(response)
            .with_priority(priority);
        match times {
            Some(times) => mock.up_to_n_times(times).mount(&coprocessor).await,
            None => mock.mount(&coprocessor).await,
        }
    }
    coprocessor
}

/// How many requests the coprocessor in [`subgraph_around_breaks`] and [`source_around_breaks`]
/// breaks between the two that reach the target.
const BREAKS: usize = 5;

/// A circuit that opens once both of the last two recorded requests failed. A break recorded as a
/// failure would open it at the first break, and one recorded as a success would keep it closed
/// after the second failure.
fn opens_on_two_failures_in_a_row() -> serde_json::Value {
    serde_json::json!({
        "window_size": 2,
        "min_requests": 2,
        "failure_rate_threshold": 0.75,
        "consecutive_failures": 100,
    })
}

/// Requests to subgraph `products`, answering with `target_status`, through a coprocessor that
/// breaks the [`BREAKS`] requests between the first and the last two with `break_status`. Returns
/// the answers, after checking which requests reached the subgraph.
async fn subgraph_around_breaks(
    break_status: u16,
    target_status: StatusCode,
) -> Vec<subgraph::Response> {
    let coprocessor =
        coprocessor_breaking_between("SubgraphRequest", BREAKS as u64, break_status).await;
    let target = Target::answering(target_status);
    let service = subgraph_stack(
        &[
            ("apollo.traffic_shaping", serde_json::json!({})),
            (
                "apollo.circuit_breaker",
                serde_json::json!({ "all": opens_on_two_failures_in_a_row() }),
            ),
            (
                "apollo.coprocessor",
                serde_json::json!({
                    "url": coprocessor.uri(),
                    "subgraph": { "all": { "request": { "body": true } } },
                }),
            ),
        ],
        target.subgraph(Duration::ZERO),
    )
    .await;

    let mut responses = Vec::new();
    for _ in 0..BREAKS + 3 {
        responses.push(
            service
                .clone()
                .oneshot(subgraph::Request::fake_builder().build())
                .await
                .expect("answered"),
        );
    }
    assert_eq!(
        target.calls(),
        2,
        "only the first and the last but one reach the subgraph"
    );
    responses
}

/// Checks the answers from [`subgraph_around_breaks`]: the breaks reach the client while the
/// circuit stays closed, and the two failures either side of them open it.
fn assert_breaks_recorded_nothing(responses: &[subgraph::Response], break_status: StatusCode) {
    let (breaks, last_two) = responses[1..].split_at(BREAKS);
    for response in breaks {
        assert_eq!(
            response.response.status(),
            break_status,
            "the circuit should still be closed"
        );
    }
    assert_ne!(
        subgraph_error_code(&last_two[0]).as_deref(),
        Some(Error::CircuitBreakerOpen.code())
    );
    assert_eq!(
        subgraph_error_code(&last_two[1]).as_deref(),
        Some(Error::CircuitBreakerOpen.code()),
        "the failures either side of the breaks should have opened the circuit"
    );
}

/// A coprocessor breaking requests with a `401` turned them away itself: the subgraph never saw
/// them. A burst of them changes neither the circuit's state nor its failure rate.
#[tokio::test]
async fn a_burst_of_subgraph_requests_a_coprocessor_breaks_records_nothing() {
    let responses = subgraph_around_breaks(401, StatusCode::INTERNAL_SERVER_ERROR).await;
    assert_breaks_recorded_nothing(&responses, StatusCode::UNAUTHORIZED);
}

/// The status a coprocessor breaks with is its own, so a `503` break says nothing about the
/// subgraph either.
#[tokio::test]
async fn a_subgraph_request_a_coprocessor_breaks_with_a_5xx_records_nothing() {
    let responses = subgraph_around_breaks(503, StatusCode::INTERNAL_SERVER_ERROR).await;
    assert_breaks_recorded_nothing(&responses, StatusCode::SERVICE_UNAVAILABLE);
}

/// A `429` from the subgraph still counts on a request a coprocessor let through.
#[tokio::test]
async fn a_429_from_the_subgraph_counts_when_a_coprocessor_ran() {
    let responses = subgraph_around_breaks(401, StatusCode::TOO_MANY_REQUESTS).await;
    assert_breaks_recorded_nothing(&responses, StatusCode::UNAUTHORIZED);
}

/// The connector counterpart of [`subgraph_around_breaks`], for source `products.api`.
async fn source_around_breaks(
    break_status: u16,
    target_status: StatusCode,
) -> Vec<ConnectorResponse> {
    let coprocessor =
        coprocessor_breaking_between("ConnectorRequest", BREAKS as u64, break_status).await;
    let target = Target::answering(target_status);
    let service = source_stack(
        &[
            ("apollo.traffic_shaping", serde_json::json!({})),
            (
                "apollo.circuit_breaker",
                serde_json::json!({ "connector": { "all": opens_on_two_failures_in_a_row() } }),
            ),
            (
                "apollo.coprocessor",
                serde_json::json!({
                    "url": coprocessor.uri(),
                    "connector": { "all": { "request": { "body": true } } },
                }),
            ),
        ],
        target.source(),
    )
    .await;

    let mut responses = Vec::new();
    for _ in 0..BREAKS + 3 {
        responses.push(
            service
                .clone()
                .oneshot(connector_request())
                .await
                .expect("answered"),
        );
    }
    assert_eq!(
        target.calls(),
        2,
        "only the first and the last but one reach the source"
    );
    responses
}

/// The connector counterpart of [`assert_breaks_recorded_nothing`].
fn assert_source_breaks_recorded_nothing(responses: &[ConnectorResponse]) {
    let (breaks, last_two) = responses[1..].split_at(BREAKS);
    for response in breaks {
        assert!(response.error().is_some(), "the coprocessor broke it");
        assert_ne!(
            connector_error_code(response),
            Some(Error::CircuitBreakerOpen.code()),
            "the circuit should still be closed"
        );
    }
    assert_ne!(
        connector_error_code(&last_two[0]),
        Some(Error::CircuitBreakerOpen.code())
    );
    assert_eq!(
        connector_error_code(&last_two[1]),
        Some(Error::CircuitBreakerOpen.code()),
        "the failures either side of the breaks should have opened the circuit"
    );
}

/// A connector request carries no HTTP response when a coprocessor breaks it, and the break is
/// what tells it apart from a transport failure: a burst of `401` breaks records nothing.
#[tokio::test]
async fn a_burst_of_connector_requests_a_coprocessor_breaks_records_nothing() {
    let responses = source_around_breaks(401, StatusCode::INTERNAL_SERVER_ERROR).await;
    assert_source_breaks_recorded_nothing(&responses);
}

/// The connector counterpart of [`a_subgraph_request_a_coprocessor_breaks_with_a_5xx_records_nothing`],
/// with a `429` from the source still counting on the requests the coprocessor let through.
#[tokio::test]
async fn a_connector_request_a_coprocessor_breaks_with_a_5xx_records_nothing() {
    let responses = source_around_breaks(503, StatusCode::TOO_MANY_REQUESTS).await;
    assert_source_breaks_recorded_nothing(&responses);
}

/// A caller that goes away before the subgraph answers says nothing about the subgraph: between
/// two failures, it must not reset the count of consecutive failures, as a success would.
#[tokio::test(start_paused = true)]
async fn a_caller_that_goes_away_records_nothing() {
    let plugin = harness(
        r#"
        circuit_breaker:
          all:
            consecutive_failures: 2
        "#,
    )
    .await;

    let calls = Arc::new(AtomicUsize::new(0));
    let service = protected_subgraph(&plugin, "products", {
        let calls = calls.clone();
        move |req: subgraph::Request| {
            // The second request hangs, and its caller gives up on it.
            let hangs = calls.fetch_add(1, Ordering::SeqCst) == 1;
            async move {
                if hangs {
                    std::future::pending::<()>().await;
                }
                Ok(subgraph_response(StatusCode::INTERNAL_SERVER_ERROR, &req))
            }
        }
    });

    service
        .call(subgraph::Request::fake_builder().build())
        .await
        .expect("the subgraph answered");
    let abandoned = tokio::time::timeout(
        Duration::from_secs(1),
        service.call(subgraph::Request::fake_builder().build()),
    )
    .await;
    assert!(abandoned.is_err(), "the caller gave up");
    service
        .call(subgraph::Request::fake_builder().build())
        .await
        .expect("the subgraph answered");

    let response = service
        .call(subgraph::Request::fake_builder().build())
        .await
        .expect("the circuit answered");
    assert_eq!(
        response.response.status(),
        StatusCode::SERVICE_UNAVAILABLE,
        "the failures either side of the abandoned request should have been consecutive"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 3);
}

/// A recovery probe whose caller goes away records nothing and hands the probe on: the circuit
/// stays half-open, and the next request becomes the probe without another `open_duration`.
#[tokio::test(start_paused = true)]
async fn a_probe_whose_caller_goes_away_hands_the_probe_to_the_next_request() {
    let plugin = harness(
        r#"
        circuit_breaker:
          all:
            consecutive_failures: 1
            open_duration: 10s
        "#,
    )
    .await;

    let calls = Arc::new(AtomicUsize::new(0));
    let service = protected_subgraph(&plugin, "products", {
        let calls = calls.clone();
        move |req: subgraph::Request| {
            let call = calls.fetch_add(1, Ordering::SeqCst);
            async move {
                match call {
                    0 => Ok(subgraph_response(StatusCode::INTERNAL_SERVER_ERROR, &req)),
                    // The first probe hangs, and its caller gives up on it.
                    1 => std::future::pending().await,
                    _ => Ok(subgraph_response(StatusCode::OK, &req)),
                }
            }
        }
    });

    service
        .call(subgraph::Request::fake_builder().build())
        .await
        .expect("the subgraph answered");
    tokio::time::advance(Duration::from_secs(11)).await;

    let abandoned = tokio::time::timeout(
        Duration::from_secs(1),
        service.call(subgraph::Request::fake_builder().build()),
    )
    .await;
    assert!(abandoned.is_err(), "the probe's caller gave up");

    let response = service
        .call(subgraph::Request::fake_builder().build())
        .await
        .expect("the next request was let through as the probe");
    assert_eq!(response.response.status(), StatusCode::OK);
    assert_eq!(calls.load(Ordering::SeqCst), 3);

    let response = service
        .call(subgraph::Request::fake_builder().build())
        .await
        .expect("the subgraph answered");
    assert_eq!(
        response.response.status(),
        StatusCode::OK,
        "the successful probe should have closed the circuit"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 4);
}

/// The config for the probe tests: two failures in a row open the circuit, while a half-open
/// circuit reopens on its probe's first failure. A failure straight after a probe that recorded
/// nothing therefore reopens the circuit only if that probe left it half-open: had the probe
/// counted as a success, the circuit would be closed and need a second failure.
const PROBE_CONFIG: &str = r#"
    circuit_breaker:
      all:
        consecutive_failures: 2
        open_duration: 10s
      connector:
        all:
          consecutive_failures: 2
          open_duration: 10s
    "#;

/// A recovery probe that `mark` marks as saying nothing about the subgraph records nothing and
/// hands the probe on: the circuit stays half-open, and the next request becomes the probe
/// without another `open_duration`.
async fn assert_a_marked_probe_hands_the_probe_on(mark: fn(&mut subgraph::Response)) {
    let plugin = harness(PROBE_CONFIG).await;

    let calls = Arc::new(AtomicUsize::new(0));
    let service = protected_subgraph(&plugin, "products", {
        let calls = calls.clone();
        move |req: subgraph::Request| {
            let call = calls.fetch_add(1, Ordering::SeqCst);
            async move {
                let mut response = subgraph_response(StatusCode::INTERNAL_SERVER_ERROR, &req);
                if call == 2 {
                    mark(&mut response);
                }
                Ok(response)
            }
        }
    });
    let request = || subgraph::Request::fake_builder().build();

    for _ in 0..2 {
        service
            .call(request())
            .await
            .expect("the subgraph answered");
    }
    tokio::time::advance(Duration::from_secs(11)).await;
    service
        .call(request())
        .await
        .expect("the probe was let through");

    let response = service
        .call(request())
        .await
        .expect("the next request was let through as the probe");
    assert_eq!(
        response.response.status(),
        StatusCode::INTERNAL_SERVER_ERROR
    );
    assert_eq!(calls.load(Ordering::SeqCst), 4);

    let response = service.call(request()).await.expect("the circuit answered");
    assert_eq!(
        subgraph_error_code(&response).as_deref(),
        Some(Error::CircuitBreakerOpen.code()),
        "the failed probe should have reopened the circuit"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 4);
}

/// A coprocessor or plugin break, or a response-cache hit, on the probe.
#[tokio::test(start_paused = true)]
async fn a_probe_the_router_answers_hands_the_probe_to_the_next_request() {
    assert_a_marked_probe_hands_the_probe_on(|response| {
        response.response.extensions_mut().insert(AnsweredByRouter);
    })
    .await;
}

/// A file upload failing part way through on the probe.
#[tokio::test(start_paused = true)]
async fn a_probe_whose_upload_fails_hands_the_probe_to_the_next_request() {
    assert_a_marked_probe_hands_the_probe_on(|response| {
        response
            .response
            .extensions_mut()
            .insert(UploadStreamFailed);
    })
    .await;
}

/// The connector counterpart of [`assert_a_marked_probe_hands_the_probe_on`], for a probe the
/// router answered without calling the source.
#[tokio::test(start_paused = true)]
async fn a_connector_probe_the_router_answers_hands_the_probe_to_the_next_request() {
    let plugin = harness(PROBE_CONFIG).await;

    let call = Arc::new(AtomicUsize::new(0));
    let (mut service, calls) = connector_service(&plugin, "products.api", {
        let call = call.clone();
        move |req| {
            let mut response = connector_response(StatusCode::INTERNAL_SERVER_ERROR, req);
            response.answered_by_router = call.fetch_add(1, Ordering::SeqCst) == 2;
            response
        }
    });
    let mut send = async || {
        service
            .ready()
            .await
            .expect("ready")
            .call(connector_request())
            .await
            .expect("answered")
    };

    for _ in 0..2 {
        send().await;
    }
    tokio::time::advance(Duration::from_secs(11)).await;
    send().await;

    let response = send().await;
    assert_ne!(
        connector_error_code(&response),
        Some(Error::CircuitBreakerOpen.code()),
        "the next request should have been let through as the probe"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 4);

    let response = send().await;
    assert_eq!(
        connector_error_code(&response),
        Some(Error::CircuitBreakerOpen.code()),
        "the failed probe should have reopened the circuit"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 4);
}

/// A file upload whose stream from the client fails part way through fails the fetch with a
/// `500`, but the client is at fault, so however many of them there are, the subgraph's circuit
/// stays closed.
#[tokio::test]
async fn uploads_that_fail_part_way_cannot_open_a_healthy_circuit() {
    let plugin = harness(
        r#"
        circuit_breaker:
          all:
            consecutive_failures: 1
        "#,
    )
    .await;

    let calls = Arc::new(AtomicUsize::new(0));
    let service = protected_subgraph(&plugin, "products", {
        let calls = calls.clone();
        move |req: subgraph::Request| {
            let upload_fails = calls.fetch_add(1, Ordering::SeqCst) < 3;
            async move {
                if upload_fails {
                    let mut response = subgraph_response(StatusCode::INTERNAL_SERVER_ERROR, &req);
                    response
                        .response
                        .extensions_mut()
                        .insert(UploadStreamFailed);
                    Ok(response)
                } else {
                    Ok(subgraph_response(StatusCode::OK, &req))
                }
            }
        }
    });

    for _ in 0..3 {
        service
            .call(subgraph::Request::fake_builder().build())
            .await
            .expect("the subgraph service answered");
    }
    let response = service
        .call(subgraph::Request::fake_builder().build())
        .await
        .expect("the subgraph answered");
    assert_eq!(
        response.response.status(),
        StatusCode::OK,
        "the circuit should still be closed"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 4);
}

/// Waits for a clone of `service` to become ready and lets it go again, failing if it cannot: a
/// clone of a service under a concurrency limit of one can only become ready while nothing else
/// holds the permit. Only meant for tests on paused time, where a clone that never becomes ready
/// runs into the timeout at once rather than hanging the test.
async fn assert_permit_is_free<Req, Res>(
    service: &tower::util::BoxCloneService<Req, Res, BoxError>,
) {
    let mut other = service.clone();
    tokio::time::timeout(Duration::from_secs(1), other.ready())
        .await
        .expect("nothing should still hold the permit")
        .expect("ready");
}

/// A concurrency limit a plugin puts under the circuit hands out its permit from `poll_ready`, and
/// takes it back only once the request it was readied for is done with it. A request the open
/// circuit turns away never reaches the limited service, so the permit has to be given back
/// without it.
#[tokio::test(start_paused = true)]
async fn a_request_that_never_reaches_the_subgraph_gives_back_its_permit() {
    let plugin = harness(
        r#"
        circuit_breaker:
          all:
            consecutive_failures: 1
        "#,
    )
    .await;

    let limited = tower::ServiceBuilder::new()
        .concurrency_limit(1)
        .service_fn(|request: subgraph::Request| async move {
            Ok::<_, BoxError>(subgraph_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                &request,
            ))
        })
        .boxed_clone();
    let mut service = behind_subgraph_circuit(&plugin, "products", limited);

    // Reaches the subgraph and fails, which opens the circuit.
    service
        .ready()
        .await
        .expect("ready")
        .call(subgraph::Request::fake_builder().build())
        .await
        .expect("the subgraph answered");

    // Turned away by the open circuit.
    let response = service
        .ready()
        .await
        .expect("ready")
        .call(subgraph::Request::fake_builder().build())
        .await
        .expect("the circuit answered");
    assert_eq!(response.response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_permit_is_free(&service).await;
}

/// An open circuit fails fast: it turns a request away without waiting for the subgraph's service
/// to be ready, here a concurrency limit under the circuit whose only permit another caller holds.
#[tokio::test(start_paused = true)]
async fn an_open_circuit_rejects_without_waiting_for_the_subgraph_service() {
    let plugin = harness(
        r#"
        circuit_breaker:
          all:
            consecutive_failures: 1
        "#,
    )
    .await;

    let limited = tower::ServiceBuilder::new()
        .concurrency_limit(1)
        .service_fn(|request: subgraph::Request| async move {
            Ok::<_, BoxError>(subgraph_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                &request,
            ))
        })
        .boxed_clone();
    let mut service = behind_subgraph_circuit(&plugin, "products", limited.clone());

    // Reaches the subgraph and fails, which opens the circuit.
    service
        .ready()
        .await
        .expect("ready")
        .call(subgraph::Request::fake_builder().build())
        .await
        .expect("the subgraph answered");

    let mut holder = limited;
    holder.ready().await.expect("the permit is free");

    let response = tokio::time::timeout(Duration::from_secs(1), async {
        service
            .ready()
            .await
            .expect("ready")
            .call(subgraph::Request::fake_builder().build())
            .await
    })
    .await
    .expect("the open circuit should not wait for the permit")
    .expect("the circuit answered");
    assert_eq!(
        subgraph_error_code(&response).as_deref(),
        Some(Error::CircuitBreakerOpen.code())
    );
}

/// A connection that drops part of the way through the body leaves the status the headers
/// carried, so a `200` can still be a transport failure.
#[tokio::test]
async fn a_subgraph_response_cut_off_part_way_through_its_body_is_a_failure() {
    let plugin = harness(
        r#"
        circuit_breaker:
          all:
            consecutive_failures: 1
        "#,
    )
    .await;

    let service = protected_subgraph(&plugin, "products", |req: subgraph::Request| async move {
        let mut response = subgraph_response(StatusCode::OK, &req);
        response
            .response
            .extensions_mut()
            .insert(IncompleteResponseBody);
        Ok(response)
    });

    let response = service
        .call(subgraph::Request::fake_builder().build())
        .await
        .expect("the subgraph answered");
    assert_eq!(response.response.status(), StatusCode::OK);

    let response = service
        .call(subgraph::Request::fake_builder().build())
        .await
        .expect("the circuit answered");
    assert_eq!(
        response.response.status(),
        StatusCode::SERVICE_UNAVAILABLE,
        "the cut-off body should have opened the circuit"
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
    let mut service = behind_source_circuit(&plugin, "products.api", {
        let calls = calls.clone();
        tower::service_fn(move |req: ConnectorRequest| {
            let calls = calls.clone();
            async move {
                calls.fetch_add(1, Ordering::SeqCst);
                Ok(connector_response(StatusCode::BAD_GATEWAY, &req))
            }
        })
        .boxed_clone()
    });

    for _ in 0..2 {
        let response = service
            .ready()
            .await
            .expect("ready")
            .call(connector_request())
            .await
            .expect("the source answered");
        assert!(!matches!(
            response.transport_outcome,
            TransportOutcome::Error(_)
        ));
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
        response.transport_outcome,
        TransportOutcome::Error(Error::CircuitBreakerOpen)
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

/// A connector source with no entry of its own is governed by `connector.all`, not by another
/// source's block.
#[tokio::test]
async fn a_connector_source_with_no_entry_of_its_own_follows_all() {
    let plugin = harness(
        r#"
        circuit_breaker:
          connector:
            all:
              consecutive_failures: 4
            sources:
              products.api:
                consecutive_failures: 1
        "#,
    )
    .await;

    let mut service = behind_source_circuit(
        &plugin,
        "reviews.api",
        tower::service_fn(|req: ConnectorRequest| async move {
            Ok(connector_response(StatusCode::BAD_GATEWAY, &req))
        })
        .boxed_clone(),
    );

    // `products.api` would have opened on its first failure; `reviews.api` takes `all`'s four.
    for failure in 1..=4 {
        let response = service
            .ready()
            .await
            .expect("ready")
            .call(connector_request())
            .await
            .expect("the source answered");
        assert!(
            !matches!(response.transport_outcome, TransportOutcome::Error(_)),
            "reviews.api should still be reaching the source after {failure} failures"
        );
    }

    let response = service
        .ready()
        .await
        .expect("ready")
        .call(connector_request())
        .await
        .expect("the circuit answered");
    assert_eq!(
        connector_error_code(&response),
        Some(Error::CircuitBreakerOpen.code()),
        "the fifth request should meet the circuit `connector.all`'s four failures opened"
    );
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
        assert!(!matches!(
            response.transport_outcome,
            TransportOutcome::Error(_)
        ));
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
        response.transport_outcome,
        TransportOutcome::Error(Error::CircuitBreakerOpen)
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
        assert!(!matches!(
            response.transport_outcome,
            TransportOutcome::Error(_)
        ));
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
        matches!(
            response.transport_outcome,
            TransportOutcome::Error(Error::CircuitBreakerOpen)
        ),
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
            !matches!(response.transport_outcome, TransportOutcome::Error(_)),
            "a 4xx should not open the circuit"
        );
    }
}

/// The connector counterpart of [`a_429_from_the_subgraph_is_a_failure`].
#[tokio::test]
async fn a_429_from_the_source_is_a_failure() {
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
            connector_response(StatusCode::TOO_MANY_REQUESTS, req)
        });

    for _ in 0..2 {
        service
            .ready()
            .await
            .expect("ready")
            .call(connector_request())
            .await
            .expect("answered");
    }
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "the 429 opened the circuit"
    );
}

/// A connector's `max_requests` is admission: a request over it is never sent, and never reaches
/// the circuit. Between two failures, it must not reset the count of consecutive failures, as a
/// success would, or open the circuit on its own, as a failure would.
#[tokio::test]
async fn a_connector_request_over_max_requests_is_not_recorded() {
    let target = Target::answering(StatusCode::BAD_GATEWAY);
    let service = placed_source(
        serde_json::json!({ "connector": { "all": { "consecutive_failures": 2 } } }),
        serde_json::json!({}),
        target.source(),
    )
    .await;

    // One operation, allowed a single request to the source.
    let operation = Context::new();
    operation
        .extensions()
        .with_lock(|lock| lock.insert(Arc::new(RequestLimits::new(Some(1)))));
    let request_in = |context: &Context| {
        let mut request = connector_request();
        request.context = context.clone();
        request
    };

    let response = service
        .clone()
        .oneshot(request_in(&operation))
        .await
        .expect("the source answered");
    assert!(!matches!(
        response.transport_outcome,
        TransportOutcome::Error(_)
    ));

    let response = service
        .clone()
        .oneshot(request_in(&operation))
        .await
        .expect("the limit answered");
    assert!(matches!(
        response.transport_outcome,
        TransportOutcome::Error(Error::RequestLimitExceeded)
    ));

    // Another operation, with its own allowance.
    service
        .clone()
        .oneshot(connector_request())
        .await
        .expect("the source answered");
    assert_eq!(target.calls(), 2);

    let response = service
        .oneshot(connector_request())
        .await
        .expect("the circuit answered");
    assert_eq!(
        connector_error_code(&response),
        Some(Error::CircuitBreakerOpen.code()),
        "the two failures should have been consecutive"
    );
    assert_eq!(target.calls(), 2);
}

/// A request over `max_requests` while the circuit waits for its recovery probe is not the probe:
/// it neither closes the circuit nor reopens it, so the next admitted request still probes the
/// source.
#[tokio::test(start_paused = true)]
async fn a_connector_request_over_max_requests_does_not_take_the_recovery_probe() {
    let target = Target::answering(StatusCode::BAD_GATEWAY);
    let service = placed_source(
        serde_json::json!({
            "connector": { "all": { "consecutive_failures": 1, "open_duration": "10s" } }
        }),
        serde_json::json!({}),
        target.source(),
    )
    .await;

    // Fails, and opens the circuit for ten seconds.
    service
        .clone()
        .oneshot(connector_request())
        .await
        .expect("the source answered");

    // The circuit is ready for a probe, but this operation may not send any requests.
    tokio::time::advance(Duration::from_secs(11)).await;
    let over_the_limit = connector_request();
    over_the_limit
        .context
        .extensions()
        .with_lock(|lock| lock.insert(Arc::new(RequestLimits::new(Some(0)))));
    let response = service
        .clone()
        .oneshot(over_the_limit)
        .await
        .expect("the limit answered");
    assert!(matches!(
        response.transport_outcome,
        TransportOutcome::Error(Error::RequestLimitExceeded)
    ));

    // Had the rejected request closed the circuit, this failure would only start a new count;
    // as the probe, it reopens the circuit.
    service
        .clone()
        .oneshot(connector_request())
        .await
        .expect("the probe was answered");
    assert_eq!(target.calls(), 2, "the probe reached the source");
    let response = service
        .oneshot(connector_request())
        .await
        .expect("the circuit answered");
    assert_eq!(
        connector_error_code(&response),
        Some(Error::CircuitBreakerOpen.code()),
        "the failed probe should have reopened the circuit"
    );
}

/// The connector counterpart of [`a_request_the_rate_limit_sheds_is_not_recorded`].
#[tokio::test(start_paused = true)]
async fn a_connector_request_the_rate_limit_sheds_is_not_recorded() {
    let target = Target::answering(StatusCode::BAD_GATEWAY);
    let service = placed_source(
        serde_json::json!({
            "connector": { "all": { "consecutive_failures": 2, "open_duration": "1h" } }
        }),
        serde_json::json!({
            "connector": { "all": { "global_rate_limit": { "capacity": 1, "interval": "10s" } } }
        }),
        target.source(),
    )
    .await;
    let call = || service.clone().oneshot(connector_request());

    call().await.expect("the source answered");
    let response = call().await.expect("the rate limit answered");
    assert!(matches!(
        response.transport_outcome,
        TransportOutcome::Error(Error::RateLimited)
    ));

    tokio::time::advance(Duration::from_secs(10)).await;
    call().await.expect("the source answered");
    assert_eq!(target.calls(), 2);

    tokio::time::advance(Duration::from_secs(10)).await;
    let response = call().await.expect("the circuit answered");
    assert_eq!(
        connector_error_code(&response),
        Some(Error::CircuitBreakerOpen.code()),
        "the two failures should have been consecutive"
    );
}

/// Each connector source has a circuit of its own, including two sources of the same subgraph.
#[tokio::test]
async fn two_sources_of_one_subgraph_trip_independently() {
    let plugin = harness(
        r#"
        circuit_breaker:
          connector:
            all:
              consecutive_failures: 1
        "#,
    )
    .await;

    let (mut failing, _) = connector_service(&plugin, "products.api", |req: &ConnectorRequest| {
        connector_response(StatusCode::BAD_GATEWAY, req)
    });
    let (mut other, other_calls) =
        connector_service(&plugin, "products.other", |req: &ConnectorRequest| {
            connector_response(StatusCode::OK, req)
        });

    for _ in 0..2 {
        failing
            .ready()
            .await
            .expect("ready")
            .call(connector_request())
            .await
            .expect("answered");
    }
    let response = other
        .ready()
        .await
        .expect("ready")
        .call(connector_request())
        .await
        .expect("the source answered");
    assert_eq!(connector_error_code(&response), None);
    assert_eq!(other_calls.load(Ordering::SeqCst), 1);
}

/// A source that failed to answer, or did not answer within traffic shaping's timeout, is the
/// failure a circuit exists for. Traffic shaping renders its timeout as `GatewayTimeout` beneath
/// the circuit, and that is the only way a source that has stopped answering shows up at all.
#[tokio::test]
async fn a_connector_transport_failure_or_timeout_is_a_source_failure() {
    for error in [
        Error::TransportFailure("connection refused".into()),
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
            Some(Error::CircuitBreakerOpen.code()),
            "{error:?} should have opened the circuit"
        );
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "the rejected request must not reach the source after {error:?}"
        );
    }
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

    let mut service = behind_source_circuit(
        &plugin,
        "products.api",
        tower::service_fn(|req: ConnectorRequest| async move {
            let mut response = connector_response(StatusCode::OK, &req);
            response.transport_outcome = TransportOutcome::MappingOnly;
            Ok(response)
        })
        .boxed_clone(),
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
            response.transport_outcome,
            TransportOutcome::MappingOnly
        ));
    }
}

/// The connector counterpart of [`a_subgraph_that_stops_answering_opens_its_circuit_at_its_timeout`].
#[tokio::test(start_paused = true)]
async fn a_source_that_stops_answering_opens_its_circuit_at_its_timeout() {
    let calls = Arc::new(AtomicUsize::new(0));
    let service = placed_source(
        serde_json::json!({ "connector": { "all": { "consecutive_failures": 1 } } }),
        serde_json::json!({ "connector": { "all": { "timeout": "1s" } } }),
        {
            let calls = calls.clone();
            move |_req: SourceHttpRequest| {
                calls.fetch_add(1, Ordering::SeqCst);
                Box::pin(std::future::pending())
            }
        },
    )
    .await;

    let response = service
        .clone()
        .oneshot(connector_request())
        .await
        .expect("the timeout answered");
    assert!(matches!(
        response.transport_outcome,
        TransportOutcome::Error(Error::GatewayTimeout)
    ));

    // That timeout was the circuit's one failure.
    let response = service
        .oneshot(connector_request())
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

/// The connector counterpart of
/// [`a_subgraph_response_cut_off_part_way_through_its_body_is_a_failure`].
#[tokio::test]
async fn a_connector_response_cut_off_part_way_through_its_body_is_a_failure() {
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
            let mut response = connector_response(StatusCode::OK, req);
            if let TransportOutcome::Response(http_response) = &mut response.transport_outcome {
                http_response
                    .inner
                    .extensions
                    .insert(IncompleteResponseBody);
            }
            response
        });

    for _ in 0..2 {
        service
            .ready()
            .await
            .expect("ready")
            .call(connector_request())
            .await
            .expect("the service answered");
    }
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "the cut-off body should have opened the circuit"
    );
}

/// A mapping-only connector can name a `@source` and so share its circuit, but it never makes a
/// request, so an open circuit has no reason to turn it away.
#[tokio::test]
async fn a_mapping_only_request_goes_around_an_open_circuit() {
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
            if matches!(req.transport_request, TransportRequest::MappingOnly) {
                let mut response = connector_response(StatusCode::OK, req);
                response.transport_outcome = TransportOutcome::MappingOnly;
                response
            } else {
                connector_response(StatusCode::BAD_GATEWAY, req)
            }
        });

    // An HTTP request fails, which opens the source's circuit.
    for _ in 0..2 {
        service
            .ready()
            .await
            .expect("ready")
            .call(connector_request())
            .await
            .expect("the service answered");
    }
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "the circuit should be open"
    );

    let mut request = connector_request();
    request.transport_request = TransportRequest::MappingOnly;
    let response = service
        .ready()
        .await
        .expect("ready")
        .call(request)
        .await
        .expect("the connector answered");
    assert!(matches!(
        response.transport_outcome,
        TransportOutcome::MappingOnly
    ));
    assert_eq!(
        calls.load(Ordering::SeqCst),
        2,
        "the mapping-only request should have gone around the open circuit"
    );
}

/// The connector counterpart of
/// [`a_request_that_never_reaches_the_subgraph_gives_back_its_permit`], for the one request that
/// goes around the circuit: it has to take the permit its service was readied with, rather than
/// wait for another one that the readied service is still holding.
#[tokio::test(start_paused = true)]
async fn a_mapping_only_request_uses_the_permit_it_was_readied_with() {
    let plugin = harness(
        r#"
        circuit_breaker:
          connector:
            all:
              consecutive_failures: 1
        "#,
    )
    .await;

    let limited = tower::ServiceBuilder::new()
        .concurrency_limit(1)
        .service_fn(|request: ConnectorRequest| async move {
            let mut response = connector_response(StatusCode::OK, &request);
            response.transport_outcome = TransportOutcome::MappingOnly;
            Ok::<_, BoxError>(response)
        })
        .boxed_clone();
    let mut service = behind_source_circuit(&plugin, "products.api", limited);

    let mut request = connector_request();
    request.transport_request = TransportRequest::MappingOnly;
    let response = tokio::time::timeout(Duration::from_secs(1), async {
        service.ready().await.expect("ready").call(request).await
    })
    .await
    .expect("the request should not wait on a permit its own service is holding")
    .expect("the connector answered");
    assert!(matches!(
        response.transport_outcome,
        TransportOutcome::MappingOnly
    ));
    assert_permit_is_free(&service).await;
}

/// [`ConnectorRequest::into_error_response`] is how a native plugin breaks a connector request.
/// The source is never called, so however many of them there are, its circuit stays closed.
#[tokio::test]
async fn a_request_a_plugin_breaks_records_nothing() {
    let plugin = harness(
        r#"
        circuit_breaker:
          connector:
            all:
              consecutive_failures: 1
        "#,
    )
    .await;

    let calls = Arc::new(AtomicUsize::new(0));
    let mut service = behind_source_circuit(&plugin, "products.api", {
        let calls = calls.clone();
        tower::service_fn(move |req: ConnectorRequest| {
            let calls = calls.clone();
            async move {
                calls.fetch_add(1, Ordering::SeqCst);
                Ok(req.into_error_response(
                    "the upstream is unhealthy",
                    "UPSTREAM_UNHEALTHY",
                    std::iter::empty::<(&str, &str)>(),
                ))
            }
        })
        .boxed_clone()
    });

    for _ in 0..3 {
        let response = service
            .ready()
            .await
            .expect("ready")
            .call(connector_request())
            .await
            .expect("the plugin answered");
        assert_eq!(connector_error_code(&response), Some("UPSTREAM_UNHEALTHY"));
    }
    assert_eq!(calls.load(Ordering::SeqCst), 3);
}

// --- through the whole router --------------------------------------------------------------

/// The codes of the errors in `response`.
fn error_codes(response: &crate::graphql::Response) -> Vec<String> {
    response
        .errors
        .iter()
        .filter_map(|error| error.extensions.get("code")?.as_str().map(str::to_string))
        .collect()
}

/// Sends `{ topProducts { name } }`, which only the `products` subgraph answers, and returns the
/// first response.
async fn query_top_products(
    service: &crate::services::supergraph::BoxCloneService,
) -> Result<crate::graphql::Response, BoxError> {
    Ok(service
        .clone()
        .oneshot(
            crate::services::supergraph::Request::fake_builder()
                .query("{ topProducts { name } }")
                .build()?,
        )
        .await?
        .next_response()
        .await
        .expect("a response"))
}

/// Exercises the plugin the way the router builds it: its layers placed by `pipeline::stages`
/// outside every plugin hook, and reached through a real supergraph service. The unit tests above
/// wrap services in the layers directly, so nothing there would notice a layer not being applied,
/// or ending up beneath a plugin that replaces the subgraph service, as the test harness's
/// `subgraph_hook` does here.
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

    // The first request reaches the failing subgraph and opens its circuit.
    let response = query_top_products(&service).await?;
    assert!(
        !error_codes(&response).contains(&Error::CircuitBreakerOpen.code().to_string()),
        "the first request should have reached the subgraph: {response:?}"
    );

    // The next one is rejected by the open circuit.
    let response = query_top_products(&service).await?;
    assert!(
        error_codes(&response).contains(&Error::CircuitBreakerOpen.code().to_string()),
        "the second request should have been rejected by the open circuit: {response:?}"
    );

    Ok(())
}

/// A subgraph that stops answering, through the pipeline the router builds: traffic shaping's
/// timeout beneath the circuit breaker, and the timeout's error on its way back out through the
/// circuit to the client.
#[tokio::test]
async fn a_subgraph_that_stops_answering_opens_its_circuit_through_a_real_supergraph_service()
-> Result<(), BoxError> {
    let service = crate::TestHarness::builder()
        .configuration_json(serde_json::json!({
            "traffic_shaping": { "all": { "timeout": "100ms" } },
            "circuit_breaker": {
                "subgraphs": {
                    "products": {
                        "consecutive_failures": 1,
                    },
                },
            },
            "include_subgraph_errors": { "all": true },
        }))?
        .subgraph_hook(|name, service| {
            if name != "products" {
                return service;
            }
            hanging_subgraph().0
        })
        .build_supergraph()
        .await?;

    let response = query_top_products(&service).await?;
    assert_eq!(
        error_codes(&response),
        ["GATEWAY_TIMEOUT"],
        "the first request should have timed out waiting on the subgraph: {response:?}"
    );

    let response = query_top_products(&service).await?;
    assert_eq!(
        error_codes(&response),
        [Error::CircuitBreakerOpen.code()],
        "the timeout should have opened the circuit: {response:?}"
    );

    Ok(())
}

/// A request traffic shaping sheds, through the pipeline the router builds: the rate limit sits
/// above the circuit, so the `503` it answers with never reaches the circuit, and the rate limit
/// cannot open the circuit on a subgraph that is answering fine.
#[tokio::test]
async fn a_request_traffic_shaping_sheds_does_not_open_a_circuit_through_a_real_supergraph_service()
-> Result<(), BoxError> {
    let service = crate::TestHarness::builder()
        .configuration_json(serde_json::json!({
            // Every request after the first is shed.
            "traffic_shaping": {
                "all": { "global_rate_limit": { "capacity": 1, "interval": "1h" } },
            },
            "circuit_breaker": {
                "subgraphs": {
                    "products": {
                        "consecutive_failures": 1,
                    },
                },
            },
            "include_subgraph_errors": { "all": true },
        }))?
        .subgraph_hook(|name, service| {
            if name != "products" {
                return service;
            }
            tower::service_fn(|request: subgraph::Request| async move {
                Ok(subgraph_response(StatusCode::OK, &request))
            })
            .boxed_clone()
        })
        .build_supergraph()
        .await?;

    let response = query_top_products(&service).await?;
    assert!(
        !error_codes(&response).contains(&"REQUEST_RATE_LIMITED".to_string()),
        "the first request should have reached the subgraph: {response:?}"
    );

    // Had the first shed request counted, the circuit would have turned the second away.
    for _ in 0..2 {
        let response = query_top_products(&service).await?;
        assert_eq!(
            error_codes(&response),
            ["REQUEST_RATE_LIMITED"],
            "a shed request should not have opened the circuit: {response:?}"
        );
    }

    Ok(())
}

/// Sends `body` as a JSON request to `router` and returns the parsed response body.
async fn post_to_router(
    router: crate::services::router::BoxCloneService,
    body: serde_json::Value,
) -> Result<serde_json::Value, BoxError> {
    let request = crate::services::router::Request {
        context: Context::new(),
        router_request: http::Request::builder()
            .method("POST")
            .header(http::header::CONTENT_TYPE, "application/json")
            .header(http::header::ACCEPT, "application/json")
            .body(crate::services::router::body::from_bytes(
                serde_json::to_vec(&body)?,
            ))?,
    };
    let response = router
        .oneshot(request)
        .await?
        .next_response()
        .await
        .expect("a response")?;
    Ok(serde_json::from_slice(&response)?)
}

/// A fetch that joins its subgraph's batch waits there until every other fetch of that batch has
/// reached it, which a fetch the circuit turned away never would. A fetch that joins a batch
/// therefore goes around the circuit, so an open circuit can't leave a client batch waiting
/// forever.
#[tokio::test(flavor = "multi_thread")]
async fn an_open_circuit_does_not_hold_up_a_client_batch() -> Result<(), BoxError> {
    use wiremock::Mock;
    use wiremock::MockServer;
    use wiremock::ResponseTemplate;
    use wiremock::matchers;

    /// Answers each operation of a batch with `field`.
    fn answer_batch(field: &'static str) -> impl Fn(&wiremock::Request) -> ResponseTemplate {
        move |request| {
            let batch: Vec<serde_json::Value> = request.body_json().expect("a batch");
            let answers: Vec<_> = batch
                .iter()
                .map(|_| serde_json::json!({ "data": { field: { "index": 0 } } }))
                .collect();
            ResponseTemplate::new(200).set_body_json(answers)
        }
    }

    let subgraphs = MockServer::start().await;
    // `a` fails the first request it receives, and answers batches after that.
    Mock::given(matchers::method("POST"))
        .and(matchers::path("/a"))
        .respond_with(ResponseTemplate::new(500))
        .up_to_n_times(1)
        .with_priority(1)
        .mount(&subgraphs)
        .await;
    Mock::given(matchers::method("POST"))
        .and(matchers::path("/a"))
        .respond_with(answer_batch("entryA"))
        .mount(&subgraphs)
        .await;
    Mock::given(matchers::method("POST"))
        .and(matchers::path("/b"))
        .respond_with(answer_batch("entryB"))
        .mount(&subgraphs)
        .await;

    let router = crate::TestHarness::builder()
        .configuration_json(serde_json::json!({
            "include_subgraph_errors": { "all": true },
            "batching": {
                "enabled": true,
                "mode": "batch_http_link",
                "subgraph": { "all": { "enabled": true } },
            },
            "circuit_breaker": {
                "subgraphs": { "a": { "consecutive_failures": 1, "open_duration": "1h" } },
            },
            "override_subgraph_url": {
                "a": format!("{}/a", subgraphs.uri()),
                "b": format!("{}/b", subgraphs.uri()),
            },
        }))?
        .schema(include_str!(
            "../../../tests/fixtures/batching/schema.graphql"
        ))
        .with_subgraph_network_requests()
        .build_router()
        .await?;

    // A request on its own isn't batched: it reaches the failing subgraph and opens its circuit,
    // and the next one is turned away.
    let single = serde_json::json!({ "query": "{ entryA(count: 1) { index } }" });
    post_to_router(router.clone(), single.clone()).await?;
    let response = post_to_router(router.clone(), single).await?;
    assert_eq!(
        response["errors"][0]["extensions"]["code"],
        Error::CircuitBreakerOpen.code(),
        "the circuit should be open: {response}"
    );

    // Each operation fetches from `a` and `b` in parallel, so `b`'s batch waits on `a`'s.
    let batch = serde_json::json!([
        { "query": "query op0 { entryA(count: 2) { index } entryB(count: 2) { index } }" },
        { "query": "query op1 { entryA(count: 2) { index } entryB(count: 2) { index } }" },
    ]);
    let response = tokio::time::timeout(Duration::from_secs(10), post_to_router(router, batch))
        .await
        .expect("the batch should be answered, not left waiting on the open circuit")?;
    let operations = response.as_array().expect("one response per operation");
    assert_eq!(operations.len(), 2, "{response}");
    // What the fetches themselves return isn't asserted: batching reads each fetch's query hash
    // from the operation's context, which the two parallel fetches of one operation share, so it
    // can fail them whether or not a circuit breaker is configured.
    for operation in operations {
        let codes: Vec<_> = operation["errors"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|error| error["extensions"]["code"].as_str())
            .collect();
        assert!(
            !codes.contains(&Error::CircuitBreakerOpen.code()),
            "a batched fetch should go around the open circuit: {response}"
        );
    }

    Ok(())
}

/// Only a fetch that joins a subgraph batch goes around the circuit. A batched client request's
/// fetch to a subgraph with batching turned off is sent on its own, so it meets the circuit.
#[tokio::test(flavor = "multi_thread")]
async fn an_open_circuit_turns_away_a_client_batch_fetch_to_a_subgraph_without_batching()
-> Result<(), BoxError> {
    use wiremock::Mock;
    use wiremock::MockServer;
    use wiremock::ResponseTemplate;
    use wiremock::matchers;

    let subgraphs = MockServer::start().await;
    // Only the request that opens the circuit should reach `a`.
    Mock::given(matchers::method("POST"))
        .and(matchers::path("/a"))
        .respond_with(ResponseTemplate::new(500))
        .expect(1)
        .mount(&subgraphs)
        .await;

    let router = crate::TestHarness::builder()
        .configuration_json(serde_json::json!({
            "include_subgraph_errors": { "all": true },
            "batching": {
                "enabled": true,
                "mode": "batch_http_link",
                "subgraph": {
                    "all": { "enabled": true },
                    "subgraphs": { "a": { "enabled": false } },
                },
            },
            "circuit_breaker": {
                "subgraphs": { "a": { "consecutive_failures": 1, "open_duration": "1h" } },
            },
            "override_subgraph_url": {
                "a": format!("{}/a", subgraphs.uri()),
                "b": format!("{}/b", subgraphs.uri()),
            },
        }))?
        .schema(include_str!(
            "../../../tests/fixtures/batching/schema.graphql"
        ))
        .with_subgraph_network_requests()
        .build_router()
        .await?;

    let single = serde_json::json!({ "query": "{ entryA(count: 1) { index } }" });
    post_to_router(router.clone(), single).await?;

    let batch = serde_json::json!([
        { "query": "query op0 { entryA(count: 2) { index } }" },
        { "query": "query op1 { entryA(count: 2) { index } }" },
    ]);
    let response = tokio::time::timeout(Duration::from_secs(10), post_to_router(router, batch))
        .await
        .expect("the batch should be answered")?;
    let operations = response.as_array().expect("one response per operation");
    assert_eq!(operations.len(), 2, "{response}");
    for operation in operations {
        assert_eq!(
            operation["errors"][0]["extensions"]["code"],
            Error::CircuitBreakerOpen.code(),
            "a fetch that joins no batch should be turned away by the open circuit: {response}"
        );
    }

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

        let service =
            protected_subgraph(&plugin, "products", |req: subgraph::Request| async move {
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
            "apollo.qos.circuit_breaker.transitions",
            1,
            "apollo.qos.circuit_breaker.name" = "products",
            "apollo.qos.circuit_breaker.transition.from" = "closed",
            "apollo.qos.circuit_breaker.transition.to" = "open"
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
            "apollo.qos.circuit_breaker.transitions",
            1,
            "apollo.qos.circuit_breaker.name" = "products.api",
            "apollo.qos.circuit_breaker.transition.from" = "closed",
            "apollo.qos.circuit_breaker.transition.to" = "open"
        );
    }
    .with_metrics()
    .await;
}
