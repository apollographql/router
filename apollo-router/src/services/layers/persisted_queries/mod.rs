use std::sync::Arc;

mod id_extractor;
mod manifest_poller;

use anyhow::anyhow;
use http::header::CACHE_CONTROL;
use http::HeaderValue;
use id_extractor::PersistedQueryIdExtractor;
pub(crate) use manifest_poller::PersistedQueryManifestPoller;
use tower::BoxError;

use crate::configuration::PersistedQueriesSafelist;
use crate::graphql::Error as GraphQLError;
use crate::plugins::telemetry::utils::TracingUtils;
use crate::services::SupergraphRequest;
use crate::services::SupergraphResponse;
use crate::Configuration;
use crate::UplinkConfig;

const SANDBOX_INTROSPECTION_QUERY_WO_DEPRECATED_ARGS: &str = "\n    query IntrospectionQuery {\n      __schema {\n        \n        queryType { name }\n        mutationType { name }\n        subscriptionType { name }\n        types {\n          ...FullType\n        }\n        directives {\n          name\n          description\n          \n          locations\n          args {\n            ...InputValue\n          }\n        }\n      }\n    }\n\n    fragment FullType on __Type {\n      kind\n      name\n      description\n      \n      fields(includeDeprecated: true) {\n        name\n        description\n        args {\n          ...InputValue\n        }\n        type {\n          ...TypeRef\n        }\n        isDeprecated\n        deprecationReason\n      }\n      inputFields {\n        ...InputValue\n      }\n      interfaces {\n        ...TypeRef\n      }\n      enumValues(includeDeprecated: true) {\n        name\n        description\n        isDeprecated\n        deprecationReason\n      }\n      possibleTypes {\n        ...TypeRef\n      }\n    }\n\n    fragment InputValue on __InputValue {\n      name\n      description\n      type { ...TypeRef }\n      defaultValue\n      \n      \n    }\n\n    fragment TypeRef on __Type {\n      kind\n      name\n      ofType {\n        kind\n        name\n        ofType {\n          kind\n          name\n          ofType {\n            kind\n            name\n            ofType {\n              kind\n              name\n              ofType {\n                kind\n                name\n                ofType {\n                  kind\n                  name\n                  ofType {\n                    kind\n                    name\n                  }\n                }\n              }\n            }\n          }\n        }\n      }\n    }\n  ";
const SANDBOX_INTROSPECTION_QUERY_W_DEPRECATED_ARGS: &str = "\n    query IntrospectionQuery {\n      __schema {\n        \n        queryType { name }\n        mutationType { name }\n        subscriptionType { name }\n        types {\n          ...FullType\n        }\n        directives {\n          name\n          description\n          \n          locations\n          args(includeDeprecated: true) {\n            ...InputValue\n          }\n        }\n      }\n    }\n\n    fragment FullType on __Type {\n      kind\n      name\n      description\n      \n      fields(includeDeprecated: true) {\n        name\n        description\n        args(includeDeprecated: true) {\n          ...InputValue\n        }\n        type {\n          ...TypeRef\n        }\n        isDeprecated\n        deprecationReason\n      }\n      inputFields(includeDeprecated: true) {\n        ...InputValue\n      }\n      interfaces {\n        ...TypeRef\n      }\n      enumValues(includeDeprecated: true) {\n        name\n        description\n        isDeprecated\n        deprecationReason\n      }\n      possibleTypes {\n        ...TypeRef\n      }\n    }\n\n    fragment InputValue on __InputValue {\n      name\n      description\n      type { ...TypeRef }\n      defaultValue\n      isDeprecated\n      deprecationReason\n    }\n\n    fragment TypeRef on __Type {\n      kind\n      name\n      ofType {\n        kind\n        name\n        ofType {\n          kind\n          name\n          ofType {\n            kind\n            name\n            ofType {\n              kind\n              name\n              ofType {\n                kind\n                name\n                ofType {\n                  kind\n                  name\n                  ofType {\n                    kind\n                    name\n                  }\n                }\n              }\n            }\n          }\n        }\n      }\n    }\n  ";
const DONT_CACHE_RESPONSE_VALUE: &str = "private, no-cache, must-revalidate";

