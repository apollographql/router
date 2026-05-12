mod config;
mod state;

use std::collections::HashMap;
use std::collections::HashSet;
use std::ops::ControlFlow;

use apollo_compiler::executable::ExecutableDocument;
use apollo_compiler::executable::Selection;
use apollo_compiler::executable::SelectionSet;
use apollo_federation::connectors::runtime::errors::Error as ConnectorError;
use apollo_federation::connectors::runtime::responses::MappedResponse;
use config::CircuitBreakerMode;
use config::Config;
use http::StatusCode;
use tower::BoxError;
use tower::ServiceBuilder;
use tower::ServiceExt;

use self::state::CheckResult;
use self::state::CircuitBreakerRegistry;
use self::state::CircuitKey;
use crate::graphql;
use crate::json_ext::PathElement;
use crate::layers::ServiceBuilderExt;
use crate::plugin::PluginInit;
use crate::plugin::PluginPrivate;
use crate::services::SubgraphResponse;
use crate::services::connector;
use crate::services::subgraph;

const SKIP_FIELDS: &[&str] = &["__typename", "_entities", "_service"];

/// Sentinel field coordinate used for connector circuit keys since connectors
/// don't have the same field-level granularity as subgraph operations.
const CONNECTOR_COORDINATE: &str = "_connector";

pub(crate) struct CircuitBreaking {
    config: Config,
}

/// Recursively walk an operation's selection set and collect type-qualified field
/// coordinates like `"Product.inventory"`. The parent type comes from
/// `selection_set.ty`, which `apollo_compiler` populates from the schema.
fn collect_coordinates(
    selection_set: &SelectionSet,
    doc: &ExecutableDocument,
    out: &mut HashSet<String>,
) {
    for selection in &selection_set.selections {
        match selection {
            Selection::Field(field) => {
                let field_name = field.name.as_str();
                if !SKIP_FIELDS.contains(&field_name) {
                    out.insert(format!("{}.{}", selection_set.ty, field_name));
                }
                if !field.selection_set.selections.is_empty() {
                    collect_coordinates(&field.selection_set, doc, out);
                }
            }
            Selection::InlineFragment(frag) => {
                collect_coordinates(&frag.selection_set, doc, out);
            }
            Selection::FragmentSpread(spread) => {
                if let Some(fragment) = doc.fragments.get(&spread.fragment_name) {
                    collect_coordinates(&fragment.selection_set, doc, out);
                }
            }
        }
    }
}

/// Sentinel coordinate used when no field-level information can be extracted.
const FALLBACK_COORDINATE: &str = "_operation";

/// Extract all type-qualified field coordinates from a subgraph request's
/// executable document. Falls back to a sentinel coordinate if the document
/// is unavailable (e.g. in tests with fake requests).
fn extract_selection_coordinates(req: &subgraph::Request) -> HashSet<String> {
    let mut coords = HashSet::new();
    if let Some(doc) = req.executable_document.as_ref() {
        let op_name = req.subgraph_request.body().operation_name.as_deref();
        if let Ok(operation) = doc.operations.get(op_name) {
            collect_coordinates(&operation.selection_set, doc, &mut coords);
        }
    }
    if coords.is_empty() {
        for field_name in req.root_operation_fields() {
            if !SKIP_FIELDS.contains(&field_name.as_str()) {
                coords.insert(format!("{}._root.{}", req.subgraph_name, field_name));
            }
        }
    }
    if coords.is_empty() {
        coords.insert(FALLBACK_COORDINATE.to_string());
    }
    coords
}

/// Build a reverse-lookup index: bare field name -> list of full coordinates.
fn build_field_name_index(coordinates: &HashSet<String>) -> HashMap<String, Vec<String>> {
    let mut index: HashMap<String, Vec<String>> = HashMap::new();
    for coord in coordinates {
        if let Some(field_name) = coord.split('.').next_back() {
            index
                .entry(field_name.to_string())
                .or_default()
                .push(coord.clone());
        }
    }
    index
}

