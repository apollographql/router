//! Tower fetcher for fetch node execution.

use std::collections::HashMap;
use std::sync::Arc;
use std::task::Poll;

use apollo_compiler::validation::Valid;
use apollo_federation::sources::connect;
use apollo_federation::sources::connect::query_plan::FetchNode;
use apollo_federation::sources::connect::ApplyTo;
use apollo_federation::sources::connect::Connector;
use apollo_federation::sources::connect::Connectors;
use apollo_federation::sources::connect::HttpJsonTransport;
use apollo_federation::sources::source::SourceId;
use futures::future::BoxFuture;
use hyper_rustls::ConfigBuilderExt;
use tower::BoxError;
use tower::ServiceExt;

use super::connect::BoxService;
use super::new_service::ServiceFactory;
use super::trust_dns_connector::new_async_http_connector;
use super::SubgraphRequest;
use crate::graphql::Error;
use crate::graphql::Request as GraphQLRequest;
use crate::http_ext;
use crate::json_ext::Object;
use crate::json_ext::Path;
use crate::json_ext::Value;
use crate::plugins::connectors::http_json_transport::http_json_transport;
use crate::plugins::connectors::http_json_transport::HttpJsonTransportError;
use crate::plugins::subscription::SubscriptionConfig;
use crate::query_planner::build_operation_with_aliasing;
use crate::query_planner::fetch::Protocol;
use crate::query_planner::fetch::RestFetchNode;
use crate::query_planner::fetch::Variables;
use crate::services::ConnectRequest;
use crate::services::ConnectResponse;
use crate::spec::Schema;

#[derive(Clone)]
pub(crate) struct ConnectorService {
    pub(crate) http_service_factory: Arc<()>, // TODO: HTTP SERVICE
    pub(crate) schema: Arc<Schema>,
    pub(crate) subgraph_schemas: Arc<HashMap<String, Arc<Valid<apollo_compiler::Schema>>>>,
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
            supergraph_request,
            data,
            current_dir,
            context,
        } = request;

        let connectors = self.connectors.clone();

        Box::pin(
            async move { Ok(process_source_node(fetch_node, connectors, data, current_dir).await) },
        )
    }
}

pub(crate) async fn process_source_node(
    source_node: FetchNode,
    connectors: Connectors,
    data: Value,
    _paths: Path,
) -> (Value, Vec<Error>) {
    let connector = if let Some(connector) =
        connectors.get(&SourceId::Connect(source_node.source_id.clone()))
    {
        connector
    } else {
        return (Default::default(), Default::default());
    };

    // TODO: remove unwraps
    let requests = create_requests(connector, &data).unwrap();

    let responses = make_requests(requests).await.unwrap();

    process_responses(responses, source_node).await
}

fn create_requests(
    connector: &Connector,
    data: &Value,
) -> Result<Vec<http::Request<hyper::Body>>, HttpJsonTransportError> {
    match &connector.transport {
        connect::Transport::HttpJson(json_transport) => {
            Ok(vec![create_request(json_transport, data)?])
        }
    }
}

fn create_request(
    json_transport: &HttpJsonTransport,
    data: &Value,
) -> Result<http::Request<hyper::Body>, HttpJsonTransportError> {
    http_json_transport::make_request(json_transport, data.clone())
}

async fn make_requests(
    requests: Vec<http::Request<hyper::Body>>,
) -> Result<Vec<http::Response<hyper::Body>>, BoxError> {
    let mut http_connector = new_async_http_connector()?;
    http_connector.set_nodelay(true);
    http_connector.set_keepalive(Some(std::time::Duration::from_secs(60)));
    http_connector.enforce_http(false);

    let tls_config = rustls::ClientConfig::builder()
        .with_safe_defaults()
        .with_native_roots()
        .with_no_client_auth();

    let http_connector = hyper_rustls::HttpsConnectorBuilder::new()
        .with_tls_config(tls_config)
        .https_or_http()
        .enable_http1()
        .enable_http2()
        .wrap_connector(http_connector);
    //TODO: add decompression
    let client = hyper::Client::builder().build(http_connector);

    let tasks = requests.into_iter().map(|req| {
        let client = client.clone();
        async move { client.request(req).await }
    });
    futures::future::try_join_all(tasks)
        .await
        .map_err(BoxError::from)
}

