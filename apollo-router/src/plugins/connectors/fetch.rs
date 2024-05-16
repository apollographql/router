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
use apollo_federation::sources::connect::HttpJsonTransport;
use apollo_federation::sources::connect::JSONSelection;
use apollo_federation::sources::connect::SubSelection;
use apollo_federation::sources::source;
use apollo_federation::sources::source::SourceId;
use hyper_rustls::ConfigBuilderExt;
use tower::BoxError;
use tower::ServiceExt;
use tracing::Instrument;

use crate::error::Error;
use crate::error::FetchError;
use crate::graphql::Request;
use crate::http_ext;
use crate::json_ext::Object;
use crate::json_ext::Path;
use crate::json_ext::Value;
use crate::query_planner::fetch::FetchNode;
use crate::query_planner::fetch::Protocol;
use crate::query_planner::fetch::RestFetchNode;
use crate::query_planner::fetch::Variables;
use crate::query_planner::log;
use crate::query_planner::ExecutionParameters;
use crate::services::trust_dns_connector::new_async_http_connector;
use crate::services::SubgraphRequest;

use super::http_json_transport::http_json_transport;
use super::http_json_transport::HttpJsonTransportError;

impl From<FetchNode> for source::query_plan::FetchNode {
    fn from(value: FetchNode) -> source::query_plan::FetchNode {
        let subgraph_name = match value.protocol.as_ref() {
            Protocol::RestFetch(rf) => rf.parent_service_name.clone().into(),
            _ => value.service_name.clone(),
        };
        source::query_plan::FetchNode::Connect(connect::query_plan::FetchNode {
            source_id: ConnectId {
                label: value.service_name.to_string(),
                subgraph_name: subgraph_name,
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

    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn process_source_node<'a>(
        &'a self,
        parameters: &'a ExecutionParameters<'a>,
        data: &'a Value,
        current_dir: &'a Path,
    ) -> (Value, Vec<Error>) {
        let Variables {
            variables,
            inverted_paths: paths,
        } = match Variables::new(
            &self.requires,
            &self.variable_usages,
            data,
            current_dir,
            // Needs the original request here
            parameters.supergraph_request,
            parameters.schema,
            &self.input_rewrites,
        ) {
            Some(variables) => variables,
            None => {
                return (Value::Object(Object::default()), Vec::new());
            }
        };

        if let Some(source_node) = self.source_node.clone() {
            if let source::query_plan::FetchNode::Connect(_c) = source_node.as_ref() {
                // return process_source_node(c, parameters, variables, paths).await;
            }
        }
        let FetchNode {
            operation,
            operation_kind,
            operation_name,
            service_name,
            ..
        } = self;

        let service_name_string = service_name.to_string();

        let (service_name, subgraph_service_name) = match &*self.protocol {
            Protocol::RestFetch(RestFetchNode {
                connector_service_name,
                parent_service_name,
                ..
            }) => (parent_service_name, connector_service_name),
            _ => (&service_name_string, &service_name_string),
        };

        let uri = parameters
            .schema
            .subgraph_url(service_name)
            .unwrap_or_else(|| {
                panic!("schema uri for subgraph '{service_name}' should already have been checked")
            })
            .clone();

        let mut subgraph_request = SubgraphRequest::builder()
            .supergraph_request(parameters.supergraph_request.clone())
            .subgraph_request(
                http_ext::Request::builder()
                    .method(http::Method::POST)
                    .uri(uri)
                    .body(
                        Request::builder()
                            .query(operation.as_serialized())
                            .and_operation_name(operation_name.as_ref().map(|n| n.to_string()))
                            .variables(variables.clone())
                            .build(),
                    )
                    .build()
                    .expect("it won't fail because the url is correct and already checked; qed"),
            )
            .subgraph_name(subgraph_service_name)
            .operation_kind(*operation_kind)
            .context(parameters.context.clone())
            .build();
        subgraph_request.query_hash = self.schema_aware_hash.clone();
        subgraph_request.authorization = self.authorization.clone();

        let service = parameters
            .service_factory
            .create(service_name)
            .expect("we already checked that the service exists during planning; qed");

        let (_parts, response) = match service
            .oneshot(subgraph_request)
            .instrument(tracing::trace_span!("subfetch_stream"))
            .await
            // TODO this is a problem since it restores details about failed service
            // when errors have been redacted in the include_subgraph_errors module.
            // Unfortunately, not easy to fix here, because at this point we don't
            // know if we should be redacting errors for this subgraph...
            .map_err(|e| match e.downcast::<FetchError>() {
                Ok(inner) => match *inner {
                    FetchError::SubrequestHttpError { .. } => *inner,
                    _ => FetchError::SubrequestHttpError {
                        status_code: None,
                        service: service_name.to_string(),
                        reason: inner.to_string(),
                    },
                },
                Err(e) => FetchError::SubrequestHttpError {
                    status_code: None,
                    service: service_name.to_string(),
                    reason: e.to_string(),
                },
            }) {
            Err(e) => {
                return (
                    Value::default(),
                    vec![e.to_graphql_error(Some(current_dir.to_owned()))],
                );
            }
            Ok(res) => res.response.into_parts(),
        };

        log::trace_subfetch(
            service_name,
            operation.as_serialized(),
            &variables,
            &response,
        );

        if !response.is_primary() {
            return (
                Value::default(),
                vec![FetchError::SubrequestUnexpectedPatchResponse {
                    service: service_name.to_string(),
                }
                .to_graphql_error(Some(current_dir.to_owned()))],
            );
        }

        let (value, errors) =
            self.response_at_path(parameters.schema, current_dir, paths, response);
        if let Some(id) = &self.id {
            if let Some(sender) = parameters.deferred_fetches.get(id.as_str()) {
                tracing::info!(monotonic_counter.apollo.router.operations.defer.fetch = 1u64);
                if let Err(e) = sender.clone().send((value.clone(), errors.clone())) {
                    tracing::error!("error sending fetch result at path {} and id {:?} for deferred response building: {}", current_dir, self.id, e);
                }
            }
        }
        (value, errors)
    }
}

#[allow(dead_code)]
async fn process_source_node<'a>(
    source_node: &'a ConnectFetchNode,
    execution_parameters: &'a ExecutionParameters<'a>,
    data: Object,
    paths: Vec<Vec<Path>>,
) -> (Value, Vec<Error>) {
    let connector = execution_parameters
        .connectors
        // TODO: not elegant at all
        .get(&SourceId::Connect(source_node.source_id.clone()))
        .unwrap();

    // TODO: this as well
    let requests = create_requests(connector, &data).unwrap();

    let responses = make_requests(requests).await.unwrap();

    process_responses(responses, source_node).await
}

fn create_requests(
    connector: &Connector,
    data: &Object,
) -> Result<Vec<http::Request<hyper::Body>>, HttpJsonTransportError> {
    match &connector.transport {
        connect::Transport::HttpJson(json_transport) => {
            Ok(vec![create_request(json_transport, data)?])
        }
    }
}

fn create_request(
    json_transport: &HttpJsonTransport,
    data: &Object,
) -> Result<http::Request<hyper::Body>, HttpJsonTransportError> {
    http_json_transport::make_request(
        json_transport,
        serde_json_bytes::Value::Object(data.clone()),
    )
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
    use crate::query_planner::fetch::SubgraphOperation;
    use crate::query_planner::PlanNode;
    use crate::services::OperationKind;
    use crate::services::SubgraphServiceFactory;
    use crate::spec::Query;
    use crate::spec::Schema;

    const SCHEMA: &str = include_str!("./test_supergraph.graphql");

    #[tokio::test]
    async fn test_process_source_node() {
        let mock_server = MockServer::start().await;
        mock_api::mount_all(&mock_server).await;
        mock_subgraph::start_join().mount(&mock_server).await;

        let context = Default::default();
        let service_factory = Arc::new(SubgraphServiceFactory::empty());
        let schema = Arc::new(Schema::parse_test(SCHEMA, &Default::default()).unwrap());
        let supergraph_request = Default::default();
        let deferred_fetches = Default::default();
        let query = Arc::new(Query::empty());

        let fetch_node = FetchNode {
            service_name: NodeStr::new("kitchen-sink.a: GET /hello"),
            requires: Default::default(),
            variable_usages: Default::default(),
            operation: SubgraphOperation::from_string("{hello{__typename id}}"),
            operation_name: Default::default(),
            operation_kind: OperationKind::Query,
            id: Default::default(),
            input_rewrites: Default::default(),
            output_rewrites: Default::default(),
            schema_aware_hash: Default::default(),
            authorization: Default::default(),
            protocol: Arc::new(Protocol::RestFetch(RestFetchNode {
                connector_service_name: "CONNECTOR_QUERY_HELLO_6".to_string(),
                connector_graph_key: Arc::new("CONNECTOR_QUERY_HELLO_6".to_string()),
                parent_service_name: "kitchen-sink".to_string(),
            })),
            source_node: Some(Arc::new(source::query_plan::FetchNode::Connect(
                fake_source_node(),
            ))),
        };
        let root_node = PlanNode::Fetch(fetch_node);

        let subscription_handle = Default::default();
        let subscription_config = Default::default();

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

        let execution_parameters = ExecutionParameters {
            context: &context,
            service_factory: &service_factory,
            schema: &schema,
            supergraph_request: &supergraph_request,
            deferred_fetches: &deferred_fetches,
            query: &query,
            root_node: &root_node,
            subscription_handle: &subscription_handle,
            subscription_config: &subscription_config,
            connectors: &Arc::new(connectors),
        };
        let source_node = fake_source_node();
        let data = Default::default();
        let paths = Default::default();

        let expected = (json!({ "data": { "id": 42 } }), Default::default());

        let actual = process_source_node(&source_node, &execution_parameters, data, paths).await;

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