/// Extract bare field names from GraphQL error paths.
fn extract_error_field_names(errors: &[graphql::Error]) -> HashSet<String> {
    let mut names = HashSet::new();
    for error in errors {
        if let Some(ref path) = error.path {
            for element in path.iter() {
                if let PathElement::Key(name, _) = element {
                    let s = name.as_str();
                    if !SKIP_FIELDS.contains(&s) {
                        names.insert(s.to_string());
                    }
                }
            }
        }
    }
    names
}

/// Coordinates stashed in the request context by the checkpoint layer so the
/// response handler can reuse them without recomputing.
#[derive(Clone)]
struct CachedCoordinates(HashSet<String>);

/// Data extracted from the request and passed through to the response handler
/// via `map_future_with_request_data`.
#[derive(Clone)]
struct RequestFieldData {
    subgraph_name: String,
    coordinates: HashSet<String>,
    field_name_index: HashMap<String, Vec<String>>,
}

fn emit_transition(key: &CircuitKey, t: &state::Transition) {
    tracing::info!(
        subgraph = %key.subgraph_name,
        field_coordinate = %key.field_coordinate,
        from = %t.from,
        to = %t.to,
        "circuit breaker state transition"
    );
    u64_counter!(
        "apollo.router.circuit_breaker.state_change",
        "Circuit breaker state transitions",
        1,
        "subgraph.name" = key.subgraph_name.clone(),
        "circuit_breaker.from_state" = t.from.to_string(),
        "circuit_breaker.to_state" = t.to.to_string()
    );
}

#[async_trait::async_trait]
impl PluginPrivate for CircuitBreaking {
    type Config = Config;

    async fn new(init: PluginInit<Self::Config>) -> Result<Self, BoxError> {
        Ok(Self {
            config: init.config,
        })
    }