async fn process_responses(
    responses: Vec<http::Response<hyper::Body>>,
    source_node: FetchNode,
) -> (Value, Vec<Error>) {
    let mut data = serde_json_bytes::Map::new();
    let errors = Vec::new();

    for response in responses {
        let (parts, body) = response.into_parts();

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
    pub(crate) http_service_factory: Arc<()>,
    pub(crate) subscription_config: Option<SubscriptionConfig>,
    pub(crate) connectors: Connectors,
}

impl ConnectorServiceFactory {
    pub(crate) fn new(
        schema: Arc<Schema>,
        subgraph_schemas: Arc<HashMap<String, Arc<Valid<apollo_compiler::Schema>>>>,
        http_service_factory: Arc<()>,
        subscription_config: Option<SubscriptionConfig>,
        connectors: Connectors,
    ) -> Self {
        Self {
            http_service_factory: Arc::new(()),
            subgraph_schemas,
            schema,
            subscription_config,
            connectors,
        }
    }

    #[cfg(test)]
    pub(crate) fn empty(schema: Arc<Schema>) -> Self {
        Self {
            http_service_factory: Arc::new(()),
            subgraph_schemas: Default::default(),
            subscription_config: Default::default(),
            connectors: Default::default(),
            schema
        }
    }
}

impl ServiceFactory<ConnectRequest> for ConnectorServiceFactory {
    type Service = BoxService;

    fn create(&self) -> Self::Service {
        ConnectorService {
            http_service_factory: Arc::new(()),
            schema: self.schema.clone(),
            subgraph_schemas: self.subgraph_schemas.clone(),
            _subscription_config: self.subscription_config.clone(),
            connectors: self.connectors.clone(),
        }
        .boxed()
    }
}

#[cfg(test)]
mod soure_node_tests {
    use apollo_compiler::NodeStr;
    use apollo_federation::schema::ObjectOrInterfaceFieldDefinitionPosition;
    use apollo_federation::schema::ObjectOrInterfaceFieldDirectivePosition;
    use apollo_federation::sources::connect::HTTPMethod;
    use apollo_federation::sources::connect::HttpJsonTransport;
    use apollo_federation::sources::connect::Transport;
    use apollo_federation::sources::connect::URLPathTemplate;
    use indexmap::IndexMap;
    use serde_json_bytes::json;
    use wiremock::MockServer;

    use super::*;
    use crate::plugins::connectors::tests::mock_api;
    use crate::plugins::connectors::tests::mock_subgraph;

    use apollo_compiler::name;
    use apollo_federation::schema::ObjectFieldDefinitionPosition;
    use apollo_federation::sources::connect;
    use apollo_federation::sources::connect::ConnectId;
    use apollo_federation::sources::connect::JSONSelection;
    use apollo_federation::sources::connect::SubSelection;
    use apollo_federation::sources::source;

    #[tokio::test]
    async fn test_process_source_node() {
        let mock_server = MockServer::start().await;
        mock_api::mount_all(&mock_server).await;
        mock_subgraph::start_join().mount(&mock_server).await;

        let mut connectors: IndexMap<SourceId, Connector> = Default::default();
        let id = fake_connect_id();
        connectors.insert(
            SourceId::Connect(id.clone()),
            Connector {
                id,
                transport: Transport::HttpJson(HttpJsonTransport {
                    base_url: NodeStr::new(&format!("{}/v1", mock_server.uri())),
                    path_template: URLPathTemplate::parse("/hello").unwrap(),
                    method: HTTPMethod::Get,
                    headers: Default::default(),
                    body: Default::default(),
                }),
                selection: JSONSelection::parse(".data { id }").unwrap().1,
            },
        );

        let connectors = Arc::new(connectors);

        let source_node = fake_source_node();
        let data = Default::default();
        let paths = Default::default();

        let expected = (json!({ "data": { "id": 42 } }), Default::default());

        let actual = process_source_node(source_node, connectors, data, paths).await;

        assert_eq!(expected, actual);
    }

    fn fake_source_node() -> FetchNode {
        FetchNode {
            source_id: fake_connect_id(),
            field_response_name: name!("data"),
            field_arguments: Default::default(),
            selection: JSONSelection::parse(".data { id }").unwrap().1,
        }
    }

    fn fake_connect_id() -> ConnectId {
        ConnectId {
            label: "kitchen-sink.a: GET /hello".to_string(),
            subgraph_name: "kitchen-sink".into(),
            directive: ObjectOrInterfaceFieldDirectivePosition {
                field: ObjectOrInterfaceFieldDefinitionPosition::Object(
                    ObjectFieldDefinitionPosition {
                        type_name: name!("Query"),
                        field_name: name!("hello"),
                    },
                ),
                directive_name: name!("sourceField"),
                directive_index: 0,
            },
        }
    }
}
