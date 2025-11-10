//! Implements support for persisted queries and safelisting at the supergraph service stage.

mod freeform_graphql_behavior;
mod id_extractor;
mod manifest;
mod manifest_poller;

#[cfg(test)]
use std::sync::Arc;

use http::HeaderValue;
use http::StatusCode;
use http::header::CACHE_CONTROL;
use id_extractor::PersistedQueryIdExtractor;
pub use manifest::FullPersistedQueryOperationId;
pub use manifest::ManifestOperation;
pub use manifest::PersistedQueryManifest;
pub(crate) use manifest_poller::PersistedQueryManifestPoller;
use tower::BoxError;

use super::query_analysis::ParsedDocument;
use crate::Configuration;
use crate::graphql::Error as GraphQLError;
use crate::plugins::telemetry::CLIENT_NAME;
use crate::services::SupergraphRequest;
use crate::services::SupergraphResponse;

const DONT_CACHE_RESPONSE_VALUE: &str = "private, no-cache, must-revalidate";
const PERSISTED_QUERIES_CLIENT_NAME_CONTEXT_KEY: &str = "apollo_persisted_queries::client_name";
const PERSISTED_QUERIES_OPERATION_ID_CONTEXT_KEY: &str = "apollo_persisted_queries::operation_id";
const PERSISTED_QUERIES_SAFELIST_SKIP_ENFORCEMENT_CONTEXT_KEY: &str =
    "apollo_persisted_queries::safelist::skip_enforcement";

/// Marker type for request context to identify requests that were expanded from a persisted query
/// ID.
struct UsedQueryIdFromManifest;

/// Stores the PQ ID for an operation expanded from a PQ ID or an operation that matches a PQ body in the manifest.
#[derive(Clone)]
pub(crate) struct RequestPersistedQueryId {
    pub(crate) pq_id: String,
}

/// Implements persisted query support, namely expanding requests using persisted query IDs and
/// filtering free-form GraphQL requests based on router configuration.
///
/// Despite the name, this is not really in any way a layer today.
///
/// This type actually consists of two conceptual layers that must both be applied at the supergraph
/// service stage, at different points:
/// - [PersistedQueryLayer::supergraph_request] must be done *before* the GraphQL request is parsed
///   and validated.
/// - [PersistedQueryLayer::supergraph_request_with_analyzed_query] must be done *after* the
///   GraphQL request is parsed and validated.
#[derive(Debug)]
pub(crate) struct PersistedQueryLayer {
    /// Manages polling uplink for persisted queries and caches the current
    /// value of the manifest and projected safelist. None if the layer is disabled.
    pub(crate) manifest_poller: Option<PersistedQueryManifestPoller>,
    introspection_enabled: bool,
}

fn skip_enforcement(request: &SupergraphRequest) -> bool {
    request
        .context
        .get(PERSISTED_QUERIES_SAFELIST_SKIP_ENFORCEMENT_CONTEXT_KEY)
        .unwrap_or_default()
        .unwrap_or(false)
}

impl PersistedQueryLayer {
    /// Create a new [`PersistedQueryLayer`] from CLI options, YAML configuration,
    /// and optionally, an existing persisted query manifest poller.
    pub(crate) async fn new(configuration: &Configuration) -> Result<Self, BoxError> {
        if configuration.persisted_queries.enabled {
            Ok(Self {
                manifest_poller: Some(
                    PersistedQueryManifestPoller::new(configuration.clone()).await?,
                ),
                introspection_enabled: configuration.supergraph.introspection,
            })
        } else {
            Ok(Self {
                manifest_poller: None,
                introspection_enabled: configuration.supergraph.introspection,
            })
        }
    }

    /// Handles pre-parsing work for requests using persisted queries.
    ///
    /// Takes care of:
    /// 1) resolving a persisted query ID to a query body
    /// 2) rejecting free-form GraphQL requests if they are never allowed by configuration.
    ///    Matching against safelists is done later in
    ///    [`PersistedQueryLayer::supergraph_request_with_analyzed_query`].
    ///
    /// This functions similarly to a checkpoint service, short-circuiting the pipeline on error
    /// (using an `Err()` return value).
    /// The user of this function is responsible for propagating short-circuiting.
    #[allow(clippy::result_large_err)]
    pub(crate) fn supergraph_request(
        &self,
        request: SupergraphRequest,
    ) -> Result<SupergraphRequest, SupergraphResponse> {
        if let Some(manifest_poller) = &self.manifest_poller {
            if let Some(persisted_query_id) = PersistedQueryIdExtractor::extract_id(&request) {
                self.replace_query_id_with_operation_body(
                    request,
                    manifest_poller,
                    &persisted_query_id,
                )
            } else if skip_enforcement(&request) {
                // A plugin told us to allow this, so let's skip to require_id check.
                Ok(request)
            } else if let Some(log_unknown) = manifest_poller.never_allows_freeform_graphql() {
                // If we don't have an ID and we require an ID, return an error immediately,
                if log_unknown
                    && let Some(operation_body) = request.supergraph_request.body().query.as_ref()
                {
                    // Note: it's kind of inconsistent that if we require
                    // IDs and skip_enforcement is set, we don't call
                    // log_unknown_operation on freeform GraphQL, but if we
                    // *don't* require IDs and skip_enforcement is set, we
                    // *do* call log_unknown_operation on unknown
                    // operations.
                    log_unknown_operation(operation_body, false);
                }
                Err(supergraph_err_pq_id_required(request))
            } else {
                // Let the freeform document (or complete lack of a document) be
                // parsed by the query analysis layer. We'll be back with
                // supergraph_request_with_analyzed_query soon to apply our
                // safelist, if any.
                Ok(request)
            }
        } else {
            // PQ layer is entirely disabled.
            Ok(request)
        }
    }