#[derive(Debug)]
pub(crate) struct PersistedQueryLayer {
    /// Manages polling uplink for persisted queries
    /// it maintains its state between schema reloads and continues running.
    pub(crate) manifest_poller: Option<Arc<PersistedQueryManifestPoller>>,

    /// Tracks whether APQ is also enabled.
    /// If it is, this layer won't reject operations it can't find in the manifest,
    /// instead passing on execution to the APQ layer, which will return an error
    /// if it can _also_ not find the operation.
    apq_enabled: bool,

    /// Tracks whether Sandbox is also enabled.
    /// If it is, this layer won't reject introspection operations.
    /// TODO: remove this in favor of proper introspection parsing.
    sandbox_enabled: bool,

    /// Tracks whether to log incoming queries that are not in the persisted query list.
    log_unknown: bool,

    /// Safelisting configuration.
    safelist_config: PersistedQueriesSafelist,
}

impl PersistedQueryLayer {
    /// Create a new [`PersistedQueryLayer`] from CLI options, YAML configuration,
    /// and optionally, an existing persisted query manifest poller.
    pub(crate) async fn new(
        configuration: &Configuration,
        previous_manifest_poller: Option<Arc<PersistedQueryManifestPoller>>,
    ) -> Result<Self, BoxError> {
        if configuration.preview_persisted_queries.enabled {
            if let Some(uplink_config) = configuration.uplink.as_ref() {
                if configuration.apq.enabled
                    && configuration.preview_persisted_queries.safelist.enabled
                {
                    return Err(anyhow!("invalid configuration: preview_persisted_queries.safelist.enabled = true, which is incompatible with apq.enabled = true. you must disable apq in your configuration to enable persisted queries with safelisting").into());
                }
                Self::new_enabled(configuration, uplink_config, previous_manifest_poller).await
            } else {
                Err(anyhow!("persisted queries requires Apollo GraphOS. ensure that you have set APOLLO_KEY and APOLLO_GRAPH_REF environment variables").into())
            }
        } else {
            Self::new_disabled(configuration, previous_manifest_poller).await
        }
    }

    /// Create a new enabled [`PersistedQueryLayer`] using the existing manifest poller if it exists,
    /// keeping state intact during state machine reloads
    /// or starting a new poller from CLI options and YAML configuration.
    async fn new_enabled(
        configuration: &Configuration,
        uplink_config: &UplinkConfig,
        preexisting_manifest_poller: Option<Arc<PersistedQueryManifestPoller>>,
    ) -> Result<Self, BoxError> {
        Self::new_with_manifest_poller(
            configuration,
            Some(
                // use the existing manifest poller if it already exists so chunks don't need refetching
                // no configuration options could have changed for the manifest poller because uplink
                // configuration options come from CLI options, not YAML, so it's safe to re-use.
                if let Some(previous_manifest_poller) = preexisting_manifest_poller.clone() {
                    previous_manifest_poller
                } else {
                    Arc::new(PersistedQueryManifestPoller::new(uplink_config).await?)
                },
            ),
        )
    }

    /// Create a new disabled [`PersistedQueryLayer`] shutting down the existing manifest poller if it exists.
    async fn new_disabled(
        configuration: &Configuration,
        preexisting_manifest_poller: Option<Arc<PersistedQueryManifestPoller>>,
    ) -> Result<Self, BoxError> {
        if let Some(preexisting_manifest_poller) = preexisting_manifest_poller {
            preexisting_manifest_poller.shutdown().await?;
        }

        Self::new_with_manifest_poller(configuration, None)
    }

    fn new_with_manifest_poller(
        configuration: &Configuration,
        manifest_poller: Option<Arc<PersistedQueryManifestPoller>>,
    ) -> Result<Self, BoxError> {
        Ok(Self {
            manifest_poller,
            apq_enabled: configuration.apq.enabled,
            sandbox_enabled: configuration.sandbox.enabled,
            safelist_config: configuration.preview_persisted_queries.safelist.clone(),
            log_unknown: configuration.preview_persisted_queries.log_unknown,
        })
    }

