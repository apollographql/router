mod id_extractor;
mod manifest_poller;

#[cfg(test)]
use std::sync::Arc;

use apollo_compiler::AstDatabase;
use apollo_compiler::HirDatabase;
use apollo_compiler::InputDatabase;
use http::header::CACHE_CONTROL;
use http::HeaderValue;
use id_extractor::PersistedQueryIdExtractor;
pub(crate) use manifest_poller::PersistedQueryManifestPoller;
use tower::BoxError;

use self::manifest_poller::FreeformGraphQLAction;
use super::query_analysis::Compiler;
use crate::graphql::Error as GraphQLError;
use crate::services::SupergraphRequest;
use crate::services::SupergraphResponse;
use crate::spec::query::QUERY_EXECUTABLE;
use crate::Configuration;

const DONT_CACHE_RESPONSE_VALUE: &str = "private, no-cache, must-revalidate";

struct UsedQueryIdFromManifest;

#[derive(Debug)]
pub(crate) struct PersistedQueryLayer {
    /// Manages polling uplink for persisted queries and caches the current
    /// value of the manifest and projected safelist. None if the layer is disabled.
    pub(crate) manifest_poller: Option<PersistedQueryManifestPoller>,
    introspection_enabled: bool,
}

impl PersistedQueryLayer {
    /// Create a new [`PersistedQueryLayer`] from CLI options, YAML configuration,
    /// and optionally, an existing persisted query manifest poller.
    pub(crate) async fn new(configuration: &Configuration) -> Result<Self, BoxError> {
        if configuration.preview_persisted_queries.enabled {
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
                    manifest_poller,
                    &persisted_query_id,
                )
            } else if let Some(log_unknown) = manifest_poller.never_allows_freeform_graphql() {
                // If we don't have an ID and we require an ID, return an error immediately,
                if log_unknown {
                    if let Some(operation_body) = request.supergraph_request.body().query.as_ref() {
                        log_unknown_operation(operation_body);
                    }
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
            if let Some(persisted_query_body) =
                manifest_poller.get_operation_body(persisted_query_id)
            {
                let body = request.supergraph_request.body_mut();
                body.query = Some(persisted_query_body);
                body.extensions.remove("persistedQuery");
                // Record that we actually used our ID, so we can skip the
                // safelist check later.
                request
                    .context
                    .private_entries
                    .lock()
                    .insert(UsedQueryIdFromManifest);
                tracing::info!(monotonic_counter.apollo.router.operations.persisted_queries = 1u64);
                Ok(request)
            } else if manifest_poller.augmenting_apq_with_pre_registration_and_no_safelisting() {
                // The query ID isn't in our manifest, but we have APQ enabled
                // (and no safelisting) so we just let APQ handle it instead of
                // returning an error. (We still might check against the
                // safelist later for log_unknown!)
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

        let compiler = {
            let context_guard = request.context.private_entries.lock();

            if context_guard.get::<UsedQueryIdFromManifest>().is_some() {
                // We got this operation from the manifest, so there's no
                // need to check the safelist.
                drop(context_guard);
                return Ok(request);
            }

            match context_guard.get::<Compiler>() {
                None => {
                    drop(context_guard);
                    // For some reason, QueryAnalysisLayer didn't give us a Compiler?
                    return Err(supergraph_err(
                        graphql_err(
                            "MISSING_PARSED_OPERATION",
                            "internal error: compiler missing",
                        ),
                        request,
                        ErrorCacheStrategy::DontCache,
                    ));
                }
                Some(c) => c.0.clone(),
            }
        };

        let compiler_guard = compiler.lock().await;
        let db = &compiler_guard.db;
        let file_id = match db.source_file(QUERY_EXECUTABLE.into()) {
            Some(file_id) => file_id,
            None => {
                return Err(supergraph_err(
                    graphql_err("MISSING_PARSED_OPERATION", "missing input file for query"),
                    request,
                    ErrorCacheStrategy::DontCache,
                ))
            }
        };

        // If introspection is enabled in this server, all introspection
        // requests are always allowed. (This means any document all of whose
        // top-level fields in all operations (after spreading fragments) are
        // __type/__schema/__typename.) We do want to make sure the document
        // parsed properly before poking around at it, though.
        if self.introspection_enabled
            && db.ast(file_id).errors().peekable().peek().is_none()
            && db
                .operations(file_id)
                .iter()
                .all(|op| op.is_introspection(db))
        {
            return Ok(request);
        }

        match manifest_poller.action_for_freeform_graphql(operation_body, db.ast(file_id)) {
            FreeformGraphQLAction::Allow => {
                tracing::info!(monotonic_counter.apollo.router.operations.persisted_queries = 1u64,);
                Ok(request)
            }
            FreeformGraphQLAction::Deny => {
                tracing::info!(
                    monotonic_counter.apollo.router.operations.persisted_queries = 1u64,
                    persisted_queries.safelist.rejected.unknown = false,
                );
                Err(supergraph_err_operation_not_in_safelist(request))
            }
            // Note that this might even include complaining about an operation that came via APQs.
            FreeformGraphQLAction::AllowAndLog => {
                tracing::info!(
                    monotonic_counter.apollo.router.operations.persisted_queries = 1u64,
                    persisted_queries.logged = true
                );
                log_unknown_operation(operation_body);
                Ok(request)
            }
            FreeformGraphQLAction::DenyAndLog => {
                tracing::info!(
                    monotonic_counter.apollo.router.operations.persisted_queries = 1u64,
                    persisted_queries.safelist.rejected.unknown = true,
                    persisted_queries.logged = true
                );
                log_unknown_operation(operation_body);
                Err(supergraph_err_operation_not_in_safelist(request))
            }
        }
    }

    pub(crate) fn all_operations(&self) -> Option<Vec<String>> {
        self.manifest_poller
            .as_ref()
            .map(|poller| poller.get_all_operations())
    }
}

fn log_unknown_operation(operation_body: &str) {
    tracing::warn!(message = "unknown operation", operation_body);
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
    use std::collections::HashMap;
    use std::time::Duration;

    use serde_json::json;

    use super::*;
    use crate::configuration::Apq;
    use crate::configuration::PersistedQueries;
    use crate::configuration::PersistedQueriesSafelist;
    use crate::configuration::Supergraph;
    use crate::services::layers::persisted_queries::manifest_poller::FreeformGraphQLBehavior;
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

        assert!(PersistedQueryManifestPoller::new(
            Configuration::fake_builder()
                .uplink(uplink_config)
                .build()
                .unwrap(),
        )
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
        )
        .await
        .unwrap();
        let incoming_request = SupergraphRequest::fake_builder()
            .extension("persistedQuery", json!({"version": 1, "sha256Hash": id}))
            .build()
            .unwrap();

        assert!(incoming_request.supergraph_request.body().query.is_none());

        let result = pq_layer.supergraph_request(incoming_request);
        let request = result
            .ok()
            .expect("pq layer returned response instead of putting the query on the request");
        assert_eq!(request.supergraph_request.body().query, Some(body));
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
        let request = result
            .ok()
            .expect("pq layer returned response instead of continuing to APQ layer");
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

        let response = pq_layer
            .supergraph_request(incoming_request)
            .expect_err("pq layer returned request instead of returning an error response")
            .next_response()
            .await
            .expect("could not get response from pq layer");
        assert_eq!(
            response.errors,
            vec![graphql_err_operation_not_found(invalid_id)]
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
        assert!(pq_layer
            .manifest_poller
            .unwrap()
            .augmenting_apq_with_pre_registration_and_no_safelisting())
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
        assert!(!pq_layer
            .manifest_poller
            .unwrap()
            .augmenting_apq_with_pre_registration_and_no_safelisting())
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
                .unwrap()
                .freeform_graphql_behavior,
            FreeformGraphQLBehavior::AllowIfInSafelist { .. }
        ))
    }