    fn subgraph_service(&self, name: &str, service: subgraph::BoxService) -> subgraph::BoxService {
        let effective = self.config.effective_config(name);
        if !effective.enabled {
            return service;
        }
        if let Err(e) = effective.validate() {
            tracing::warn!(subgraph = %name, "{e} — circuit breaking disabled for this subgraph");
            return service;
        }

        let registry = CircuitBreakerRegistry::new(
            effective.error_threshold,
            effective.window,
            effective.recovery_timeout,
            effective.half_open_max_requests,
        );
        let mode = effective.mode;
        let subgraph_name = name.to_string();

        let check_registry = registry.clone();
        let check_subgraph = subgraph_name.clone();
        let response_registry = registry.clone();

        ServiceBuilder::new()
            .checkpoint(move |req: subgraph::Request| {
                let coordinates = extract_selection_coordinates(&req);
                req.context.extensions().with_lock(|lock| {
                    lock.insert(CachedCoordinates(coordinates.clone()));
                });

                let mut any_rejected = false;
                let mut rejected_coordinate = None;
                for coord in &coordinates {
                    let key = CircuitKey {
                        subgraph_name: check_subgraph.clone(),
                        field_coordinate: coord.clone(),
                    };
                    match check_registry.check(&key) {
                        CheckResult::Allowed(transition) => {
                            if let Some(ref t) = transition {
                                emit_transition(&key, t);
                            }
                        }
                        CheckResult::Rejected => {
                            any_rejected = true;
                            rejected_coordinate = Some(coord.clone());
                            break;
                        }
                    }
                }

                if any_rejected {
                    let coord = rejected_coordinate.unwrap_or_default();
                    let key = CircuitKey {
                        subgraph_name: check_subgraph.clone(),
                        field_coordinate: coord,
                    };
                    tracing::warn!(
                        subgraph = %key.subgraph_name,
                        field_coordinate = %key.field_coordinate,
                        "circuit breaker rejected request (circuit is open)"
                    );
                    u64_counter!(
                        "apollo.router.circuit_breaker.rejected",
                        "Requests rejected by circuit breaker",
                        1,
                        "subgraph.name" = key.subgraph_name.clone()
                    );
                    if mode == CircuitBreakerMode::Enforce {
                        Ok(ControlFlow::Break(
                            SubgraphResponse::error_builder()
                                .status_code(StatusCode::SERVICE_UNAVAILABLE)
                                .subgraph_name(key.subgraph_name.clone())
                                .error(circuit_breaker_open_error())
                                .context(req.context)
                                .build(),
                        ))
                    } else {
                        Ok(ControlFlow::Continue(req))
                    }
                } else {
                    Ok(ControlFlow::Continue(req))
                }
            })
            .map_future_with_request_data(
                move |req: &subgraph::Request| {
                    let coordinates = req
                        .context
                        .extensions()
                        .with_lock(|lock| lock.get::<CachedCoordinates>().cloned())
                        .map(|c| c.0)
                        .unwrap_or_else(|| extract_selection_coordinates(req));
                    let field_name_index = build_field_name_index(&coordinates);
                    RequestFieldData {
                        subgraph_name: subgraph_name.clone(),
                        coordinates,
                        field_name_index,
                    }
                },
                move |data: RequestFieldData, fut| {
                    let registry = response_registry.clone();
                    async move {
                        let response: Result<SubgraphResponse, BoxError> = fut.await;
                        match &response {
                            Ok(resp) => {
                                let errors = &resp.response.body().errors;
                                let is_error =
                                    !errors.is_empty() || !resp.response.status().is_success();

                                if is_error {
                                    let error_field_names = extract_error_field_names(errors);

                                    let errored_coords: HashSet<String> =
                                        if error_field_names.is_empty() {
                                            data.coordinates.clone()
                                        } else {
                                            let resolved: HashSet<String> = error_field_names
                                                .iter()
                                                .filter_map(|name| data.field_name_index.get(name))
                                                .flatten()
                                                .cloned()
                                                .collect();
                                            if resolved.is_empty() {
                                                data.coordinates.clone()
                                            } else {
                                                resolved
                                            }
                                        };

                                    for coord in &errored_coords {
                                        let key = CircuitKey {
                                            subgraph_name: data.subgraph_name.clone(),
                                            field_coordinate: coord.clone(),
                                        };
                                        if let Some(t) = registry.record_error(&key) {
                                            emit_transition(&key, &t);
                                        }
                                    }

                                    for coord in data.coordinates.difference(&errored_coords) {
                                        let key = CircuitKey {
                                            subgraph_name: data.subgraph_name.clone(),
                                            field_coordinate: coord.clone(),
                                        };
                                        if let Some(t) = registry.record_success(&key) {
                                            emit_transition(&key, &t);
                                        }
                                    }
                                } else {
                                    for coord in &data.coordinates {
                                        let key = CircuitKey {
                                            subgraph_name: data.subgraph_name.clone(),
                                            field_coordinate: coord.clone(),
                                        };
                                        if let Some(t) = registry.record_success(&key) {
                                            emit_transition(&key, &t);
                                        }
                                    }
                                }
                            }
                            Err(_) => {
                                for coord in &data.coordinates {
                                    let key = CircuitKey {
                                        subgraph_name: data.subgraph_name.clone(),
                                        field_coordinate: coord.clone(),
                                    };
                                    if let Some(t) = registry.record_error(&key) {
                                        emit_transition(&key, &t);
                                    }
                                }
                            }
                        }
                        response
                    }
                },
            )
            .service(service)
            .boxed()
    }

