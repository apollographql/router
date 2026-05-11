mod config;
mod state;

use std::collections::HashMap;
use std::collections::HashSet;
use std::ops::ControlFlow;

use apollo_compiler::executable::ExecutableDocument;
use apollo_compiler::executable::Selection;
use apollo_compiler::executable::SelectionSet;
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
use crate::services::subgraph;

const SKIP_FIELDS: &[&str] = &["__typename", "_entities", "_service"];

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
        // TODO(circuit_breaking): This will be high cardinality, remove?
        "circuit_breaker.field_coordinate" = key.field_coordinate.clone(),
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
                        "subgraph.name" = key.subgraph_name.clone(),
                        // TODO(circuit_breaking): This will be high cardinality, remove?
                        "circuit_breaker.field_coordinate" = key.field_coordinate.clone()
                    );
                    if mode == CircuitBreakerMode::Enforce {
                        Ok(ControlFlow::Break(
                            SubgraphResponse::error_builder()
                                .status_code(StatusCode::SERVICE_UNAVAILABLE)
                                .subgraph_name(key.subgraph_name.clone())
                                .error(circuit_breaker_open_error(&key))
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
                    let coordinates = extract_selection_coordinates(req);
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
}

fn circuit_breaker_open_error(key: &CircuitKey) -> graphql::Error {
    graphql::Error::builder()
        // TODO(circuit_breaking): This should not be user facing
        .message(format!(
            "Circuit breaker is open for subgraph '{}' (field: {})",
            key.subgraph_name, key.field_coordinate
        ))
        // TODO(circuit_breaking): Check if this code makes sense
        .extension_code("CIRCUIT_BREAKER_OPEN")
        .extension("service", key.subgraph_name.as_str())
        .build()
}

register_private_plugin!("apollo", "circuit_breaking", CircuitBreaking);

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::AtomicU32;
    use std::sync::atomic::Ordering;

    use http::StatusCode;

    use super::*;
    use crate::graphql;
    use crate::plugins::test::PluginTestHarness;
    use crate::services::subgraph;

    #[test]
    fn circuit_breaker_error_has_expected_shape() {
        let key = CircuitKey {
            subgraph_name: "products".to_string(),
            field_coordinate: "Product.inventory".to_string(),
        };
        let err = circuit_breaker_open_error(&key);
        assert!(
            err.message
                .starts_with("Circuit breaker is open for subgraph 'products'")
        );
        assert!(err.message.contains("Product.inventory"));
        assert_eq!(
            err.extensions.get("code").and_then(|v| v.as_str()),
            Some("CIRCUIT_BREAKER_OPEN")
        );
        assert_eq!(
            err.extensions.get("service").and_then(|v| v.as_str()),
            Some("products")
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
circuit_breaking:
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
circuit_breaking:
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
circuit_breaking:
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
circuit_breaking:
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
circuit_breaking:
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
circuit_breaking:
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
}
