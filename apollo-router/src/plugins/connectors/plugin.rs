use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;

use apollo_federation::connectors::runtime::debug::ConnectorContext;
use futures::StreamExt;
use http::HeaderValue;
use itertools::Itertools;
use parking_lot::Mutex;
use serde_json_bytes::json;
use tower::BoxError;
use tower::ServiceBuilder;
use tower::ServiceExt as TowerServiceExt;

use super::query_plans::get_connectors;
use crate::layers::ServiceExt;
use crate::plugin::Plugin;
use crate::plugin::PluginInit;
use crate::plugins::connectors::configuration::ConnectorsConfig;
use crate::plugins::connectors::declared_errors::CONNECTOR_ERRORS_EXTENSION_KEY;
use crate::plugins::connectors::declared_errors::ConnectorDeclaredErrors;
use crate::plugins::connectors::request_limit::RequestLimits;
use crate::services::connector_service::ConnectorSourceRef;
use crate::services::execution;
use crate::services::supergraph;

const CONNECTORS_DEBUG_HEADER_NAME: &str = "Apollo-Connectors-Debugging";
const CONNECTORS_DEBUG_ENV: &str = "APOLLO_CONNECTORS_DEBUGGING";
const CONNECTORS_DEBUG_KEY: &str = "apolloConnectorsDebugging";
const CONNECTORS_MAX_REQUESTS_ENV: &str = "APOLLO_CONNECTORS_MAX_REQUESTS_PER_OPERATION";
const CONNECTOR_SOURCES_IN_QUERY_PLAN: &str = "apollo_connectors::sources_in_query_plan";

static LAST_DEBUG_ENABLED_VALUE: AtomicBool = AtomicBool::new(false);

#[derive(Debug, Clone)]
struct Connectors {
    debug_extensions: bool,
    max_requests: Option<usize>,
    expose_sources_in_context: bool,
}

#[async_trait::async_trait]
impl Plugin for Connectors {
    type Config = ConnectorsConfig;

    async fn new(init: PluginInit<Self::Config>) -> Result<Self, BoxError> {
        let debug_extensions = init.config.debug_extensions
            || std::env::var(CONNECTORS_DEBUG_ENV).as_deref() == Ok("true");

        let last_value = LAST_DEBUG_ENABLED_VALUE.load(Ordering::Relaxed);
        let swap_result = LAST_DEBUG_ENABLED_VALUE.compare_exchange(
            last_value,
            debug_extensions,
            Ordering::Relaxed,
            Ordering::Relaxed,
        );
        // Ok means we swapped value, inner value is old value. Ok(false) means we went false -> true
        if matches!(swap_result, Ok(false)) {
            tracing::warn!(
                "Connector debugging is enabled, this may expose sensitive information."
            );
        }

        #[allow(deprecated)]
        if !init.config.subgraphs.is_empty() {
            tracing::warn!(
                "The `connectors.subgraphs` configuration field is deprecated and will be \
                 removed in a future release. Rename it to `connectors.sources`. See \
                 https://www.apollographql.com/docs/graphos/routing/configuration/yaml#connectors"
            );
        }

        let max_requests = init
            .config
            .max_requests_per_operation_per_source
            .or(std::env::var(CONNECTORS_MAX_REQUESTS_ENV)
                .ok()
                .and_then(|v| v.parse().ok()));

        Ok(Connectors {
            debug_extensions,
            max_requests,
            expose_sources_in_context: init.config.expose_sources_in_context,
        })
    }

    fn supergraph_service(&self, service: supergraph::BoxService) -> supergraph::BoxService {
        let conf_enabled = self.debug_extensions;
        let max_requests = self.max_requests;
        service
            .map_future_with_request_data(
                move |req: &supergraph::Request| {
                    let is_debug_enabled = conf_enabled
                        && req
                            .supergraph_request
                            .headers()
                            .get(CONNECTORS_DEBUG_HEADER_NAME)
                            == Some(&HeaderValue::from_static("true"));

                    req.context.extensions().with_lock(|lock| {
                        lock.insert::<Arc<RequestLimits>>(Arc::new(RequestLimits::new(
                            max_requests,
                        )));
                        if is_debug_enabled {
                            lock.insert::<Arc<Mutex<ConnectorContext>>>(Arc::new(Mutex::new(
                                ConnectorContext::default(),
                            )));
                        }
                    });

                    is_debug_enabled
                },
                move |is_debug_enabled: bool, f| async move {
                    let mut res: supergraph::ServiceResult = f.await;

                    res = match res {
                        Ok(mut res) => {
                            res.context.extensions().with_lock(|lock| {
                                if let Some(limits) = lock.remove::<Arc<RequestLimits>>() {
                                    limits.log();
                                }
                            });
                            let debug = is_debug_enabled
                                .then(|| {
                                    res.context.extensions().with_lock(|lock| {
                                        lock.get::<Arc<Mutex<ConnectorContext>>>().cloned()
                                    })
                                })
                                .flatten();

                            // Errors declared with `->withError` are reported
                            // here rather than in `errors`: the fields they
                            // describe resolved, and the GraphQL spec reserves
                            // `errors` for positions absent from `data`. The
                            // fetch service has already lifted them out of the
                            // subgraph responses' `errors` and into the
                            // context; this is where they reach the client.
                            let context = res.context.clone();
                            let (parts, stream) = res.response.into_parts();

                            let stream = stream.map(move |mut chunk| {
                                if let Some(errors) = ConnectorDeclaredErrors::drain(&context) {
                                    chunk
                                        .extensions
                                        .insert(CONNECTOR_ERRORS_EXTENSION_KEY, errors);
                                }
                                if let Some(debug) = &debug {
                                    let serialized = { &debug.lock().clone().serialize() };
                                    chunk.extensions.insert(
                                        CONNECTORS_DEBUG_KEY,
                                        json!({"version": "2", "data": serialized }),
                                    );
                                }
                                chunk
                            });

                            res.response = http::Response::from_parts(parts, Box::pin(stream));

                            Ok(res)
                        }
                        Err(err) => Err(err),
                    };

                    res
                },
            )
            .boxed()
    }