    fn connector_request_service(
        &self,
        service: connector::request_service::BoxService,
        source_name: String,
    ) -> connector::request_service::BoxService {
        let effective = self.config.effective_connector_config(&source_name);
        if !effective.enabled {
            return service;
        }
        if let Err(e) = effective.validate() {
            tracing::warn!(connector.source.name = %source_name, "{e} — circuit breaking disabled for this connector source");
            return service;
        }

        let registry = CircuitBreakerRegistry::new(
            effective.error_threshold,
            effective.window,
            effective.recovery_timeout,
            effective.half_open_max_requests,
        );
        let mode = effective.mode;

        let check_registry = registry.clone();
        let check_source = source_name.clone();
        let response_registry = registry.clone();
        let response_source = source_name.clone();

        ServiceBuilder::new()
            .checkpoint(move |req: connector::request_service::Request| {
                let key = CircuitKey {
                    subgraph_name: check_source.clone(),
                    field_coordinate: CONNECTOR_COORDINATE.to_string(),
                };
                match check_registry.check(&key) {
                    CheckResult::Allowed(transition) => {
                        if let Some(ref t) = transition {
                            emit_transition(&key, t);
                        }
                        Ok(ControlFlow::Continue(req))
                    }
                    CheckResult::Rejected => {
                        tracing::warn!(
                            connector.source.name = %key.subgraph_name,
                            "circuit breaker rejected connector request (circuit is open)"
                        );
                        u64_counter!(
                            "apollo.router.circuit_breaker.rejected",
                            "Requests rejected by circuit breaker",
                            1,
                            "connector.source.name" = key.subgraph_name.clone()
                        );
                        if mode == CircuitBreakerMode::Enforce {
                            Ok(ControlFlow::Break(
                                connector::request_service::Response::error_new(
                                    req.context,
                                    ConnectorError::TransportFailure(
                                        "Circuit breaker is open".to_string(),
                                    ),
                                    "Circuit breaker is open",
                                    req.key,
                                ),
                            ))
                        } else {
                            Ok(ControlFlow::Continue(req))
                        }
                    }
                }
            })
            .map_future_with_request_data(
                move |_req: &connector::request_service::Request| response_source.clone(),
                move |source: String, fut| {
                    let registry = response_registry.clone();
                    async move {
                        let response: Result<connector::request_service::Response, BoxError> =
                            fut.await;
                        let key = CircuitKey {
                            subgraph_name: source,
                            field_coordinate: CONNECTOR_COORDINATE.to_string(),
                        };
                        match &response {
                            Ok(resp) => {
                                let is_error = resp.transport_result.is_err()
                                    || matches!(resp.mapped_response, MappedResponse::Error { .. });
                                if is_error {
                                    if let Some(t) = registry.record_error(&key) {
                                        emit_transition(&key, &t);
                                    }
                                } else if let Some(t) = registry.record_success(&key) {
                                    emit_transition(&key, &t);
                                }
                            }
                            Err(_) => {
                                if let Some(t) = registry.record_error(&key) {
                                    emit_transition(&key, &t);
                                }
                            }
                        }
                        response
                    }
                },
            )
            .service(service)
            .boxed()
    }
}

fn circuit_breaker_open_error() -> graphql::Error {
    graphql::Error::builder()
        .message("Circuit breaker is open")
        .extension_code("CIRCUIT_BREAKER_OPEN")
        .build()
}

register_private_plugin!("apollo", "experimental_circuit_breaking", CircuitBreaking);

#[cfg(test)]
mod tests {
    use std::str::FromStr;
    use std::sync::Arc;
    use std::sync::atomic::AtomicU32;
    use std::sync::atomic::Ordering;

    use apollo_compiler::name;
    use apollo_federation::connectors::ConnectId;
    use apollo_federation::connectors::ConnectSpec;
    use apollo_federation::connectors::Connector;
    use apollo_federation::connectors::HttpJsonTransport;
    use apollo_federation::connectors::JSONSelection;
    use apollo_federation::connectors::SourceName;
    use apollo_federation::connectors::StringTemplate;
    use apollo_federation::connectors::runtime::http_json_transport::HttpRequest as ConnectorHttpRequest;
    use apollo_federation::connectors::runtime::http_json_transport::HttpResponse as ConnectorHttpResponse;
    use apollo_federation::connectors::runtime::http_json_transport::TransportRequest;
    use apollo_federation::connectors::runtime::http_json_transport::TransportResponse;
    use apollo_federation::connectors::runtime::key::ResponseKey;
    use http::StatusCode;

