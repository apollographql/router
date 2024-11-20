pub(crate) mod debug;

use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use debug::ConnectorContext;
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
use crate::plugins::connectors::request_limit::RequestLimits;
use crate::register_plugin;
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

                    req.context.extensions().with_lock(|mut lock| {
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
                            res.context.extensions().with_lock(|mut lock| {
                                if let Some(limits) = lock.remove::<Arc<RequestLimits>>() {
                                    limits.log();
                                }
                            });
                            if is_debug_enabled {
                                if let Some(debug) =
                                    res.context.extensions().with_lock(|mut lock| {
                                        lock.remove::<Arc<Mutex<ConnectorContext>>>()
                                    })
                                {
                                    let (parts, stream) = res.response.into_parts();
                                    let (mut first, rest) = stream.into_future().await;

                                    if let Some(first) = &mut first {
                                        if let Some(inner) = Arc::into_inner(debug) {
                                            first.extensions.insert(
                                            CONNECTORS_DEBUG_KEY,
                                                json!({"version": "1", "data": inner.into_inner().serialize() }),
                                        );
                                        }
                                    }
                                    res.response = http::Response::from_parts(
                                        parts,
                                        once(ready(first.unwrap_or_default())).chain(rest).boxed(),
                                    );
                                }
                            }

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

pub(crate) const PLUGIN_NAME: &str = "preview_connectors";

register_plugin!("apollo", PLUGIN_NAME, Connectors);