    /// Places an operation body on a [`SupergraphRequest`] if it has been persisted
    #[allow(clippy::result_large_err)]
    pub(crate) fn replace_query_id_with_operation_body(
        &self,
        mut request: SupergraphRequest,
        manifest_poller: &PersistedQueryManifestPoller,
        persisted_query_id: &str,
    ) -> Result<SupergraphRequest, SupergraphResponse> {
        if request.supergraph_request.body().query.is_some() {
            if manifest_poller.augmenting_apq_with_pre_registration_and_no_safelisting() {
                // Providing both a query string and an ID is how the clients of
                // the APQ feature (which is incompatible with safelisting and
                // log_unknown) register an operation. We let the APQ layer
                // handle this instead of handling it ourselves. Note that we
                // still may end up checking it against the safelist for the
                // purpose of log_unknown!
                Ok(request)
            } else {
                Err(supergraph_err_cannot_send_id_and_body_with_apq_disabled(
                    request,
                ))
            }
        } else {
            // if there is no query, look up the persisted query in the manifest
            // and put the body on the `supergraph_request`
            if let Some(persisted_query_body) = manifest_poller.get_operation_body(
                persisted_query_id,
                // Use the first one of these that exists:
                // - The PQL-specific context name entry
                //   `apollo_persisted_queries::client_name` (which can be set
                //   by router_service plugins)
                // - The same name used by telemetry (ie, the value of the
                //   header named by `telemetry.apollo.client_name_header`,
                //   which defaults to `apollographql-client-name` by default)
                request
                    .context
                    .get(PERSISTED_QUERIES_CLIENT_NAME_CONTEXT_KEY)
                    .unwrap_or_default()
                    .or_else(|| request.context.get(CLIENT_NAME).unwrap_or_default()),
            ) {
                let body = request.supergraph_request.body_mut();
                body.query = Some(persisted_query_body);
                // Note that we always remove this extension even if the ID was
                // set in the context by a plugin, so that the request doesn't
                // look like an APQ register request.
                body.extensions.remove("persistedQuery");
                request.context.extensions().with_lock(|lock| {
                    // Record that we actually used our ID, so we can skip the
                    // safelist check later.
                    lock.insert(UsedQueryIdFromManifest);
                    // Also store the actual PQ ID for usage reporting.
                    lock.insert(RequestPersistedQueryId {
                        pq_id: persisted_query_id.into(),
                    });
                });
                u64_counter!(
                    "apollo.router.operations.persisted_queries",
                    "Total requests with persisted queries enabled",
                    1
                );
                Ok(request)
            } else if manifest_poller.augmenting_apq_with_pre_registration_and_no_safelisting() {
                // The query ID isn't in our manifest, but we have APQ enabled
                // (and no safelisting) so we just let APQ handle it instead of
                // returning an error. (We still might check against the
                // safelist later for log_unknown!)
                Ok(request)
            } else {
                u64_counter!(
                    "apollo.router.operations.persisted_queries",
                    "Total requests with persisted queries enabled",
                    1,
                    persisted_queries.not_found = true
                );
                // if APQ is not enabled, return an error indicating the query was not found
                Err(supergraph_err_operation_not_found(
                    request,
                    persisted_query_id,
                ))
            }
        }
    }

    /// Handles post-GraphQL-parsing work for requests using the persisted queries feature,
    /// in particular safelisting.
    ///
    /// Any request that was expanded by the [`PersistedQueryLayer::supergraph_request`] call is
    /// passed through immediately. Free-form GraphQL is matched against safelists and rejected or
    /// passed through based on router configuration.
    ///
    /// This functions similarly to a checkpoint service, short-circuiting the pipeline on error
    /// (using an `Err()` return value).
    /// The user of this function is responsible for propagating short-circuiting.
    pub(crate) async fn supergraph_request_with_analyzed_query(
        &self,
        request: SupergraphRequest,
    ) -> Result<SupergraphRequest, SupergraphResponse> {
        let manifest_poller = match &self.manifest_poller {
            // PQ feature entirely disabled; just pass through.
            None => return Ok(request),
            Some(mp) => mp,
        };

        let operation_body = match request.supergraph_request.body().query.as_ref() {
            // if the request doesn't have a `query` document, continue with normal execution, which
            // will result in the normal no-operation error.
            None => return Ok(request),
            Some(ob) => ob,
        };

        let doc = {
            if request
                .context
                .extensions()
                .with_lock(|lock| lock.get::<UsedQueryIdFromManifest>().is_some())
            {
                return Ok(request);
            }

            let doc_opt = request
                .context
                .extensions()
                .with_lock(|lock| lock.get::<ParsedDocument>().cloned());

            match doc_opt {
                None => {
                    // For some reason, QueryAnalysisLayer didn't give us a document?
                    return Err(supergraph_err(
                        graphql_err(
                            "MISSING_PARSED_OPERATION",
                            "internal error: executable document missing",
                        ),
                        request,
                        ErrorCacheStrategy::DontCache,
                        StatusCode::INTERNAL_SERVER_ERROR,
                    ));
                }
                Some(d) => d,
            }
        };

        // If introspection is enabled in this server, all introspection
        // requests are always allowed. (This means any document all of whose
        // top-level fields in all operations (after spreading fragments) are
        // __type/__schema/__typename.) We do want to make sure the document
        // parsed properly before poking around at it, though.
        if self.introspection_enabled
            && doc
                .executable
                .operations
                .iter()
                .all(|op| op.is_introspection(&doc.executable))
        {
            return Ok(request);
        }

        let mut metric_attributes = vec![];
        let freeform_graphql_action = manifest_poller.action_for_freeform_graphql(Ok(&doc.ast));
        let skip_enforcement = skip_enforcement(&request);
        let allow = skip_enforcement || freeform_graphql_action.should_allow;
        if !allow {
            metric_attributes.push(opentelemetry::KeyValue::new(
                "persisted_queries.safelist.rejected.unknown".to_string(),
                true,
            ));
        } else if !freeform_graphql_action.should_allow {
            metric_attributes.push(opentelemetry::KeyValue::new(
                "persisted_queries.safelist.enforcement_skipped".to_string(),
                true,
            ));
        }
        if freeform_graphql_action.should_log {
            log_unknown_operation(operation_body, skip_enforcement);
            metric_attributes.push(opentelemetry::KeyValue::new(
                "persisted_queries.logged".to_string(),
                true,
            ));
        }

        // Store PQ ID for reporting if it was used
        if let Some(pq_id) = freeform_graphql_action.pq_id {
            request
                .context
                .extensions()
                .with_lock(|lock| lock.insert(RequestPersistedQueryId { pq_id }));
        }

        u64_counter!(
            "apollo.router.operations.persisted_queries",
            "Total requests with persisted queries enabled",
            1,
            metric_attributes
        );

        if allow {
            Ok(request)
        } else {
            Err(supergraph_err_operation_not_in_safelist(request))
        }
    }

