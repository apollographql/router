//! Tower service for connectors.

use std::collections::HashMap;
use std::str::FromStr;
use std::sync::Arc;
use std::task::Poll;

use apollo_compiler::validation::Valid;
use apollo_federation::sources::connect::Connector;
use futures::future::BoxFuture;
use indexmap::IndexMap;
use opentelemetry::metrics::ObservableGauge;
use opentelemetry::Key;
use parking_lot::Mutex;
use serde::Deserialize;
use serde::Serialize;
use tower::BoxError;
use tower::ServiceExt;
use tracing::error;
use tracing::Instrument;

use super::connect::BoxService;
use super::http::HttpClientServiceFactory;
use super::http::HttpRequest;
use super::new_service::ServiceFactory;
use crate::error::FetchError;
use crate::plugins::connectors::error::Error as ConnectorError;
use crate::plugins::connectors::handle_responses::aggregate_responses;
use crate::plugins::connectors::handle_responses::process_response;
use crate::plugins::connectors::http::Request;
use crate::plugins::connectors::http::Response as ConnectorResponse;
use crate::plugins::connectors::http::Result as ConnectorResult;
use crate::plugins::connectors::make_requests::make_requests;
use crate::plugins::connectors::plugin::debug::ConnectorContext;
use crate::plugins::connectors::request_limit::RequestLimits;
use crate::plugins::connectors::tracing::connect_spec_version_instrument;
use crate::plugins::connectors::tracing::CONNECTOR_TYPE_HTTP;
use crate::plugins::subscription::SubscriptionConfig;
use crate::plugins::telemetry::consts::CONNECT_SPAN_NAME;
use crate::services::router::body::RouterBody;
use crate::services::ConnectRequest;
use crate::services::ConnectResponse;
use crate::spec::Schema;

pub(crate) const APOLLO_CONNECTOR_TYPE: Key = Key::from_static_str("apollo.connector.type");
pub(crate) const APOLLO_CONNECTOR_DETAIL: Key = Key::from_static_str("apollo.connector.detail");
pub(crate) const APOLLO_CONNECTOR_SELECTION: Key =
    Key::from_static_str("apollo.connector.selection");
pub(crate) const APOLLO_CONNECTOR_FIELD_NAME: Key =
    Key::from_static_str("apollo.connector.field.name");
pub(crate) const APOLLO_CONNECTOR_FIELD_ALIAS: Key =
    Key::from_static_str("apollo.connector.field.alias");
pub(crate) const APOLLO_CONNECTOR_FIELD_RETURN_TYPE: Key =
    Key::from_static_str("apollo.connector.field.return_type");
pub(crate) const APOLLO_CONNECTOR_SOURCE_NAME: Key =
    Key::from_static_str("apollo.connector.source.name");
pub(crate) const APOLLO_CONNECTOR_SOURCE_DETAIL: Key =
    Key::from_static_str("apollo.connector.source.detail");
pub(crate) const CONNECTOR_INFO_CONTEXT_KEY: &str = "apollo_router::connector::info";

/// A service for executing connector requests.
#[derive(Clone)]
pub(crate) struct ConnectorService {
    pub(crate) http_service_factory: Arc<IndexMap<String, HttpClientServiceFactory>>,
    pub(crate) _schema: Arc<Schema>,
    pub(crate) _subgraph_schemas: Arc<HashMap<String, Arc<Valid<apollo_compiler::Schema>>>>,
    pub(crate) _subscription_config: Option<SubscriptionConfig>,
    pub(crate) connectors_by_service_name: Arc<IndexMap<Arc<str>, Connector>>,
}

/// Serializable information about a connector.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct ConnectorInfo {
    pub(crate) subgraph_name: String,
    pub(crate) source_name: Option<String>,
    pub(crate) http_method: String,
    pub(crate) url_template: String,
}

impl From<&Connector> for ConnectorInfo {
    fn from(connector: &Connector) -> Self {
        Self {
            subgraph_name: connector.id.subgraph_name.to_string(),
            source_name: connector.id.source_name.clone(),
            http_method: connector.transport.method.as_str().to_string(),
            url_template: connector.transport.connect_template.to_string(),
        }
    }
}

/// A reference to a unique Connector source.
#[derive(Hash, Eq, PartialEq, Clone, Serialize, Deserialize)]
pub(crate) struct ConnectorSourceRef {
    pub(crate) subgraph_name: String,
    pub(crate) source_name: String,
}