    /// Run a request through the layer.
    /// Takes care of:
    /// 1) resolving a persisted query ID to a query body
    /// 2) matching a freeform GraphQL request against persisted queries, optionally rejecting it based on configuration
    /// 3) continuing to the next stage of the router
    pub(crate) fn supergraph_request(
        &self,
        request: SupergraphRequest,
    ) -> Result<SupergraphRequest, SupergraphResponse> {
        if let Some(manifest_poller) = &self.manifest_poller {
            if let Some(persisted_query_id) = PersistedQueryIdExtractor::extract_id(&request) {
                self.replace_query_id_with_operation_body(
                    request,
                    manifest_poller.clone(),
                    &persisted_query_id,
                )
            } else {
                self.handle_freeform_graphql(request, manifest_poller.clone())
            }
        } else {
            Ok(request)
        }
    }

    /// Places an operation body on a [`SupergraphRequest`] if it has been persisted
    pub(crate) fn replace_query_id_with_operation_body(
        &self,
        mut request: SupergraphRequest,
        manifest_poller: Arc<PersistedQueryManifestPoller>,
        persisted_query_id: &str,
    ) -> Result<SupergraphRequest, SupergraphResponse> {
        if request.supergraph_request.body().query.is_some() {
            if self.apq_enabled {
                // if the request has a query and an ID, and APQ is enabled, continue with normal execution.
                // safelisting and APQ are incomaptible with each other - therefore we don't need to check
                // if the ID in the requests exactly maps to the body in the persisted query manifest,
                // we can just ignore the ID and let APQ handle it for us
                assert!(!self.safelist_config.enabled);
                Ok(request)
            } else {
                Err(supergraph_err_cannot_send_id_and_body_with_apq_disabled(
                    request,
                ))
            }
        } else {
            // if there is no query, look up the persisted query in the manifest
            // and put the body on the `supergraph_request`
            if let Some(persisted_query_body) =
                manifest_poller.get_operation_body(persisted_query_id)
            {
                let mut body = request.supergraph_request.body_mut();
                body.query = Some(persisted_query_body);
                body.extensions.remove("persistedQuery");
                tracing::info!(monotonic_counter.apollo.router.operations.persisted_queries = 1u64);
                Ok(request)
            } else if self.apq_enabled {
                // if APQ is also enabled, pass the request along to the APQ plugin
                // where it will do its own lookup
                Ok(request)
            } else {
                tracing::info!(
                    monotonic_counter.apollo.router.operations.persisted_queries = 1u64,
                    persisted_quieries.not_found = true
                );
                // if APQ is not enabled, return an error indicating the query was not found
                Err(supergraph_err_operation_not_found(
                    request,
                    persisted_query_id,
                ))
            }
        }
    }

