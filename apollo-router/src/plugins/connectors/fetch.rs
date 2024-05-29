use std::collections::HashMap;
use std::sync::Arc;

use apollo_compiler::name;
use apollo_federation::schema::ObjectFieldDefinitionPosition;
use apollo_federation::schema::ObjectOrInterfaceFieldDefinitionPosition;
use apollo_federation::schema::ObjectOrInterfaceFieldDirectivePosition;
use apollo_federation::sources::connect;
use apollo_federation::sources::connect::query_plan::FetchNode as ConnectFetchNode;
use apollo_federation::sources::connect::ApplyTo;
use apollo_federation::sources::connect::ConnectId;
use apollo_federation::sources::connect::Connector;
use apollo_federation::sources::connect::Connectors;
use apollo_federation::sources::connect::HttpJsonTransport;
use apollo_federation::sources::connect::JSONSelection;
use apollo_federation::sources::connect::SubSelection;
use apollo_federation::sources::source;
use apollo_federation::sources::source::SourceId;
use hyper_rustls::ConfigBuilderExt;
use tower::BoxError;

use super::http_json_transport::http_json_transport;
use super::http_json_transport::HttpJsonTransportError;
use crate::error::Error;
use crate::json_ext::Path;
use crate::json_ext::Value;
use crate::query_planner::fetch::FetchNode;
use crate::query_planner::fetch::Protocol;
use crate::query_planner::fetch::RestFetchNode;
use crate::services::trust_dns_connector::new_async_http_connector;

impl From<FetchNode> for source::query_plan::FetchNode {
    fn from(value: FetchNode) -> source::query_plan::FetchNode {
        let subgraph_name = match value.protocol.as_ref() {
            Protocol::RestFetch(rf) => rf.parent_service_name.clone().into(),
            _ => value.service_name.clone(),
        };
        source::query_plan::FetchNode::Connect(connect::query_plan::FetchNode {
            source_id: ConnectId {
                label: value.service_name.to_string(),
                subgraph_name,
                directive: ObjectOrInterfaceFieldDirectivePosition {
                    field: ObjectOrInterfaceFieldDefinitionPosition::Object(
                        ObjectFieldDefinitionPosition {
                            type_name: name!("TypeName"),
                            field_name: name!("field_name"),
                        },
                    ),
                    directive_name: name!("Directive__name"),
                    directive_index: 0,
                },
            },
            field_response_name: name!("Field"),
            field_arguments: Default::default(),
            selection: JSONSelection::Named(SubSelection {
                selections: vec![],
                star: None,
            }),
        })
    }
}

impl FetchNode {
    // TODO: let's go all in on nodestr
    pub(crate) fn update_connector_plan(
        &mut self,
        parent_service_name: &String,
        connectors: &Arc<HashMap<Arc<String>, super::Connector>>,
    ) {
        let parent_service_name = parent_service_name.to_string();
        let connector = connectors.get(&self.service_name.to_string()).unwrap();
        let service_name =
            std::mem::replace(&mut self.service_name, connector.display_name().into());
        self.protocol = Arc::new(Protocol::RestFetch(RestFetchNode {
            connector_service_name: service_name.to_string(),
            connector_graph_key: connector._name(),
            parent_service_name,
        }));
        let as_fednext_node: source::query_plan::FetchNode = self.clone().into();
        self.source_node = Some(Arc::new(as_fednext_node));
    }
}

pub(crate) async fn process_source_node(
    source_node: &ConnectFetchNode,
    connectors: Connectors,
    data: Value,
    _paths: Vec<Vec<Path>>,
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
    source_node: &ConnectFetchNode,
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
                        data.insert(source_node.field_response_name.to_string(), d);
                    }
                });
            }
        });
    }

    (serde_json_bytes::Value::Object(data), errors)
}

#[cfg(test)]
mod soure_node_tests {
    use apollo_compiler::NodeStr;
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

        let actual = process_source_node(&source_node, connectors, data, paths).await;

        assert_eq!(expected, actual);
    }

    fn fake_source_node() -> ConnectFetchNode {
        ConnectFetchNode {
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
