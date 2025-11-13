//! Tower service for connectors.

use std::fmt::Display;
use std::str::FromStr;
use std::sync::Arc;
use std::task::Poll;

use apollo_federation::connectors::Connector;
use apollo_federation::connectors::SourceName;
use apollo_federation::connectors::runtime::debug::ConnectorContext;
use futures::future::BoxFuture;
use indexmap::IndexMap;
use opentelemetry::Key;
use opentelemetry::metrics::ObservableGauge;
use parking_lot::Mutex;
use serde::Deserialize;
use serde::Serialize;
use tower::BoxError;
use tower::ServiceExt;
use tracing_futures::Instrument;

use super::connect::BoxService;
use super::new_service::ServiceFactory;
use crate::plugins::connectors::handle_responses::aggregate_responses;
use crate::plugins::connectors::make_requests::make_requests;
use crate::plugins::connectors::tracing::CONNECTOR_TYPE_HTTP;
use crate::plugins::connectors::tracing::connect_spec_version_instrument;
use crate::plugins::subscription::SubscriptionConfig;
use crate::plugins::telemetry::consts::CONNECT_SPAN_NAME;
use crate::query_planner::fetch::SubgraphSchemas;
use crate::services::ConnectRequest;
use crate::services::ConnectResponse;
use crate::services::connector::request_service::ConnectorRequestServiceFactory;
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

/// A service for executing connector requests.
#[derive(Clone)]
pub(crate) struct ConnectorService {
    pub(crate) _schema: Arc<Schema>,
    pub(crate) _subgraph_schemas: Arc<SubgraphSchemas>,
    pub(crate) _subscription_config: Option<SubscriptionConfig>,
    pub(crate) connectors_by_service_name: Arc<IndexMap<Arc<str>, Connector>>,
    pub(crate) connector_request_service_factory: Arc<ConnectorRequestServiceFactory>,
}

/// A reference to a unique Connector source.
#[derive(Hash, Eq, PartialEq, Clone, Serialize, Deserialize)]
pub(crate) struct ConnectorSourceRef {
    pub(crate) subgraph_name: String,
    pub(crate) source_name: SourceName,
}

impl ConnectorSourceRef {
    pub(crate) fn new(subgraph_name: String, source_name: SourceName) -> Self {
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
            .ok_or(format!("Invalid connector source reference '{s}'"))?
            .to_string();
        let source_name = parts
            .next()
            .ok_or(format!("Invalid connector source reference '{s}'"))?;
        Ok(Self::new(subgraph_name, SourceName::cast(source_name)))
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

impl TryFrom<&mut Connector> for ConnectorSourceRef {
    type Error = ();

    fn try_from(value: &mut Connector) -> Result<Self, Self::Error> {
        Ok(Self {
            subgraph_name: value.id.subgraph_name.to_string(),
            source_name: value.id.source_name.clone().ok_or(())?,
        })
    }
}

impl Display for ConnectorSourceRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}", self.subgraph_name, self.source_name)
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

        let connector_request_service_factory = self.connector_request_service_factory.clone();

        Box::pin(async move {
            let Some(connector) = connector else {
                return Err("no connector found".into());
            };

            let fetch_time_offset = request.context.created_at.elapsed().as_nanos() as i64;
            let span = tracing::info_span!(
                CONNECT_SPAN_NAME,
                "otel.kind" = "INTERNAL",
                "apollo.connector.type" = CONNECTOR_TYPE_HTTP,
                "apollo.connector.detail" = tracing::field::Empty,
                "apollo.connector.coordinate" = %connector.id.coordinate(),
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
                span.record("apollo.connector.source.name", source_name.as_str());
                if let Ok(detail) = serde_json::to_string(
                    &serde_json::json!({ "baseURL": transport.source_template.as_ref().map(|uri| uri.to_string()) }),
                ) {
                    span.record("apollo.connector.source.detail", detail);
                }
            }

            execute(&connector_request_service_factory, request, connector)
                .instrument(span)
                .await
        })
    }
}

async fn execute(
    connector_request_service_factory: &ConnectorRequestServiceFactory,
    request: ConnectRequest,
    connector: Connector,
) -> Result<ConnectResponse, BoxError> {
    let context = request.context.clone();
    let connector = Arc::new(connector);
    let source_name = connector.source_config_key();
    let debug = &context
        .extensions()
        .with_lock(|lock| lock.get::<Arc<Mutex<ConnectorContext>>>().cloned());

    let tasks = make_requests(
        &request.operation,
        &request.variables,
        request.keys.as_ref(),
        &context,
        request.supergraph_request.clone(),
        connector,
        debug,
    )
    .map_err(BoxError::from)?
    .into_iter()
    .map(move |request| {
        let source_name = source_name.clone();
        async move {
            connector_request_service_factory
                .create(source_name)
                .oneshot(request)
                .await
        }
    });

    aggregate_responses(
        futures::future::try_join_all(tasks)
            .await
            .map(|responses| {
                responses
                    .into_iter()
                    .map(|response| response.mapped_response)
                    .collect()
            })?,
        context,
    )
    .map_err(BoxError::from)
}

#[derive(Clone)]
pub(crate) struct ConnectorServiceFactory {
    pub(crate) schema: Arc<Schema>,
    pub(crate) subgraph_schemas: Arc<SubgraphSchemas>,
    pub(crate) subscription_config: Option<SubscriptionConfig>,
    pub(crate) connectors_by_service_name: Arc<IndexMap<Arc<str>, Connector>>,
    _connect_spec_version_instrument: Option<ObservableGauge<u64>>,
    pub(crate) connector_request_service_factory: Arc<ConnectorRequestServiceFactory>,
}

impl ConnectorServiceFactory {
    pub(crate) fn new(
        schema: Arc<Schema>,
        subgraph_schemas: Arc<SubgraphSchemas>,
        subscription_config: Option<SubscriptionConfig>,
        connectors_by_service_name: Arc<IndexMap<Arc<str>, Connector>>,
        connector_request_service_factory: Arc<ConnectorRequestServiceFactory>,
    ) -> Self {
        Self {
            subgraph_schemas,
            schema: schema.clone(),
            subscription_config,
            connectors_by_service_name,
            _connect_spec_version_instrument: connect_spec_version_instrument(
                schema.connectors.as_ref(),
            ),
            connector_request_service_factory,
        }
    }

    #[cfg(test)]
    pub(crate) fn empty(schema: Arc<Schema>) -> Self {
        Self::new(
            schema,
            Default::default(),
            Default::default(),
            Default::default(),
            Arc::new(ConnectorRequestServiceFactory::new(
                Default::default(),
                Default::default(),
                Default::default(),
            )),
        )
    }
}

impl ServiceFactory<ConnectRequest> for ConnectorServiceFactory {
    type Service = BoxService;

    fn create(&self) -> Self::Service {
        ConnectorService {
            _schema: self.schema.clone(),
            _subgraph_schemas: self.subgraph_schemas.clone(),
            _subscription_config: self.subscription_config.clone(),
            connectors_by_service_name: self.connectors_by_service_name.clone(),
            connector_request_service_factory: self.connector_request_service_factory.clone(),
        }
        .boxed()
    }
}