    /// Handles incoming freeform GraphQL requests according to the safelisting configuration options
    pub(crate) fn handle_freeform_graphql(
        &self,
        request: SupergraphRequest,
        manifest_poller: Arc<PersistedQueryManifestPoller>,
    ) -> Result<SupergraphRequest, SupergraphResponse> {
        if let Some(operation_body) = request.supergraph_request.body().query.as_ref() {
            // TODO: replace this with proper introspection parsing
            if self.sandbox_enabled
                && (operation_body == SANDBOX_INTROSPECTION_QUERY_WO_DEPRECATED_ARGS
                    || operation_body == SANDBOX_INTROSPECTION_QUERY_W_DEPRECATED_ARGS)
            {
                // if sandbox is enabled and the incoming operation is the introspection query sent by sandbox,
                // allow the request to continue on.
                return Ok(request);
            }

            let mut is_persisted = None;

            let known = is_operation_persisted(&mut is_persisted, manifest_poller, operation_body);
            let logged = self.log_unknown && !known;
            if logged {
                tracing::warn!(message = "unknown operation", operation_body);
            }

            if self.safelist_config.enabled {
                if self.safelist_config.require_id {
                    tracing::info!(
                        monotonic_counter.apollo.router.operations.persisted_queries = 1u64,
                        persisted_queries.safelist.rejected.missing_id = true,
                        persisted_queries.logged = logged.or_empty()
                    );
                    Err(supergraph_err_pq_id_required(request))
                } else if known {
                    tracing::info!(
                        monotonic_counter.apollo.router.operations.persisted_queries = 1u64,
                    );
                    // if the freeform GraphQL body we received was found in the manifest,
                    // allow the request to continue execution
                    Ok(request)
                } else {
                    tracing::info!(
                        monotonic_counter.apollo.router.operations.persisted_queries = 1u64,
                        persisted_queries.safelist.rejected.unknown = true,
                        persisted_queries.logged = logged.or_empty()
                    );
                    Err(supergraph_err_operation_not_in_safelist(request))
                }
            } else {
                // if the request already has a query, continue with normal execution
                // because there is no need to substitute the body
                // and freeform GraphQL is always allowed if safelisting is not enabled
                Ok(request)
            }
        } else {
            // if the request doesn't have a query, continue with normal execution
            // if APQ is enabled, it will handle this request, otherwise this request
            // is likely to eventually result in an error because there is no query specified
            Ok(request)
        }
    }
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
    ) -> SupergraphResponse {
        let mut error_builder = SupergraphResponse::error_builder()
            .error(graphql_error)
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

/// checks if the query body is persisted, storing the result in a local cache
/// can be called multiple times and the full map lookup will only occur once
fn is_operation_persisted(
    is_persisted: &mut Option<bool>,
    manifest_poller: Arc<PersistedQueryManifestPoller>,
    operation_body: &str,
) -> bool {
    if let Some(result) = is_persisted {
        *result
    } else {
        let result = manifest_poller.is_operation_persisted(operation_body);
        *is_persisted = Some(result);
        result
    }
}

fn graphql_err_operation_not_found(persisted_query_id: &str) -> GraphQLError {
    graphql_err(
        "PERSISTED_QUERY_NOT_IN_LIST",
        &format!("Persisted query '{persisted_query_id}' not found in the persisted query list"),
    )
}

fn supergraph_err_operation_not_found(
    request: SupergraphRequest,
    persisted_query_id: &str,
) -> SupergraphResponse {
    supergraph_err(
        graphql_err_operation_not_found(persisted_query_id),
        request,
        ErrorCacheStrategy::DontCache,
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
    )
}

fn graphql_err_pq_id_required() -> GraphQLError {
    graphql_err("PERSISTED_QUERY_ID_REQUIRED",
        "This endpoint does not allow freeform GraphQL requests; operations must be sent by ID in the persisted queries GraphQL extension.",
     )
}

fn supergraph_err_pq_id_required(request: SupergraphRequest) -> SupergraphResponse {
    supergraph_err(
        graphql_err_pq_id_required(),
        request,
        ErrorCacheStrategy::Cache,
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
) -> SupergraphResponse {
    cache_strategy.get_supergraph_response(graphql_error, request)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use serde_json::json;

    use super::*;
    use crate::configuration::Apq;
    use crate::configuration::PersistedQueries;
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
            None,
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
            None,
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

        assert!(PersistedQueryManifestPoller::new(&uplink_config)
            .await
            .is_ok());

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
            None,
        )
        .await
        .unwrap();
        let incoming_request = SupergraphRequest::fake_builder()
            .extension("persistedQuery", json!({"version": 1, "sha256Hash": id}))
            .build()
            .unwrap();

        assert!(incoming_request.supergraph_request.body().query.is_none());

        let result = pq_layer.supergraph_request(incoming_request);
        if let Ok(request) = result {
            assert_eq!(request.supergraph_request.body().query, Some(body));
        } else {
            panic!("pq layer returned response instead of putting the query on the request");
        }
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
            None,
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
        if let Ok(request) = result {
            assert!(request.supergraph_request.body().query.is_none());
        } else {
            panic!("pq layer returned response instead of continuing to APQ layer");
        }
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
            None,
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

        let result = pq_layer.supergraph_request(incoming_request);
        if let Err(mut response) = result {
            if let Some(response) = response.next_response().await {
                assert_eq!(
                    response.errors,
                    vec![graphql_err_operation_not_found(invalid_id)]
                );
            } else {
                panic!("could not get response from pq layer");
            }
        } else {
            panic!("pq layer returned request instead of returning an error response");
        }
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
            None,
        )
        .await
        .unwrap();
        assert!(pq_layer.apq_enabled)
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
            None,
        )
        .await
        .unwrap();
        assert!(!pq_layer.apq_enabled)
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
            None,
        )
        .await
        .unwrap();
        assert!(pq_layer.safelist_config.enabled)
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn pq_layer_allows_freeform_graphql_when_in_safelist() {
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
            None,
        )
        .await
        .unwrap();

        let incoming_request = SupergraphRequest::fake_builder()
            .query("query NamedQuery { typename }")
            .build()
            .unwrap();

        assert!(incoming_request.supergraph_request.body().query.is_some());

        let result = pq_layer.supergraph_request(incoming_request);
        if let Err(mut response) = result {
            if let Some(response) = response.next_response().await {
                assert_eq!(
                    response.errors,
                    vec![graphql_err_operation_not_in_safelist()]
                );
            } else {
                panic!("could not get response from pq layer");
            }
        } else {
            panic!("pq layer returned request instead of returning an error response");
        }
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
            None,
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

        let result = pq_layer.supergraph_request(incoming_request);
        if let Err(mut response) = result {
            if let Some(response) = response.next_response().await {
                assert_eq!(
                    response.errors,
                    vec![graphql_err_operation_not_found(invalid_id)]
                );
            } else {
                panic!("could not get response from pq layer");
            }
        } else {
            panic!("pq layer returned request instead of returning an error response");
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn apq_and_pq_safelisting_is_invalid_config() {
        let (_mock_guard, uplink_config) = mock_empty_pq_uplink().await;
        let safelist_config = PersistedQueriesSafelist::builder().enabled(true).build();
        let pq_layer_result = PersistedQueryLayer::new(
            &Configuration::fake_builder()
                .persisted_query(
                    PersistedQueries::builder()
                        .enabled(true)
                        .safelist(safelist_config)
                        .build(),
                )
                .apq(Apq::fake_builder().enabled(true).build())
                .uplink(uplink_config)
                .build()
                .unwrap(),
            None,
        )
        .await;
        assert!(pq_layer_result.is_err());
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
            None,
        )
        .await
        .unwrap();
        assert!(!pq_layer.safelist_config.require_id)
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
            None,
        )
        .await
        .unwrap();
        assert!(pq_layer.safelist_config.require_id)
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
            None,
        )
        .await
        .unwrap();

        let incoming_request = SupergraphRequest::fake_builder()
            .query("query { typename }")
            .build()
            .unwrap();

        assert!(incoming_request.supergraph_request.body().query.is_some());

        let result = pq_layer.supergraph_request(incoming_request);
        if let Err(mut response) = result {
            if let Some(response) = response.next_response().await {
                assert_eq!(response.errors, vec![graphql_err_pq_id_required()]);
            } else {
                panic!("could not get response from pq layer");
            }
        } else {
            panic!("pq layer returned request instead of returning an error response");
        }
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
            None,
        )
        .await
        .unwrap();
        assert!(!pq_layer.safelist_config.enabled)
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
            None,
        )
        .await
        .unwrap();
        assert!(!pq_layer.safelist_config.enabled)
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
            None,
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
            None,
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
        if let Err(mut response) = result {
            if let Some(response) = response.next_response().await {
                assert_eq!(response.errors, vec![graphql_err_cannot_send_id_and_body()]);
            } else {
                panic!("could not get response from pq layer");
            }
        } else {
            panic!("pq layer returned request instead of returning an error response");
        }
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
            None,
        )
        .await
        .unwrap();
        let incoming_request = SupergraphRequest::fake_builder()
            .extension("persistedQuery", json!({"version": 1, "sha256Hash": id}))
            .query(body)
            .build()
            .unwrap();

        assert!(incoming_request.supergraph_request.body().query.is_some());

        let result = pq_layer.supergraph_request(incoming_request);
        if let Err(mut response) = result {
            if let Some(response) = response.next_response().await {
                assert_eq!(response.errors, vec![graphql_err_cannot_send_id_and_body()]);
            } else {
                panic!("could not get response from pq layer");
            }
        } else {
            panic!("pq layer returned request instead of returning an error response");
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn can_memoize_is_persisted() {
        let mut is_persisted = None;

        let (_id, body, manifest) = fake_manifest();

        let (_mock_guard, uplink_config) = mock_pq_uplink(&manifest).await;

        let manifest_poller = Arc::new(
            PersistedQueryManifestPoller::new(&uplink_config)
                .await
                .unwrap(),
        );

        assert_eq!(is_persisted, None);
        assert!(is_operation_persisted(
            &mut is_persisted,
            manifest_poller.clone(),
            &body
        ));
        assert_eq!(is_persisted, Some(true));
        assert!(is_operation_persisted(
            &mut is_persisted,
            manifest_poller,
            &body
        ));
    }
}