impl ConnectorSourceRef {
    pub(crate) fn new(subgraph_name: String, source_name: String) -> Self {
        Self {
            subgraph_name,
            source_name,
        }
    }
}

impl FromStr for ConnectorSourceRef {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let mut parts = s.split('.');
        let subgraph_name = parts
            .next()
            .ok_or(format!("Invalid connector source reference '{}'", s))?
            .to_string();
        let source_name = parts
            .next()
            .ok_or(format!("Invalid connector source reference '{}'", s))?
            .to_string();
        Ok(Self::new(subgraph_name, source_name))
    }
}

impl TryFrom<&Connector> for ConnectorSourceRef {
    type Error = ();

    fn try_from(value: &Connector) -> Result<Self, Self::Error> {
        Ok(Self {
            subgraph_name: value.id.subgraph_name.to_string(),
            source_name: value.id.source_name.clone().ok_or(())?,
        })
    }
}

impl tower::Service<ConnectRequest> for ConnectorService {
    type Response = ConnectResponse;
    type Error = BoxError;
    type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn poll_ready(&mut self, _cx: &mut std::task::Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: ConnectRequest) -> Self::Future {
        let connector = self
            .connectors_by_service_name
            .get(&request.service_name)
            .cloned();

        let http_client_factory = self
            .http_service_factory
            .get(&request.service_name.to_string())
            .cloned();

        Box::pin(async move {
            let Some(connector) = connector else {
                return Err("no connector found".into());
            };

            let Some(http_client_factory) = http_client_factory else {
                return Err("no http client found".into());
            };
            let fetch_time_offset = request.context.created_at.elapsed().as_nanos() as i64;
            let span = tracing::info_span!(
                CONNECT_SPAN_NAME,
                "otel.kind" = "INTERNAL",
                "apollo.connector.type" = CONNECTOR_TYPE_HTTP,
                "apollo.connector.detail" = tracing::field::Empty,
                "apollo.connector.field.name" = %connector.field_name(),
                "apollo.connector.selection" = %connector.selection,
                "apollo.connector.source.name" = tracing::field::Empty,
                "apollo.connector.source.detail" = tracing::field::Empty,
                "apollo_private.sent_time_offset" = fetch_time_offset,
                "otel.status_code" = tracing::field::Empty,
            );
            // TODO: I think we should get rid of these attributes by default and only add it from custom telemetry. We just need to double check it's not required for Studio.

            // These additional attributes will be added to custom telemetry feature
            // TODO: apollo.connector.field.alias
            // TODO: apollo.connector.field.return_type
            // TODO: apollo.connector.field.selection_set
            let transport = &connector.transport;
            if let Ok(detail) = serde_json::to_string(
                &serde_json::json!({ transport.method.as_str(): transport.connect_template.to_string() }),
            ) {
                span.record("apollo.connector.detail", detail);
            }
            if let Some(source_name) = connector.id.source_name.as_ref() {
                span.record("apollo.connector.source.name", source_name);
                if let Ok(detail) =
                    serde_json::to_string(&serde_json::json!({ "baseURL": transport.source_url }))
                {
                    span.record("apollo.connector.source.detail", detail);
                }
            }

            execute(&http_client_factory, request, &connector)
                .instrument(span)
                .await
        })
    }
}