    use super::*;
    use crate::graphql;
    use crate::plugins::test::PluginTestHarness;
    use crate::plugins::test::ServiceHandle;
    use crate::services::connector;
    use crate::services::router::body;
    use crate::services::subgraph;

    #[test]
    fn circuit_breaker_error_has_expected_shape() {
        let err = circuit_breaker_open_error();
        assert_eq!(err.message, "Circuit breaker is open");
        assert_eq!(
            err.extensions.get("code").and_then(|v| v.as_str()),
            Some("CIRCUIT_BREAKER_OPEN")
        );
        assert!(
            err.extensions.get("service").is_none(),
            "error should not leak subgraph name"
        );
    }

    #[test]
    fn extract_error_field_names_from_paths() {
        use crate::json_ext::Path;

        let error_with_path = graphql::Error::builder()
            .message("fail")
            .extension_code("ERR")
            .path(Path::from("product/inventory"))
            .build();
        let error_without_path = graphql::Error::builder()
            .message("fail")
            .extension_code("ERR")
            .build();

        let names = extract_error_field_names(&[error_with_path, error_without_path]);
        assert!(names.contains("product"));
        assert!(names.contains("inventory"));
        assert_eq!(names.len(), 2);
    }

    #[test]
    fn build_field_name_index_works() {
        let mut coords = HashSet::new();
        coords.insert("Product.inventory".to_string());
        coords.insert("Product.name".to_string());
        coords.insert("Inventory.count".to_string());

        let index = build_field_name_index(&coords);
        assert_eq!(index.get("inventory").unwrap(), &["Product.inventory"]);
        assert_eq!(index.get("name").unwrap(), &["Product.name"]);
        assert_eq!(index.get("count").unwrap(), &["Inventory.count"]);
    }

    async fn build_harness(yaml: &str) -> PluginTestHarness<CircuitBreaking> {
        PluginTestHarness::builder()
            .config(yaml)
            .build()
            .await
            .expect("harness should build")
    }

    fn error_response() -> graphql::Error {
        graphql::Error::builder()
            .message("something went wrong")
            .extension_code("INTERNAL_ERROR")
            .build()
    }