    pub(crate) fn all_operations(&self) -> Option<Vec<String>> {
        self.manifest_poller
            .as_ref()
            .map(|poller| poller.get_all_operations())
    }
}

fn log_unknown_operation(operation_body: &str, enforcement_skipped: bool) {
    tracing::warn!(
        message = "unknown operation",
        operation_body,
        enforcement_skipped
    );
}

#[derive(Debug, Clone, Eq, PartialEq)]
enum ErrorCacheStrategy {
    Cache,
    DontCache,
}

impl ErrorCacheStrategy {
    fn get_supergraph_response(
        &self,
        graphql_error: GraphQLError,
        request: SupergraphRequest,
        status_code: StatusCode,
    ) -> SupergraphResponse {
        let mut error_builder = SupergraphResponse::error_builder()
            .error(graphql_error)
            .status_code(status_code)
            .context(request.context);

        if matches!(self, Self::DontCache) {
            // Persisted query errors (especially "not registered") need to be uncached, because
            // if we accidentally end up in a state where clients are "ahead" of Routers,
            // we don't want them to get "stuck" believing we don't know the PQ if we
            // catch up afterwards.
            error_builder = error_builder.header(
                CACHE_CONTROL,
                HeaderValue::from_static(DONT_CACHE_RESPONSE_VALUE),
            );
        }

        error_builder.build().expect("response is valid")
    }
}

fn graphql_err_operation_not_found(
    persisted_query_id: &str,
    operation_name: Option<String>,
) -> GraphQLError {
    let mut builder = GraphQLError::builder()
        .extension_code("PERSISTED_QUERY_NOT_IN_LIST")
        .message(format!(
            "Persisted query '{persisted_query_id}' not found in the persisted query list"
        ));
    if let Some(operation_name) = operation_name {
        builder = builder.extension("operation_name", operation_name);
    }
    builder.build()
}

fn supergraph_err_operation_not_found(
    request: SupergraphRequest,
    persisted_query_id: &str,
) -> SupergraphResponse {
    supergraph_err(
        graphql_err_operation_not_found(
            persisted_query_id,
            request.supergraph_request.body().operation_name.clone(),
        ),
        request,
        ErrorCacheStrategy::DontCache,
        StatusCode::NOT_FOUND,
    )
}

fn graphql_err_cannot_send_id_and_body() -> GraphQLError {
    graphql_err(
        "CANNOT_SEND_PQ_ID_AND_BODY",
        "Sending a persisted query ID and a body in the same request is disallowed",
    )
}

fn supergraph_err_cannot_send_id_and_body_with_apq_disabled(
    request: SupergraphRequest,
) -> SupergraphResponse {
    supergraph_err(
        graphql_err_cannot_send_id_and_body(),
        request,
        ErrorCacheStrategy::DontCache,
        StatusCode::BAD_REQUEST,
    )
}

fn graphql_err_operation_not_in_safelist() -> GraphQLError {
    graphql_err(
        "QUERY_NOT_IN_SAFELIST",
        "The operation body was not found in the persisted query safelist",
    )
}

fn supergraph_err_operation_not_in_safelist(request: SupergraphRequest) -> SupergraphResponse {
    supergraph_err(
        graphql_err_operation_not_in_safelist(),
        request,
        ErrorCacheStrategy::DontCache,
        StatusCode::FORBIDDEN,
    )
}

fn graphql_err_pq_id_required() -> GraphQLError {
    graphql_err(
        "PERSISTED_QUERY_ID_REQUIRED",
        "This endpoint does not allow freeform GraphQL requests; operations must be sent by ID in the persisted queries GraphQL extension.",
    )
}

fn supergraph_err_pq_id_required(request: SupergraphRequest) -> SupergraphResponse {
    supergraph_err(
        graphql_err_pq_id_required(),
        request,
        ErrorCacheStrategy::Cache,
        StatusCode::BAD_REQUEST,
    )
}

fn graphql_err(code: &str, message: &str) -> GraphQLError {
    GraphQLError::builder()
        .extension_code(code)
        .message(message)
        .build()
}