    async fn run_first_two_layers(
        pq_layer: &PersistedQueryLayer,
        query_analysis_layer: &QueryAnalysisLayer,
        body: &str,
    ) -> SupergraphRequest {
        let incoming_request = SupergraphRequest::fake_builder()
            .query(body)
            .build()
            .unwrap();

        assert!(incoming_request.supergraph_request.body().query.is_some());

        // The initial hook won't block us --- that waits until after we've parsed
        // the operation.
        let updated_request = pq_layer
            .supergraph_request(incoming_request)
            .ok()
            .expect("pq layer returned error response instead of returning a request");
        query_analysis_layer
            .supergraph_request(updated_request)
            .await
            .ok()
            .expect("QA layer returned error response instead of returning a request")
    }

    async fn denied_by_safelist(
        pq_layer: &PersistedQueryLayer,
        query_analysis_layer: &QueryAnalysisLayer,
        body: &str,
    ) {
        let request_with_analyzed_query =
            run_first_two_layers(pq_layer, query_analysis_layer, body).await;

        let response = pq_layer
            .supergraph_request_with_analyzed_query(request_with_analyzed_query)
            .await
            .expect_err(
                "pq layer second hook returned request instead of returning an error response",
            )
            .next_response()
            .await
            .expect("could not get response from pq layer");
        assert_eq!(
            response.errors,
            vec![graphql_err_operation_not_in_safelist()]
        );
    }