    #[tokio::test]
    async fn disabled_plugin_passes_through() {
        let harness = build_harness(
            r#"
experimental_circuit_breaking:
  all:
    enabled: false
"#,
        )
        .await;

        let call_count = Arc::new(AtomicU32::new(0));
        let counter = call_count.clone();
        let service = harness.subgraph_service("products", move |req: subgraph::Request| {
            counter.fetch_add(1, Ordering::SeqCst);
            async move {
                Ok(subgraph::Response::fake_builder()
                    .context(req.context)
                    .build())
            }
        });

        service.call_default().await.unwrap();
        assert_eq!(call_count.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn trips_after_threshold_and_rejects() {
        let harness = build_harness(
            r#"
experimental_circuit_breaking:
  all:
    enabled: true
    error_threshold: 2
    window: 60s
    recovery_timeout: 300s
    mode: enforce
"#,
        )
        .await;

        let call_count = Arc::new(AtomicU32::new(0));
        let counter = call_count.clone();

        let service = harness.subgraph_service("products", move |req: subgraph::Request| {
            counter.fetch_add(1, Ordering::SeqCst);
            let err = error_response();
            async move {
                Ok(subgraph::Response::fake_builder()
                    .error(err)
                    .context(req.context)
                    .build())
            }
        });

        let resp = service.call_default().await.unwrap();
        assert!(!resp.response.body().errors.is_empty());

        let resp = service.call_default().await.unwrap();
        assert!(!resp.response.body().errors.is_empty());

        let resp = service.call_default().await.unwrap();
        assert_eq!(resp.response.status(), StatusCode::SERVICE_UNAVAILABLE);
        let err = &resp.response.body().errors[0];
        assert_eq!(
            err.extensions.get("code").and_then(|v| v.as_str()),
            Some("CIRCUIT_BREAKER_OPEN")
        );

        assert_eq!(call_count.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn measure_mode_does_not_reject() {
        let harness = build_harness(
            r#"
experimental_circuit_breaking:
  all:
    enabled: true
    error_threshold: 1
    window: 60s
    recovery_timeout: 300s
    mode: measure
"#,
        )
        .await;

        let call_count = Arc::new(AtomicU32::new(0));
        let counter = call_count.clone();

        let service = harness.subgraph_service("products", move |req: subgraph::Request| {
            counter.fetch_add(1, Ordering::SeqCst);
            let err = error_response();
            async move {
                Ok(subgraph::Response::fake_builder()
                    .error(err)
                    .context(req.context)
                    .build())
            }
        });

        service.call_default().await.unwrap();

        let resp = service.call_default().await.unwrap();
        assert_ne!(resp.response.status(), StatusCode::SERVICE_UNAVAILABLE);

        assert_eq!(call_count.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn successful_responses_do_not_trip() {
        let harness = build_harness(
            r#"
experimental_circuit_breaking:
  all:
    enabled: true
    error_threshold: 2
    window: 60s
    recovery_timeout: 300s
    mode: enforce
"#,
        )
        .await;

        let call_count = Arc::new(AtomicU32::new(0));
        let counter = call_count.clone();

        let service = harness.subgraph_service("products", move |req: subgraph::Request| {
            counter.fetch_add(1, Ordering::SeqCst);
            async move {
                Ok(subgraph::Response::fake_builder()
                    .context(req.context)
                    .build())
            }
        });

        for _ in 0..10 {
            let resp = service.call_default().await.unwrap();
            assert!(resp.response.body().errors.is_empty());
        }
        assert_eq!(call_count.load(Ordering::SeqCst), 10);
    }

    #[tokio::test]
    async fn per_subgraph_config_applies_independently() {
        let harness = build_harness(
            r#"
experimental_circuit_breaking:
  all:
    enabled: true
    error_threshold: 100
    window: 60s
    recovery_timeout: 300s
    mode: enforce
  subgraphs:
    products:
      enabled: true
      error_threshold: 1
      window: 60s
      recovery_timeout: 300s
      mode: enforce
"#,
        )
        .await;

        let products_calls = Arc::new(AtomicU32::new(0));
        let pc = products_calls.clone();
        let products_svc = harness.subgraph_service("products", move |req: subgraph::Request| {
            pc.fetch_add(1, Ordering::SeqCst);
            let err = error_response();
            async move {
                Ok(subgraph::Response::fake_builder()
                    .error(err)
                    .context(req.context)
                    .build())
            }
        });

        products_svc.call_default().await.unwrap();
        let resp = products_svc.call_default().await.unwrap();
        assert_eq!(resp.response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(products_calls.load(Ordering::SeqCst), 1);

        let reviews_calls = Arc::new(AtomicU32::new(0));
        let rc = reviews_calls.clone();
        let reviews_svc = harness.subgraph_service("reviews", move |req: subgraph::Request| {
            rc.fetch_add(1, Ordering::SeqCst);
            let err = error_response();
            async move {
                Ok(subgraph::Response::fake_builder()
                    .error(err)
                    .context(req.context)
                    .build())
            }
        });

        reviews_svc.call_default().await.unwrap();
        reviews_svc.call_default().await.unwrap();
        assert_eq!(reviews_calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn recovery_allows_probe_then_closes() {
        let harness = build_harness(
            r#"
experimental_circuit_breaking:
  all:
    enabled: true
    error_threshold: 1
    window: 60s
    recovery_timeout: 0s
    half_open_max_requests: 1
    mode: enforce
"#,
        )
        .await;

        let call_count = Arc::new(AtomicU32::new(0));
        let should_error = Arc::new(std::sync::atomic::AtomicBool::new(true));

        let counter = call_count.clone();
        let err_flag = should_error.clone();
        let service = harness.subgraph_service("products", move |req: subgraph::Request| {
            counter.fetch_add(1, Ordering::SeqCst);
            let make_error = err_flag.load(Ordering::SeqCst);
            async move {
                if make_error {
                    Ok(subgraph::Response::fake_builder()
                        .error(error_response())
                        .context(req.context)
                        .build())
                } else {
                    Ok(subgraph::Response::fake_builder()
                        .context(req.context)
                        .build())
                }
            }
        });

        service.call_default().await.unwrap();
        assert_eq!(call_count.load(Ordering::SeqCst), 1);

        should_error.store(false, Ordering::SeqCst);
        let resp = service.call_default().await.unwrap();
        assert!(resp.response.body().errors.is_empty());
        assert_eq!(call_count.load(Ordering::SeqCst), 2);

        let resp = service.call_default().await.unwrap();
        assert!(resp.response.body().errors.is_empty());
        assert_eq!(call_count.load(Ordering::SeqCst), 3);
    }

    // --- Connector test helpers ---

    fn test_connector() -> Connector {
        Connector {
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
            label: "test_connector".into(),
        }
    }

    fn test_response_key() -> ResponseKey {
        ResponseKey::RootField {
            name: "users".to_string(),
            inputs: Default::default(),
            selection: Arc::new(JSONSelection::parse("$.data").unwrap()),
        }
    }

    fn connector_request() -> connector::request_service::Request {
        let http_request = http::Request::builder().body("".into()).unwrap();
        connector::request_service::Request {
            context: crate::Context::default(),
            connector: Arc::new(test_connector()),
            transport_request: TransportRequest::Http(Box::new(ConnectorHttpRequest {
                inner: http_request,
                debug: Default::default(),
            })),
            key: test_response_key(),
            mapping_problems: vec![],
            supergraph_request: Default::default(),
            operation: Default::default(),
        }
    }

    fn connector_success_response(
        req: connector::request_service::Request,
    ) -> connector::request_service::Response {
        connector::request_service::Response {
            context: req.context,
            transport_result: Ok(TransportResponse::Http(ConnectorHttpResponse {
                inner: http::Response::builder()
                    .status(200)
                    .body(body::empty())
                    .unwrap()
                    .into_parts()
                    .0,
            })),
            mapped_response: MappedResponse::Data {
                data: serde_json::json!({}).into(),
                problems: vec![],
                key: test_response_key(),
            },
        }
    }

    fn connector_error_response(
        req: connector::request_service::Request,
    ) -> connector::request_service::Response {
        connector::request_service::Response::error_new(
            req.context,
            ConnectorError::TransportFailure("test error".into()),
            "something went wrong",
            req.key,
        )
    }

    // --- Connector tests ---

    fn build_connector_service(
        harness: &PluginTestHarness<CircuitBreaking>,
        response_fn: impl Fn(
            connector::request_service::Request,
        ) -> connector::request_service::Response
        + Send
        + Sync
        + Clone
        + 'static,
    ) -> ServiceHandle<connector::request_service::Request, connector::request_service::BoxService>
    {
        let inner: connector::request_service::BoxService =
            connector::request_service::BoxService::new(ServiceBuilder::new().service_fn(
                move |req: connector::request_service::Request| {
                    let response_fn = response_fn.clone();
                    async move { Ok((response_fn)(req)) }
                },
            ));
        ServiceHandle::new(harness.connector_request_service(inner, "my_connector".to_string()))
    }

    #[tokio::test]
    async fn connector_disabled_passes_through() {
        let harness = build_harness(
            r#"
experimental_circuit_breaking:
  connector:
    all:
      enabled: false
"#,
        )
        .await;

        let svc = build_connector_service(&harness, connector_success_response);
        let resp = svc.call(connector_request()).await.unwrap();
        assert!(resp.transport_result.is_ok());
    }

    #[tokio::test]
    async fn connector_trips_after_threshold() {
        let harness = build_harness(
            r#"
experimental_circuit_breaking:
  connector:
    all:
      enabled: true
      error_threshold: 2
      window: 60s
      recovery_timeout: 300s
      mode: enforce
"#,
        )
        .await;

        let svc = build_connector_service(&harness, connector_error_response);

        let resp = svc.call(connector_request()).await.unwrap();
        assert!(resp.transport_result.is_err());

        let resp = svc.call(connector_request()).await.unwrap();
        assert!(resp.transport_result.is_err());

        // Third request should be rejected by the circuit breaker
        let resp = svc.call(connector_request()).await.unwrap();
        assert!(resp.transport_result.is_err());
        if let MappedResponse::Error { ref error, .. } = resp.mapped_response {
            assert!(
                error.message.contains("Circuit breaker is open"),
                "expected circuit breaker message, got: {}",
                error.message
            );
        } else {
            panic!("expected MappedResponse::Error");
        }
    }

    #[tokio::test]
    async fn connector_measure_mode_does_not_reject() {
        let harness = build_harness(
            r#"
experimental_circuit_breaking:
  connector:
    all:
      enabled: true
      error_threshold: 1
      window: 60s
      recovery_timeout: 300s
      mode: measure
"#,
        )
        .await;

        let svc = build_connector_service(&harness, connector_error_response);

        // Trip the threshold
        svc.call(connector_request()).await.unwrap();

        // Measure mode should not reject
        let resp = svc.call(connector_request()).await.unwrap();
        assert!(resp.transport_result.is_err());
        if let MappedResponse::Error { ref error, .. } = resp.mapped_response {
            assert!(
                !error.message.contains("Circuit breaker is open"),
                "measure mode should not produce circuit breaker errors"
            );
        }
    }

    #[tokio::test]
    async fn connector_and_subgraph_circuits_are_independent() {
        let harness = build_harness(
            r#"
experimental_circuit_breaking:
  all:
    enabled: true
    error_threshold: 1
    window: 60s
    recovery_timeout: 300s
    mode: enforce
  connector:
    all:
      enabled: true
      error_threshold: 100
      window: 60s
      recovery_timeout: 300s
      mode: enforce
"#,
        )
        .await;

        // Trip the subgraph circuit
        let subgraph_svc = harness.subgraph_service("products", move |req: subgraph::Request| {
            let err = error_response();
            async move {
                Ok(subgraph::Response::fake_builder()
                    .error(err)
                    .context(req.context)
                    .build())
            }
        });
        subgraph_svc.call_default().await.unwrap();
        let resp = subgraph_svc.call_default().await.unwrap();
        assert_eq!(resp.response.status(), StatusCode::SERVICE_UNAVAILABLE);

        // Connector should still work fine (independent circuit with high threshold)
        let connector_svc = build_connector_service(&harness, connector_success_response);
        let resp = connector_svc.call(connector_request()).await.unwrap();
        assert!(resp.transport_result.is_ok());
    }

    #[tokio::test]
    async fn connector_per_source_config_overrides() {
        let harness = build_harness(
            r#"
experimental_circuit_breaking:
  connector:
    all:
      enabled: true
      error_threshold: 100
      window: 60s
      recovery_timeout: 300s
      mode: enforce
    sources:
      my_connector:
        enabled: true
        error_threshold: 1
        window: 60s
        recovery_timeout: 300s
        mode: enforce
"#,
        )
        .await;

        let svc = build_connector_service(&harness, connector_error_response);

        // Trip the circuit with just 1 error (per-source override)
        svc.call(connector_request()).await.unwrap();

        let resp = svc.call(connector_request()).await.unwrap();
        if let MappedResponse::Error { ref error, .. } = resp.mapped_response {
            assert!(
                error.message.contains("Circuit breaker is open"),
                "per-source config should override all: {}",
                error.message
            );
        } else {
            panic!("expected circuit breaker rejection");
        }
    }
}
