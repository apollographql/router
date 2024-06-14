//! Tower fetcher for fetch node execution.

use std::collections::HashMap;
use std::sync::Arc;
use std::task::Poll;

use apollo_compiler::validation::Valid;
use apollo_federation::sources::connect;
use apollo_federation::sources::connect::ApplyTo;
use apollo_federation::sources::connect::Connector;
use apollo_federation::sources::connect::Connectors;
use apollo_federation::sources::connect::HttpJsonTransport;
use apollo_federation::sources::to_remove::connect::FetchNode;
use apollo_federation::sources::to_remove::SourceId;
use futures::future::BoxFuture;
use indexmap::IndexMap;
use tower::BoxError;
use tower::ServiceExt;

use super::connect::BoxService;
use super::http::HttpClientServiceFactory;
use super::http::HttpRequest;
use super::http::HttpResponse;
use super::new_service::ServiceFactory;
use crate::graphql::Error;
use crate::json_ext::Path;
use crate::json_ext::Value;
use crate::plugins::connectors::http_json_transport::HttpJsonTransportError;
use crate::plugins::subscription::SubscriptionConfig;
use crate::query_planner::fetch::Variables;
use crate::services::ConnectRequest;
use crate::services::ConnectResponse;
use crate::spec::Schema;
use crate::Context;

#[derive(Clone)]
pub(crate) struct ConnectorService {
    pub(crate) http_service_factory: Arc<IndexMap<String, HttpClientServiceFactory>>,
    pub(crate) _schema: Arc<Schema>,
    pub(crate) _subgraph_schemas: Arc<HashMap<String, Arc<Valid<apollo_compiler::Schema>>>>,
    pub(crate) _subscription_config: Option<SubscriptionConfig>,
    pub(crate) connectors: Connectors,
}

impl tower::Service<ConnectRequest> for ConnectorService {
    type Response = ConnectResponse;
    type Error = BoxError;
    type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn poll_ready(&mut self, _cx: &mut std::task::Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: ConnectRequest) -> Self::Future {
        let ConnectRequest {
            fetch_node,
            variables,
            current_dir,
            context,
            ..
        } = request;

        let connectors = self.connectors.clone();

        // TODO: unwrap
        let sf = self
            .http_service_factory
            .get(fetch_node.source_id.subgraph_name.as_str())
            .unwrap()
            .clone();
        Box::pin(async move {
            Ok(
                process_source_node(sf, fetch_node, connectors, variables, current_dir, context)
                    .await,
            )
        })
    }
}

pub(crate) async fn process_source_node(
    http_client: HttpClientServiceFactory,
    source_node: FetchNode,
    connectors: Connectors,
    variables: Variables,
    _paths: Path,
    context: Context,
) -> (Value, Vec<Error>) {
    let connector = if let Some(connector) =
        connectors.get(&SourceId::Connect(source_node.source_id.clone()))
    {
        connector
    } else {
        return (Default::default(), Default::default());
    };

    // TODO: remove unwraps
    let requests = create_requests(connector, variables, context).unwrap();

    let responses = make_requests(
        http_client,
        source_node.source_id.subgraph_name.as_str(),
        requests,
    )
    .await
    .unwrap();

    process_responses(responses, source_node).await
}

fn create_requests(
    connector: &Connector,
    variables: Variables,
    context: Context,
) -> Result<Vec<HttpRequest>, HttpJsonTransportError> {
    match &connector.transport {
        connect::Transport::HttpJson(json_transport) => {
            Ok(create_request(json_transport, variables, context.clone())?)
        }
    }
}

#[allow(unreachable_code)]
fn create_request(
    _json_transport: &HttpJsonTransport,
    _variables: Variables,
    context: Context,
) -> Result<Vec<HttpRequest>, HttpJsonTransportError> {
    Ok(vec![HttpRequest {
        context,
        http_request: todo!(),
    }])
}

async fn make_requests(
    http_client: HttpClientServiceFactory,
    subgraph_name: &str,
    requests: Vec<HttpRequest>,
) -> Result<Vec<HttpResponse>, BoxError> {
    let tasks = requests.into_iter().map(|req| {
        let client = http_client.create(subgraph_name);
        async move { client.oneshot(req).await }
    });
    futures::future::try_join_all(tasks)
        .await
        .map_err(BoxError::from)
}

async fn process_responses(
    responses: Vec<HttpResponse>,
    source_node: FetchNode,
) -> (Value, Vec<Error>) {
    let mut data = serde_json_bytes::Map::new();
    let errors = Vec::new();

    for response in responses {
        let (parts, body) = response.http_response.into_parts();

        let _ = hyper::body::to_bytes(body).await.map(|body| {
            if parts.status.is_success() {
                let _ = serde_json::from_slice(&body).map(|d: Value| {
                    let (d, _apply_to_error) = source_node.selection.apply_to(&d);

                    // todo: errors
                    if let Some(d) = d {
                        // todo: use json_ext to merge stuff
                        data.insert(source_node.field_response_name.to_string(), d);
                    }
                });
            }
        });
    }

    (serde_json_bytes::Value::Object(data), errors)
}

#[derive(Clone)]
pub(crate) struct ConnectorServiceFactory {
    pub(crate) schema: Arc<Schema>,
    pub(crate) subgraph_schemas: Arc<HashMap<String, Arc<Valid<apollo_compiler::Schema>>>>,
    pub(crate) http_service_factory: Arc<IndexMap<String, HttpClientServiceFactory>>,
    pub(crate) subscription_config: Option<SubscriptionConfig>,
    pub(crate) connectors: Connectors,
}

impl ConnectorServiceFactory {
    pub(crate) fn new(
        schema: Arc<Schema>,
        subgraph_schemas: Arc<HashMap<String, Arc<Valid<apollo_compiler::Schema>>>>,
        http_service_factory: Arc<IndexMap<String, HttpClientServiceFactory>>,
        subscription_config: Option<SubscriptionConfig>,
        connectors: Connectors,
    ) -> Self {
        Self {
            http_service_factory,
            subgraph_schemas,
            schema,
            subscription_config,
            connectors,
        }
    }

    #[cfg(test)]
    pub(crate) fn empty(schema: Arc<Schema>) -> Self {
        Self {
            http_service_factory: Arc::new(Default::default()),
            subgraph_schemas: Default::default(),
            subscription_config: Default::default(),
            connectors: Default::default(),
            schema,
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
            connectors: self.connectors.clone(),
        }
        .boxed()
    }
}