fn supergraph_err(
    graphql_error: GraphQLError,
    request: SupergraphRequest,
    cache_strategy: ErrorCacheStrategy,
    status_code: StatusCode,
) -> SupergraphResponse {
    cache_strategy.get_supergraph_response(graphql_error, request, status_code)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use serde_json::json;
    use tracing::instrument::WithSubscriber;

    use super::manifest::ManifestOperation;
    use super::*;
    use crate::Context;
    use crate::assert_errors_eq_ignoring_id;
    use crate::assert_snapshot_subscriber;
    use crate::configuration::Apq;
    use crate::configuration::PersistedQueries;
    use crate::configuration::PersistedQueriesSafelist;
    use crate::configuration::Supergraph;
    use crate::graphql;
    use crate::metrics::FutureMetricsExt;
    use crate::services::layers::persisted_queries::freeform_graphql_behavior::FreeformGraphQLBehavior;
    use crate::services::layers::query_analysis::QueryAnalysisLayer;
    use crate::spec::Schema;
    use crate::test_harness::mocks::persisted_queries::*;

    #[tokio::test(flavor = "multi_thread")]
    async fn disabled_pq_layer_has_no_poller() {
        let (_mock_guard, uplink_config) = mock_empty_pq_uplink().await;
        let pq_layer = PersistedQueryLayer::new(
            &Configuration::fake_builder()
                .persisted_query(PersistedQueries::builder().enabled(false).build())
                .uplink(uplink_config)
                .build()
                .unwrap(),
        )
        .await
        .unwrap();
        assert!(pq_layer.manifest_poller.is_none());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn enabled_pq_layer_has_poller() {
        let (_mock_guard, uplink_config) = mock_empty_pq_uplink().await;
        let pq_layer = PersistedQueryLayer::new(
            &Configuration::fake_builder()
                .persisted_query(PersistedQueries::builder().enabled(true).build())
                .uplink(uplink_config)
                .build()
                .unwrap(),
        )
        .await
        .unwrap();
        assert!(pq_layer.manifest_poller.is_some())
    }

    #[tokio::test]
    async fn poller_waits_to_start() {
        let (_id, _body, manifest) = fake_manifest();
        let delay = Duration::from_secs(2);
        let (_mock_guard, uplink_config) = mock_pq_uplink_with_delay(&manifest, delay).await;
        let now = tokio::time::Instant::now();

        assert!(
            PersistedQueryManifestPoller::new(
                Configuration::fake_builder()
                    .uplink(uplink_config)
                    .build()
                    .unwrap(),
            )
            .await
            .is_ok()
        );

        assert!(now.elapsed() >= delay);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn enabled_pq_layer_can_run_pq() {
        let (id, body, manifest) = fake_manifest();

        let (_mock_guard, uplink_config) = mock_pq_uplink(&manifest).await;

        let pq_layer = PersistedQueryLayer::new(
            &Configuration::fake_builder()
                .persisted_query(PersistedQueries::builder().enabled(true).build())
                .uplink(uplink_config)
                .build()
                .unwrap(),
        )
        .await
        .unwrap();
        let incoming_request = SupergraphRequest::fake_builder()
            .extension("persistedQuery", json!({"version": 1, "sha256Hash": id}))
            .build()
            .unwrap();

        assert!(incoming_request.supergraph_request.body().query.is_none());

        let result = pq_layer.supergraph_request(incoming_request);
        let request =
            result.expect("pq layer returned response instead of putting the query on the request");
        assert_eq!(request.supergraph_request.body().query, Some(body));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn enabled_pq_layer_with_client_names() {
        let manifest = PersistedQueryManifest::from(vec![
            ManifestOperation {
                id: "both-plain-and-cliented".to_string(),
                body: "query { bpac_no_client: __typename }".to_string(),
                client_name: None,
            },
            ManifestOperation {
                id: "both-plain-and-cliented".to_string(),
                body: "query { bpac_web_client: __typename }".to_string(),
                client_name: Some("web".to_string()),
            },
            ManifestOperation {
                id: "only-cliented".to_string(),
                body: "query { oc_web_client: __typename }".to_string(),
                client_name: Some("web".to_string()),
            },
        ]);
        let (_mock_guard, uplink_config) = mock_pq_uplink(&manifest).await;

        let pq_layer = PersistedQueryLayer::new(
            &Configuration::fake_builder()
                .persisted_query(PersistedQueries::builder().enabled(true).build())
                .uplink(uplink_config)
                .build()
                .unwrap(),
        )
        .await
        .unwrap();

        let map_to_query = |operation_id: &str, client_name: Option<&str>| -> Option<String> {
            let context = Context::new();
            if let Some(client_name) = client_name {
                context
                    .insert(
                        PERSISTED_QUERIES_CLIENT_NAME_CONTEXT_KEY,
                        client_name.to_string(),
                    )
                    .unwrap();
            }

            let incoming_request = SupergraphRequest::fake_builder()
                .extension(
                    "persistedQuery",
                    json!({"version": 1, "sha256Hash": operation_id.to_string()}),
                )
                .context(context)
                .build()
                .unwrap();

            pq_layer
                .supergraph_request(incoming_request)
                .expect("pq layer returned response instead of putting the query on the request")
                .supergraph_request
                .body()
                .query
                .clone()
        };

        assert_eq!(
            map_to_query("both-plain-and-cliented", None),
            Some("query { bpac_no_client: __typename }".to_string())
        );
        assert_eq!(
            map_to_query("both-plain-and-cliented", Some("not-web")),
            Some("query { bpac_no_client: __typename }".to_string())
        );
        assert_eq!(
            map_to_query("both-plain-and-cliented", Some("web")),
            Some("query { bpac_web_client: __typename }".to_string())
        );
        assert_eq!(
            map_to_query("only-cliented", Some("web")),
            Some("query { oc_web_client: __typename }".to_string())
        );
        assert_eq!(map_to_query("only-cliented", None), None);
        assert_eq!(map_to_query("only-cliented", Some("not-web")), None);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn enabled_pq_layer_operation_id_from_context() {
        let manifest = PersistedQueryManifest::from(vec![
            ManifestOperation {
                id: "id-from-context".to_string(),
                body: "query { id_from_context: __typename }".to_string(),
                client_name: None,
            },
            ManifestOperation {
                id: "ignored-id-in-body".to_string(),
                body: "query { ignored_id_in_body: __typename }".to_string(),
                client_name: None,
            },
        ]);
        let (_mock_guard, uplink_config) = mock_pq_uplink(&manifest).await;

        let pq_layer = PersistedQueryLayer::new(
            &Configuration::fake_builder()
                .persisted_query(PersistedQueries::builder().enabled(true).build())
                .uplink(uplink_config)
                .build()
                .unwrap(),
        )
        .await
        .unwrap();

        let context = Context::new();
        context
            .insert(
                PERSISTED_QUERIES_OPERATION_ID_CONTEXT_KEY,
                "id-from-context".to_string(),
            )
            .unwrap();
        let incoming_request = SupergraphRequest::fake_builder()
            .extension(
                "persistedQuery",
                json!({"version": 1, "sha256Hash": "ignored-id-in-body".to_string()}),
            )
            .context(context)
            .build()
            .unwrap();

        let mapped_query = pq_layer
            .supergraph_request(incoming_request)
            .expect("pq layer returned response instead of putting the query on the request")
            .supergraph_request
            .body()
            .query
            .clone();

        assert_eq!(
            mapped_query,
            Some("query { id_from_context: __typename }".to_string())
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn pq_layer_passes_on_to_apq_layer_when_id_not_found() {
        let (_id, _body, manifest) = fake_manifest();

        let (_mock_guard, uplink_config) = mock_pq_uplink(&manifest).await;

        let pq_layer = PersistedQueryLayer::new(
            &Configuration::fake_builder()
                .persisted_query(PersistedQueries::builder().enabled(true).build())
                .apq(Apq::fake_builder().enabled(true).build())
                .uplink(uplink_config)
                .build()
                .unwrap(),
        )
        .await
        .unwrap();
        let incoming_request = SupergraphRequest::fake_builder()
            .extension(
                "persistedQuery",
                json!({"version": 1, "sha256Hash": "this-id-is-invalid"}),
            )
            .build()
            .unwrap();

        assert!(incoming_request.supergraph_request.body().query.is_none());

        let result = pq_layer.supergraph_request(incoming_request);
        let request =
            result.expect("pq layer returned response instead of continuing to APQ layer");
        assert!(request.supergraph_request.body().query.is_none());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn pq_layer_errors_when_id_not_found_and_apq_disabled() {
        let (_id, _body, manifest) = fake_manifest();

        let (_mock_guard, uplink_config) = mock_pq_uplink(&manifest).await;

        let pq_layer = PersistedQueryLayer::new(
            &Configuration::fake_builder()
                .persisted_query(PersistedQueries::builder().enabled(true).build())
                .apq(Apq::fake_builder().enabled(false).build())
                .uplink(uplink_config)
                .build()
                .unwrap(),
        )
        .await
        .unwrap();
        let invalid_id = "this-id-is-invalid";
        let incoming_request = SupergraphRequest::fake_builder()
            .extension(
                "persistedQuery",
                json!({"version": 1, "sha256Hash": invalid_id}),
            )
            .build()
            .unwrap();

        assert!(incoming_request.supergraph_request.body().query.is_none());

        let mut supergraph_response = pq_layer
            .supergraph_request(incoming_request)
            .expect_err("pq layer returned request instead of returning an error response");
        assert_eq!(supergraph_response.response.status(), 404);
        let response = supergraph_response
            .next_response()
            .await
            .expect("could not get response from pq layer");
        assert_errors_eq_ignoring_id!(
            response.errors,
            [graphql_err_operation_not_found(invalid_id, None)]
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn enabled_apq_configuration_tracked_in_pq_layer() {
        let (_mock_guard, uplink_config) = mock_empty_pq_uplink().await;
        let pq_layer = PersistedQueryLayer::new(
            &Configuration::fake_builder()
                .apq(Apq::fake_builder().enabled(true).build())
                .persisted_query(PersistedQueries::builder().enabled(true).build())
                .uplink(uplink_config)
                .build()
                .unwrap(),
        )
        .await
        .unwrap();
        assert!(
            pq_layer
                .manifest_poller
                .unwrap()
                .augmenting_apq_with_pre_registration_and_no_safelisting()
        )
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn disabled_apq_configuration_tracked_in_pq_layer() {
        let (_mock_guard, uplink_config) = mock_empty_pq_uplink().await;
        let pq_layer = PersistedQueryLayer::new(
            &Configuration::fake_builder()
                .apq(Apq::fake_builder().enabled(false).build())
                .uplink(uplink_config)
                .persisted_query(PersistedQueries::builder().enabled(true).build())
                .build()
                .unwrap(),
        )
        .await
        .unwrap();
        assert!(
            !pq_layer
                .manifest_poller
                .unwrap()
                .augmenting_apq_with_pre_registration_and_no_safelisting()
        )
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn enabled_safelist_configuration_tracked_in_pq_layer() {
        let safelist_config = PersistedQueriesSafelist::builder().enabled(true).build();
        let (_mock_guard, uplink_config) = mock_empty_pq_uplink().await;
        let pq_layer = PersistedQueryLayer::new(
            &Configuration::fake_builder()
                .persisted_query(
                    PersistedQueries::builder()
                        .enabled(true)
                        .safelist(safelist_config)
                        .build(),
                )
                .uplink(uplink_config)
                .apq(Apq::fake_builder().enabled(false).build())
                .build()
                .unwrap(),
        )
        .await
        .unwrap();
        assert!(matches!(
            pq_layer
                .manifest_poller
                .unwrap()
                .state
                .read()
                .freeform_graphql_behavior,
            FreeformGraphQLBehavior::AllowIfInSafelist { .. }
        ))
    }

    async fn run_first_two_layers(
        pq_layer: &PersistedQueryLayer,
        query_analysis_layer: &QueryAnalysisLayer,
        body: &str,
        skip_enforcement: bool,
    ) -> SupergraphRequest {
        let context = Context::new();
        if skip_enforcement {
            context
                .insert(
                    PERSISTED_QUERIES_SAFELIST_SKIP_ENFORCEMENT_CONTEXT_KEY,
                    true,
                )
                .unwrap();
        }

        let incoming_request = SupergraphRequest::fake_builder()
            .query(body)
            .context(context)
            .build()
            .unwrap();

        assert!(incoming_request.supergraph_request.body().query.is_some());

        // The initial hook won't block us --- that waits until after we've parsed
        // the operation.
        let updated_request = pq_layer
            .supergraph_request(incoming_request)
            .expect("pq layer returned error response instead of returning a request");
        query_analysis_layer
            .supergraph_request(updated_request)
            .await
            .expect("QA layer returned error response instead of returning a request")
    }

    async fn denied_by_safelist(
        pq_layer: &PersistedQueryLayer,
        query_analysis_layer: &QueryAnalysisLayer,
        body: &str,
        log_unknown: bool,
        counter_value: u64,
    ) {
        let request_with_analyzed_query =
            run_first_two_layers(pq_layer, query_analysis_layer, body, false).await;

        let mut supergraph_response = pq_layer
            .supergraph_request_with_analyzed_query(request_with_analyzed_query)
            .await
            .expect_err(
                "pq layer second hook returned request instead of returning an error response",
            );
        assert_eq!(supergraph_response.response.status(), 403);
        let response = supergraph_response
            .next_response()
            .await
            .expect("could not get response from pq layer");
        assert_errors_eq_ignoring_id!(response.errors, [graphql_err_operation_not_in_safelist()]);
        let mut metric_attributes = vec![opentelemetry::KeyValue::new(
            "persisted_queries.safelist.rejected.unknown".to_string(),
            true,
        )];
        if log_unknown {
            metric_attributes.push(opentelemetry::KeyValue::new(
                "persisted_queries.logged".to_string(),
                true,
            ));
        }
        assert_counter!(
            "apollo.router.operations.persisted_queries",
            counter_value,
            &metric_attributes
        );
    }

    async fn allowed_by_safelist(
        pq_layer: &PersistedQueryLayer,
        query_analysis_layer: &QueryAnalysisLayer,
        body: &str,
        log_unknown: bool,
        skip_enforcement: bool,
        counter_value: u64,
    ) {
        let request_with_analyzed_query =
            run_first_two_layers(pq_layer, query_analysis_layer, body, skip_enforcement).await;

        pq_layer
            .supergraph_request_with_analyzed_query(request_with_analyzed_query)
            .await
            .expect("pq layer second hook returned error response instead of returning a request");

        let mut metric_attributes = vec![];
        if skip_enforcement {
            metric_attributes.push(opentelemetry::KeyValue::new(
                "persisted_queries.safelist.enforcement_skipped".to_string(),
                true,
            ));
            if log_unknown {
                metric_attributes.push(opentelemetry::KeyValue::new(
                    "persisted_queries.logged".to_string(),
                    true,
                ));
            }
        }

        assert_counter!(
            "apollo.router.operations.persisted_queries",
            counter_value,
            &metric_attributes
        );
    }

    async fn pq_layer_freeform_graphql_with_safelist(log_unknown: bool) {
        async move {
            let manifest = PersistedQueryManifest::from(vec![
                ManifestOperation {
                    id: "valid-syntax".to_string(),
                    body: "fragment A on Query { me { id } }    query SomeOp { ...A ...B }    fragment,,, B on Query{me{name,username}  } # yeah".to_string(),
                    client_name: None,
                },
                ManifestOperation {
                    id: "invalid-syntax".to_string(),
                    body: "}}}".to_string(),
                    client_name: None,
                },
            ]);

            let (_mock_guard, uplink_config) = mock_pq_uplink(&manifest).await;

            let config = Configuration::fake_builder()
                .persisted_query(
                    PersistedQueries::builder()
                        .enabled(true)
                        .safelist(PersistedQueriesSafelist::builder().enabled(true).build())
                        .log_unknown(log_unknown)
                        .build(),
                )
                .uplink(uplink_config)
                .apq(Apq::fake_builder().enabled(false).build())
                .supergraph(Supergraph::fake_builder().introspection(true).build())
                .build()
                .unwrap();

            let pq_layer = PersistedQueryLayer::new(&config).await.unwrap();

            let schema = Arc::new(Schema::parse(include_str!("../../../testdata/supergraph.graphql"), &Default::default()).unwrap());

            let query_analysis_layer = QueryAnalysisLayer::new(schema, Arc::new(config)).await;

            // A random query is blocked.
            denied_by_safelist(
                &pq_layer,
                &query_analysis_layer,
                "query SomeQuery { me { id } }",
                log_unknown,
                1,
            ).await;

            // But it is allowed with skip_enforcement set.
            allowed_by_safelist(
                &pq_layer,
                &query_analysis_layer,
                "query SomeQuery { me { id } }",
                log_unknown,
                true,
                1,
            ).await;

            // The exact string from the manifest is allowed.
            allowed_by_safelist(
                &pq_layer,
                &query_analysis_layer,
                "fragment A on Query { me { id } }    query SomeOp { ...A ...B }    fragment,,, B on Query{me{name,username}  } # yeah",
                log_unknown,
                false,
                1,
            )
            .await;

            // Reordering definitions and reformatting a bit matches.
            allowed_by_safelist(
                &pq_layer,
                &query_analysis_layer,
                "#comment\n  fragment, B on Query  , { me{name    username} }    query SomeOp {  ...A ...B }  fragment    \nA on Query { me{ id} }",
                log_unknown,
                false,
                2,
            )
            .await;

            // Reordering fields does not match!
            denied_by_safelist(
                &pq_layer,
                &query_analysis_layer,
                "fragment A on Query { me { id } }    query SomeOp { ...A ...B }    fragment,,, B on Query{me{username,name}  } # yeah",
                log_unknown,
                2,
            )
            .await;

            // Introspection queries are allowed (even using fragments and aliases), because
            // introspection is enabled.
            allowed_by_safelist(
                &pq_layer,
                &query_analysis_layer,
                r#"fragment F on Query { __typename foo: __schema { __typename } } query Q { __type(name: "foo") { name } ...F }"#,
                log_unknown,
                false,
                // Note that introspection queries don't actually interact with the PQ machinery enough
                // to update this metric, for better or for worse.
                2,
            )
            .await;

            // Multiple spreads of the same fragment are also allowed
            // (https://github.com/apollographql/apollo-rs/issues/613)
            allowed_by_safelist(
                &pq_layer,
                &query_analysis_layer,
                r#"fragment F on Query { __typename foo: __schema { __typename } } query Q { __type(name: "foo") { name } ...F ...F }"#,
                log_unknown,
                false,
                // Note that introspection queries don't actually interact with the PQ machinery enough
                // to update this metric, for better or for worse.
                2,
            )
            .await;

            // But adding any top-level non-introspection field is enough to make it not count as introspection.
            denied_by_safelist(
                &pq_layer,
                &query_analysis_layer,
                r#"fragment F on Query { __typename foo: __schema { __typename } me { id } } query Q { __type(name: "foo") { name } ...F }"#,
                log_unknown,
                3,
            )
            .await;
        }
        .with_metrics()
        .await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn pq_layer_freeform_graphql_with_safelist_log_unknown_false() {
        pq_layer_freeform_graphql_with_safelist(false).await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn pq_layer_freeform_graphql_with_safelist_log_unknown_true() {
        async {
            pq_layer_freeform_graphql_with_safelist(true).await;
        }
        .with_subscriber(assert_snapshot_subscriber!())
        .await
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn pq_layer_rejects_invalid_ids_with_safelisting_enabled() {
        let (_id, _body, manifest) = fake_manifest();

        let (_mock_guard, uplink_config) = mock_pq_uplink(&manifest).await;

        let safelist_config = PersistedQueriesSafelist::builder().enabled(true).build();
        let pq_layer = PersistedQueryLayer::new(
            &Configuration::fake_builder()
                .persisted_query(
                    PersistedQueries::builder()
                        .enabled(true)
                        .safelist(safelist_config)
                        .build(),
                )
                .uplink(uplink_config)
                .apq(Apq::fake_builder().enabled(false).build())
                .build()
                .unwrap(),
        )
        .await
        .unwrap();
        let invalid_id = "this-id-is-invalid";
        let incoming_request = SupergraphRequest::fake_builder()
            .extension(
                "persistedQuery",
                json!({"version": 1, "sha256Hash": invalid_id}),
            )
            .operation_name("SomeOperation")
            .build()
            .unwrap();

        assert!(incoming_request.supergraph_request.body().query.is_none());

        let result = pq_layer.supergraph_request(incoming_request);
        let response = result
            .expect_err("pq layer returned request instead of returning an error response")
            .next_response()
            .await
            .expect("could not get response from pq layer");
        assert_errors_eq_ignoring_id!(
            response.errors,
            [graphql_err_operation_not_found(
                invalid_id,
                Some("SomeOperation".to_string()),
            )]
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn apq_and_pq_safelisting_is_invalid_config() {
        let (_mock_guard, uplink_config) = mock_empty_pq_uplink().await;
        let safelist_config = PersistedQueriesSafelist::builder().enabled(true).build();
        assert!(
            Configuration::fake_builder()
                .persisted_query(
                    PersistedQueries::builder()
                        .enabled(true)
                        .safelist(safelist_config)
                        .build(),
                )
                .apq(Apq::fake_builder().enabled(true).build())
                .uplink(uplink_config)
                .build()
                .is_err()
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn require_id_disabled_by_default_with_safelisting_enabled_in_pq_layer() {
        let safelist_config = PersistedQueriesSafelist::builder().enabled(true).build();
        let (_mock_guard, uplink_config) = mock_empty_pq_uplink().await;
        let pq_layer = PersistedQueryLayer::new(
            &Configuration::fake_builder()
                .persisted_query(
                    PersistedQueries::builder()
                        .enabled(true)
                        .safelist(safelist_config)
                        .build(),
                )
                .apq(Apq::fake_builder().enabled(false).build())
                .uplink(uplink_config)
                .build()
                .unwrap(),
        )
        .await
        .unwrap();
        assert!(matches!(
            pq_layer
                .manifest_poller
                .unwrap()
                .state
                .read()
                .freeform_graphql_behavior,
            FreeformGraphQLBehavior::AllowIfInSafelist { .. }
        ))
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn safelisting_require_id_can_be_enabled_in_pq_layer() {
        let safelist_config = PersistedQueriesSafelist::builder()
            .enabled(true)
            .require_id(true)
            .build();
        let (_mock_guard, uplink_config) = mock_empty_pq_uplink().await;
        let pq_layer = PersistedQueryLayer::new(
            &Configuration::fake_builder()
                .persisted_query(
                    PersistedQueries::builder()
                        .enabled(true)
                        .safelist(safelist_config)
                        .build(),
                )
                .apq(Apq::fake_builder().enabled(false).build())
                .uplink(uplink_config)
                .build()
                .unwrap(),
        )
        .await
        .unwrap();
        assert!(
            pq_layer
                .manifest_poller
                .unwrap()
                .never_allows_freeform_graphql()
                .is_some()
        )
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn safelisting_require_id_rejects_freeform_graphql_in_pq_layer() {
        let safelist_config = PersistedQueriesSafelist::builder()
            .enabled(true)
            .require_id(true)
            .build();
        let (_mock_guard, uplink_config) = mock_empty_pq_uplink().await;
        let pq_layer = PersistedQueryLayer::new(
            &Configuration::fake_builder()
                .persisted_query(
                    PersistedQueries::builder()
                        .enabled(true)
                        .safelist(safelist_config)
                        .build(),
                )
                .apq(Apq::fake_builder().enabled(false).build())
                .uplink(uplink_config)
                .build()
                .unwrap(),
        )
        .await
        .unwrap();

        let incoming_request = SupergraphRequest::fake_builder()
            .query("query { typename }")
            .build()
            .unwrap();

        assert!(incoming_request.supergraph_request.body().query.is_some());

        let mut supergraph_response = pq_layer
            .supergraph_request(incoming_request)
            .expect_err("pq layer returned request instead of returning an error response");
        assert_eq!(supergraph_response.response.status(), 400);
        let response = supergraph_response
            .next_response()
            .await
            .expect("could not get response from pq layer");
        assert_errors_eq_ignoring_id!(response.errors, [graphql_err_pq_id_required()]);

        // Try again skipping enforcement.
        let context = Context::new();
        context
            .insert(
                PERSISTED_QUERIES_SAFELIST_SKIP_ENFORCEMENT_CONTEXT_KEY,
                true,
            )
            .unwrap();
        let incoming_request = SupergraphRequest::fake_builder()
            .query("query { typename }")
            .context(context)
            .build()
            .unwrap();
        assert!(incoming_request.supergraph_request.body().query.is_some());
        assert!(pq_layer.supergraph_request(incoming_request).is_ok());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn safelisting_disabled_by_default_in_pq_layer() {
        let (_mock_guard, uplink_config) = mock_empty_pq_uplink().await;
        let pq_layer = PersistedQueryLayer::new(
            &Configuration::fake_builder()
                .persisted_query(PersistedQueries::builder().enabled(true).build())
                .apq(Apq::fake_builder().enabled(false).build())
                .uplink(uplink_config)
                .build()
                .unwrap(),
        )
        .await
        .unwrap();
        assert!(matches!(
            pq_layer
                .manifest_poller
                .unwrap()
                .state
                .read()
                .freeform_graphql_behavior,
            FreeformGraphQLBehavior::AllowAll { apq_enabled: false }
        ))
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn disabled_safelist_configuration_tracked_in_pq_layer() {
        let (_mock_guard, uplink_config) = mock_empty_pq_uplink().await;
        let safelist_config = PersistedQueriesSafelist::builder().enabled(false).build();
        let pq_layer = PersistedQueryLayer::new(
            &Configuration::fake_builder()
                .persisted_query(
                    PersistedQueries::builder()
                        .enabled(true)
                        .safelist(safelist_config)
                        .build(),
                )
                .uplink(uplink_config)
                .build()
                .unwrap(),
        )
        .await
        .unwrap();
        assert!(matches!(
            pq_layer
                .manifest_poller
                .unwrap()
                .state
                .read()
                .freeform_graphql_behavior,
            FreeformGraphQLBehavior::AllowAll { apq_enabled: true }
        ))
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn can_pass_different_body_from_published_pq_id_with_apq_enabled() {
        let (id, _body, manifest) = fake_manifest();
        let (_mock_guard, uplink_config) = mock_pq_uplink(&manifest).await;
        let pq_layer = PersistedQueryLayer::new(
            &Configuration::fake_builder()
                .persisted_query(PersistedQueries::builder().enabled(true).build())
                .apq(Apq::fake_builder().enabled(true).build())
                .uplink(uplink_config)
                .build()
                .unwrap(),
        )
        .await
        .unwrap();
        let incoming_request = SupergraphRequest::fake_builder()
            .extension("persistedQuery", json!({"version": 1, "sha256Hash": id}))
            .query("invalid body")
            .build()
            .unwrap();

        assert!(incoming_request.supergraph_request.body().query.is_some());

        let result = pq_layer.supergraph_request(incoming_request);
        assert!(result.is_ok())
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn cannot_pass_different_body_as_published_pq_id_with_apq_disabled() {
        let (id, _body, manifest) = fake_manifest();
        let (_mock_guard, uplink_config) = mock_pq_uplink(&manifest).await;
        let pq_layer = PersistedQueryLayer::new(
            &Configuration::fake_builder()
                .persisted_query(PersistedQueries::builder().enabled(true).build())
                .apq(Apq::fake_builder().enabled(false).build())
                .uplink(uplink_config)
                .build()
                .unwrap(),
        )
        .await
        .unwrap();
        let incoming_request = SupergraphRequest::fake_builder()
            .extension("persistedQuery", json!({"version": 1, "sha256Hash": id}))
            .query("invalid body")
            .build()
            .unwrap();

        assert!(incoming_request.supergraph_request.body().query.is_some());

        let mut supergraph_response = pq_layer
            .supergraph_request(incoming_request)
            .expect_err("pq layer returned request instead of returning an error response");
        assert_eq!(supergraph_response.response.status(), 400);
        let response = supergraph_response
            .next_response()
            .await
            .expect("could not get response from pq layer");
        assert_errors_eq_ignoring_id!(response.errors, [graphql_err_cannot_send_id_and_body()]);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn cannot_pass_same_body_as_published_pq_id_with_apq_disabled() {
        let (id, body, manifest) = fake_manifest();
        let (_mock_guard, uplink_config) = mock_pq_uplink(&manifest).await;
        let pq_layer = PersistedQueryLayer::new(
            &Configuration::fake_builder()
                .persisted_query(PersistedQueries::builder().enabled(true).build())
                .apq(Apq::fake_builder().enabled(false).build())
                .uplink(uplink_config)
                .build()
                .unwrap(),
        )
        .await
        .unwrap();
        let incoming_request = SupergraphRequest::fake_builder()
            .extension("persistedQuery", json!({"version": 1, "sha256Hash": id}))
            .query(body)
            .build()
            .unwrap();

        assert!(incoming_request.supergraph_request.body().query.is_some());

        let mut supergraph_response = pq_layer
            .supergraph_request(incoming_request)
            .expect_err("pq layer returned request instead of returning an error response");
        assert_eq!(supergraph_response.response.status(), 400);
        let response = supergraph_response
            .next_response()
            .await
            .expect("could not get response from pq layer");
        assert_errors_eq_ignoring_id!(response.errors, [graphql_err_cannot_send_id_and_body()]);
    }
}
