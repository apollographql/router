//! Tower fetcher for fetch node execution.
use std::collections::HashMap;
use std::sync::Arc;
use std::task::Poll;

use apollo_compiler::validation::Valid;
use apollo_federation::sources::connect::Connector;
use apollo_federation::sources::connect::Transport;
use futures::future::BoxFuture;
use indexmap::IndexMap;
use opentelemetry::Key;
use tower::BoxError;
use tower::ServiceExt;
use tracing::Instrument;

use super::connect::BoxService;
use super::http::HttpClientServiceFactory;
use super::http::HttpRequest;
use super::new_service::ServiceFactory;
use crate::plugins::connectors::handle_responses::handle_responses;
use crate::plugins::connectors::make_requests::make_requests;
use crate::plugins::connectors::plugin::ConnectorContext;
use crate::plugins::connectors::tracing::CONNECTOR_TYPE_HTTP;
use crate::plugins::connectors::tracing::CONNECT_SPAN_NAME;
use crate::plugins::subscription::SubscriptionConfig;
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

#[derive(Clone)]
pub(crate) struct ConnectorService {
    pub(crate) http_service_factory: Arc<IndexMap<String, HttpClientServiceFactory>>,
    pub(crate) schema: Arc<Schema>,
    pub(crate) _subgraph_schemas: Arc<HashMap<String, Arc<Valid<apollo_compiler::Schema>>>>,
    pub(crate) _subscription_config: Option<SubscriptionConfig>,
    pub(crate) connectors_by_service_name: Arc<IndexMap<Arc<str>, Connector>>,
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

        let schema = self.schema.supergraph_schema().clone();

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
                "apollo.connector.field.name" = connector.field_name().to_string(),
                "apollo.connector.selection" = connector.selection.to_string(),
                "apollo.connector.source.name" = tracing::field::Empty,
                "apollo.connector.source.detail" = tracing::field::Empty,
                "apollo_private.sent_time_offset" = fetch_time_offset,
            );
            // TODO: apollo.connector.field.alias
            // TODO: apollo.connector.field.return_type
            // TODO: apollo.connector.field.selection_set
            let Transport::HttpJson(ref http_json) = connector.transport;
            if let Ok(detail) = serde_json::to_string(
                &serde_json::json!({ http_json.method.as_str(): http_json.path_template.to_string() }),
            ) {
                span.record("apollo.connector.detail", detail);
            }
            if let Some(source_name) = connector.id.source_name.as_ref() {
                span.record("apollo.connector.source.name", source_name);
                if let Ok(detail) =
                    serde_json::to_string(&serde_json::json!({ "baseURL": http_json.base_url }))
                {
                    span.record("apollo.connector.source.detail", detail);
                }
            }

            execute(&http_client_factory, request, &connector, &schema)
                .instrument(span)
                .await
        })
    }
}

async fn execute(
    http_client_factory: &HttpClientServiceFactory,
    request: ConnectRequest,
    connector: &Connector,
    schema: &Valid<apollo_compiler::Schema>,
) -> Result<ConnectResponse, BoxError> {
    let context = request.context.clone();
    let context2 = context.clone();
    let original_subgraph_name = connector.id.subgraph_name.to_string();

    let mut debug = context
        .extensions()
        .with_lock(|mut lock| lock.remove::<ConnectorContext>());

    let requests = make_requests(request, connector, &mut debug).map_err(BoxError::from)?;

    let tasks = requests.into_iter().map(move |(req, key)| {
        let context = context.clone();
        let original_subgraph_name = original_subgraph_name.clone();
        async move {
            let context = context.clone();

            let client = http_client_factory.create(&original_subgraph_name);
            let req = HttpRequest {
                http_request: req,
                context,
            };
            let res = client.oneshot(req).await?;
            let mut res = res.http_response;
            let extensions = res.extensions_mut();
            extensions.insert(key);

            Ok::<_, BoxError>(res)
        }
    });

    let responses = futures::future::try_join_all(tasks)
        .await
        .map_err(BoxError::from)?;

    let result = handle_responses(responses, connector, &mut debug, schema)
        .await
        .map_err(BoxError::from);

    if let Some(debug) = debug {
        context2
            .extensions()
            .with_lock(|mut lock| lock.insert::<ConnectorContext>(debug));
    }

    result
}

#[derive(Clone)]
pub(crate) struct ConnectorServiceFactory {
    pub(crate) schema: Arc<Schema>,
    pub(crate) subgraph_schemas: Arc<HashMap<String, Arc<Valid<apollo_compiler::Schema>>>>,
    pub(crate) http_service_factory: Arc<IndexMap<String, HttpClientServiceFactory>>,
    pub(crate) subscription_config: Option<SubscriptionConfig>,
    pub(crate) connectors_by_service_name: Arc<IndexMap<Arc<str>, Connector>>,
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
            schema,
            subscription_config,
            connectors_by_service_name,
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
        }
    }
}

impl ServiceFactory<ConnectRequest> for ConnectorServiceFactory {
    type Service = BoxService;

    fn create(&self) -> Self::Service {
        ConnectorService {
            http_service_factory: self.http_service_factory.clone(),
            schema: self.schema.clone(),
            _subgraph_schemas: self.subgraph_schemas.clone(),
            _subscription_config: self.subscription_config.clone(),
            connectors_by_service_name: self.connectors_by_service_name.clone(),
        }
        .boxed()
    }
}