async fn execute(
    http_client_factory: &HttpClientServiceFactory,
    request: ConnectRequest,
    connector: &Connector,
) -> Result<ConnectResponse, BoxError> {
    let context = request.context.clone();
    let original_subgraph_name = connector.id.subgraph_name.to_string();

    let (ref debug, request_limit) = context.extensions().with_lock(|lock| {
        let debug = lock.get::<Arc<Mutex<ConnectorContext>>>().cloned();
        let request_limit = lock
            .get::<Arc<RequestLimits>>()
            .map(|limits| limits.get((&connector.id).into(), connector.max_requests))
            .unwrap_or(None);
        (debug, request_limit)
    });

    let requests = make_requests(request, connector, debug).map_err(BoxError::from)?;

    let tasks = requests.into_iter().map(
        move |Request {
                  request: req,
                  key,
                  debug_request,
              }| {
            // Returning an error from this closure causes all tasks to be cancelled and the operation
            // to fail. This is the reason for the Result-wrapped-in-a-Result here. An `Err` on the
            // inner result fails just that one task, but an `Err` on the outer result cancels all the
            // tasks and fails the whole operation.
            let context = context.clone();
            if context
                .insert(CONNECTOR_INFO_CONTEXT_KEY, ConnectorInfo::from(connector))
                .is_err()
            {
                error!("Failed to store connector info in context");
            }
            let original_subgraph_name = original_subgraph_name.clone();
            let request_limit = request_limit.clone();
            async move {
                let res = if request_limit.is_some_and(|request_limit| !request_limit.allow()) {
                    ConnectorResponse {
                        result: ConnectorResult::<RouterBody>::Err(
                            ConnectorError::RequestLimitExceeded,
                        ),
                        key,
                        debug_request,
                    }
                } else {
                    let client = http_client_factory.create(&original_subgraph_name);
                    let req = HttpRequest {
                        http_request: req,
                        context: context.clone(),
                    };
                    let res = match client.oneshot(req).await {
                        Ok(res) => ConnectorResponse {
                            result: ConnectorResult::HttpResponse(res.http_response),
                            key,
                            debug_request,
                        },
                        Err(e) => ConnectorResponse {
                            result: ConnectorResult::<RouterBody>::Err(
                                ConnectorError::HTTPClientError(handle_subrequest_http_error(
                                    e, connector,
                                )),
                            ),
                            key,
                            debug_request,
                        },
                    };

                    u64_counter!(
                        "apollo.router.operations.connectors",
                        "Total number of requests to connectors",
                        1,
                        "connector.type" = CONNECTOR_TYPE_HTTP,
                        "subgraph.name" = original_subgraph_name
                    );

                    res
                };

                Ok::<_, BoxError>(process_response(res, connector, &context, debug).await)
            }
        },
    );

    aggregate_responses(futures::future::try_join_all(tasks).await?).map_err(BoxError::from)
}

fn handle_subrequest_http_error(err: BoxError, connector: &Connector) -> BoxError {
    match err.downcast::<FetchError>() {
        // Replace the internal subgraph name with the connector label
        Ok(inner) => match *inner {
            FetchError::SubrequestHttpError {
                status_code,
                service: _,
                reason,
            } => Box::new(FetchError::SubrequestHttpError {
                status_code,
                service: connector.id.subgraph_source(),
                reason,
            }),
            _ => inner,
        },
        Err(e) => e,
    }
}

#[derive(Clone)]
pub(crate) struct ConnectorServiceFactory {
    pub(crate) schema: Arc<Schema>,
    pub(crate) subgraph_schemas: Arc<HashMap<String, Arc<Valid<apollo_compiler::Schema>>>>,
    pub(crate) http_service_factory: Arc<IndexMap<String, HttpClientServiceFactory>>,
    pub(crate) subscription_config: Option<SubscriptionConfig>,
    pub(crate) connectors_by_service_name: Arc<IndexMap<Arc<str>, Connector>>,
    _connect_spec_version_instrument: Option<ObservableGauge<u64>>,
}

impl ConnectorServiceFactory {
    pub(crate) fn new(
        schema: Arc<Schema>,
        subgraph_schemas: Arc<HashMap<String, Arc<Valid<apollo_compiler::Schema>>>>,
        http_service_factory: Arc<IndexMap<String, HttpClientServiceFactory>>,
        subscription_config: Option<SubscriptionConfig>,
        connectors_by_service_name: Arc<IndexMap<Arc<str>, Connector>>,
    ) -> Self {
        Self {
            http_service_factory,
            subgraph_schemas,
            schema: schema.clone(),
            subscription_config,
            connectors_by_service_name,
            _connect_spec_version_instrument: connect_spec_version_instrument(
                schema.connectors.as_ref(),
            ),
        }
    }

    #[cfg(test)]
    pub(crate) fn empty(schema: Arc<Schema>) -> Self {
        Self {
            http_service_factory: Arc::new(Default::default()),
            subgraph_schemas: Default::default(),
            subscription_config: Default::default(),
            connectors_by_service_name: Default::default(),
            schema,
            _connect_spec_version_instrument: None,
        }
    }
}

impl ServiceFactory<ConnectRequest> for ConnectorServiceFactory {
    type Service = BoxService;

    fn create(&self) -> Self::Service {
        ConnectorService {
            http_service_factory: self.http_service_factory.clone(),
            _schema: self.schema.clone(),
            _subgraph_schemas: self.subgraph_schemas.clone(),
            _subscription_config: self.subscription_config.clone(),
            connectors_by_service_name: self.connectors_by_service_name.clone(),
        }
        .boxed()
    }
}