    async fn allowed_by_safelist(
        pq_layer: &PersistedQueryLayer,
        query_analysis_layer: &QueryAnalysisLayer,
        body: &str,
    ) {
        let request_with_analyzed_query =
            run_first_two_layers(pq_layer, query_analysis_layer, body).await;

        pq_layer
            .supergraph_request_with_analyzed_query(request_with_analyzed_query)
            .await
            .ok()
            .expect("pq layer second hook returned error response instead of returning a request");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn pq_layer_freeform_graphql_with_safelist() {
        let manifest = HashMap::from([(
            "valid-syntax".to_string(),
            "fragment A on T { a }    query SomeOp { ...A ...B }    fragment,,, B on U{b c  } # yeah"
                .to_string(),
        ), (
            "invalid-syntax".to_string(),
            "}}}".to_string()),
        ]);

        let (_mock_guard, uplink_config) = mock_pq_uplink(&manifest).await;

        let config = Configuration::fake_builder()
            .persisted_query(
                PersistedQueries::builder()
                    .enabled(true)
                    .safelist(PersistedQueriesSafelist::builder().enabled(true).build())
                    .build(),
            )
            .uplink(uplink_config)
            .apq(Apq::fake_builder().enabled(false).build())
            .supergraph(Supergraph::fake_builder().introspection(true).build())
            .build()
            .unwrap();

        let pq_layer = PersistedQueryLayer::new(&config).await.unwrap();

        let schema = Arc::new(
            Schema::parse_test(
                include_str!("../../../testdata/supergraph.graphql"),
                &config,
            )
            .unwrap(),
        );

        let query_analysis_layer = QueryAnalysisLayer::new(schema, Arc::new(config)).await;

        // A random query is blocked.
        denied_by_safelist(
            &pq_layer,
            &query_analysis_layer,
            "query SomeQuery { hooray }",
        )
        .await;

        // The exact string from the manifest is allowed.
        allowed_by_safelist(
            &pq_layer,
            &query_analysis_layer,
            "fragment A on T { a }    query SomeOp { ...A ...B }    fragment,,, B on U{b c  } # yeah",
        )
        .await;

        // Reordering definitions and reformatting a bit matches.
        allowed_by_safelist(
                &pq_layer,
                &query_analysis_layer,
                    "#comment\n  fragment, B on U  , { b    c }    query SomeOp {  ...A ...B }  fragment    \nA on T { a }"
            ).await;

        // Reordering fields does not match!
        denied_by_safelist(
                &pq_layer,
                &query_analysis_layer,
                    "fragment A on T { a }    query SomeOp { ...A ...B }    fragment,,, B on U{c b  } # yeah"
            ).await;

        // Documents with invalid syntax don't match...
        denied_by_safelist(&pq_layer, &query_analysis_layer, "}}}}").await;

        // ... unless they precisely match a safelisted document that also has invalid syntax.
        allowed_by_safelist(&pq_layer, &query_analysis_layer, "}}}").await;

        // Introspection queries are allowed (even using fragments and aliases), because
        // introspection is enabled.
        allowed_by_safelist(
            &pq_layer,
            &query_analysis_layer,
            r#"fragment F on Query { __typename foo: __schema { __typename } } query Q { __type(name: "foo") { name } ...F }"#,
        ).await;

        // Multiple spreads of the same fragment are also allowed
        // (https://github.com/apollographql/apollo-rs/issues/613)
        allowed_by_safelist(
            &pq_layer,
            &query_analysis_layer,
            r#"fragment F on Query { __typename foo: __schema { __typename } } query Q { __type(name: "foo") { name } ...F ...F }"#,
        ).await;

        // But adding any top-level non-introspection field is enough to make it not count as introspection.
        denied_by_safelist(
            &pq_layer,
            &query_analysis_layer,
            r#"fragment F on Query { __typename foo: __schema { __typename } bla } query Q { __type(name: "foo") { name } ...F }"#,
        ).await;
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
            .build()
            .unwrap();

        assert!(incoming_request.supergraph_request.body().query.is_none());

        let result = pq_layer.supergraph_request(incoming_request);
        let response = result
            .expect_err("pq layer returned request instead of returning an error response")
            .next_response()
            .await
            .expect("could not get response from pq layer");
        assert_eq!(
            response.errors,
            vec![graphql_err_operation_not_found(invalid_id)]
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn apq_and_pq_safelisting_is_invalid_config() {
        let (_mock_guard, uplink_config) = mock_empty_pq_uplink().await;
        let safelist_config = PersistedQueriesSafelist::builder().enabled(true).build();
        assert!(Configuration::fake_builder()
            .persisted_query(
                PersistedQueries::builder()
                    .enabled(true)
                    .safelist(safelist_config)
                    .build(),
            )
            .apq(Apq::fake_builder().enabled(true).build())
            .uplink(uplink_config)
            .build()
            .is_err());
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
                .unwrap()
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
        assert!(pq_layer
            .manifest_poller
            .unwrap()
            .never_allows_freeform_graphql()
            .is_some())
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

        let result = pq_layer.supergraph_request(incoming_request);
        let response = result
            .expect_err("pq layer returned request instead of returning an error response")
            .next_response()
            .await
            .expect("could not get response from pq layer");
        assert_eq!(response.errors, vec![graphql_err_pq_id_required()]);
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
                .unwrap()
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
                .unwrap()
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

        let result = pq_layer.supergraph_request(incoming_request);
        let response = result
            .expect_err("pq layer returned request instead of returning an error response")
            .next_response()
            .await
            .expect("could not get response from pq layer");
        assert_eq!(response.errors, vec![graphql_err_cannot_send_id_and_body()]);
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

        let response = pq_layer
            .supergraph_request(incoming_request)
            .expect_err("pq layer returned request instead of returning an error response")
            .next_response()
            .await
            .expect("could not get response from pq layer");
        assert_eq!(response.errors, vec![graphql_err_cannot_send_id_and_body()]);
    }
}