    fn execution_service(&self, service: execution::BoxService) -> execution::BoxService {
        if !self.expose_sources_in_context {
            return service;
        }

        ServiceBuilder::new()
            .map_request(|req: execution::Request| {
                let Some(connectors) = get_connectors(&req.context) else {
                    return req;
                };

                // add [{"subgraph_name": "", "source_name": ""}] to the context
                // for connectors with sources in the query plan.
                let list = req
                    .query_plan
                    .root
                    .service_usage_set()
                    .into_iter()
                    .flat_map(|service_name| {
                        connectors
                            .get(service_name)
                            .map(|connector| ConnectorSourceRef::try_from(connector).ok())
                    })
                    .unique()
                    .collect_vec();

                req.context
                    .insert(CONNECTOR_SOURCES_IN_QUERY_PLAN, list)
                    .unwrap();
                req
            })
            .service(service)
            .boxed()
    }
}

pub(crate) const PLUGIN_NAME: &str = "connectors";

register_plugin!("apollo", PLUGIN_NAME, Connectors);

#[cfg(test)]
mod tests {
    use futures::StreamExt;
    use serde_json_bytes::Value;

    use super::*;
    use crate::graphql;
    use crate::json_ext::Path;
    use crate::plugins::connectors::declared_errors::DECLARED_ERROR_MARKER;
    use crate::plugins::test::PluginTestHarness;

    /// One declared error, as it looks on arrival from a connector fetch.
    fn declared() -> graphql::Error {
        graphql::Error::builder()
            .message("balance unavailable")
            .path(Path::from("account/balance"))
            .extension_code("CONNECTORS_MAPPING_ERROR")
            .extension(DECLARED_ERROR_MARKER, Value::Bool(true))
            .build()
    }

    async fn harness() -> PluginTestHarness<Connectors> {
        PluginTestHarness::builder()
            .config("connectors: {}")
            .build()
            .await
            .expect("plugin should load")
    }

    /// The far end of the hand-off, asserted on what the client actually
    /// receives: a declared error reaches the response `extensions`, with the
    /// path the fetch node gave it, and reaches `errors` not at all — the
    /// field it describes is present in `data`, and the spec reserves `errors`
    /// for positions that are not.
    #[tokio::test]
    async fn declared_errors_are_reported_in_the_response_extensions() {
        let harness = harness().await;

        let service = harness.supergraph_service(|req| async move {
            // Stands in for a connector fetch completing mid-request.
            ConnectorDeclaredErrors::take_marked(&req.context, &mut vec![declared()]);

            supergraph::Response::fake_builder()
                .data(json!({ "account": { "balance": 0 } }))
                .context(req.context)
                .build()
        });

        let mut response = service.call_default().await.unwrap();
        let chunks: Vec<graphql::Response> = response.response.body_mut().collect().await;

        assert_eq!(chunks.len(), 1);
        assert!(chunks[0].errors.is_empty());
        assert_eq!(
            chunks[0].extensions.get(CONNECTOR_ERRORS_EXTENSION_KEY),
            Some(&json!([{
                "message": "balance unavailable",
                "path": ["account", "balance"],
                "extensions": { "code": "CONNECTORS_MAPPING_ERROR" },
            }])),
        );

        // On the wire, the execution result has no `errors` entry at all —
        // the spec requires it to be absent, not empty, when execution raised
        // nothing — and no entry outside `data`/`errors`/`extensions`.
        let serialized = serde_json_bytes::to_value(&chunks[0]).expect("serializes");
        let serialized = serialized.as_object().expect("a map");
        assert_eq!(
            serialized
                .keys()
                .map(|key| key.as_str())
                .collect::<Vec<_>>(),
            vec!["data", "extensions"],
        );
    }

    /// The common case: a request whose connectors declared nothing gets no
    /// `connectorErrors` key at all, rather than an empty array.
    #[tokio::test]
    async fn a_response_without_declared_errors_carries_no_extension() {
        let harness = harness().await;

        let service = harness.supergraph_service(|req| async move {
            supergraph::Response::fake_builder()
                .data(json!({ "account": { "balance": 0 } }))
                .context(req.context)
                .build()
        });

        let mut response = service.call_default().await.unwrap();
        let chunks: Vec<graphql::Response> = response.response.body_mut().collect().await;

        assert_eq!(chunks.len(), 1);
        assert!(
            !chunks[0]
                .extensions
                .contains_key(CONNECTOR_ERRORS_EXTENSION_KEY)
        );
    }
}
