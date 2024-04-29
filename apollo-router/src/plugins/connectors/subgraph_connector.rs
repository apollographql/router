use std::collections::HashMap;
use std::sync::Arc;
use std::task::Poll;

use apollo_compiler::validation::Valid;
use apollo_compiler::Schema;
use futures::future::BoxFuture;
use hyper::client::HttpConnector;
use hyper_rustls::ConfigBuilderExt;
use hyper_rustls::HttpsConnector;
use tower::BoxError;
use tracing::Instrument;
use tracing::Span;

use super::configuration::SourceApiConfiguration;
use super::request_response::handle_responses;
use super::request_response::make_requests;
use super::request_response::HandleResponseError;
use super::request_response::MakeRequestError;
use super::Connector;
use crate::plugins::telemetry::LOGGING_DISPLAY_BODY;
use crate::plugins::telemetry::LOGGING_DISPLAY_HEADERS;
use crate::plugins::telemetry::OTEL_STATUS_CODE;
use crate::services::trust_dns_connector::new_async_http_connector;
use crate::services::trust_dns_connector::AsyncHyperResolver;
use crate::services::SubgraphRequest;
use crate::services::SubgraphResponse;

static CONNECTOR_FETCH: &str = "connector fetch";
static CONNECTOR_HTTP_REQUEST: &str = "connector http request";

#[derive(Clone)]
pub(crate) struct SubgraphConnector {
    http_connectors: HashMap<Arc<String>, HTTPConnector>,
}

impl SubgraphConnector {
    pub(crate) fn for_schema(
        schema: Arc<Valid<Schema>>,
        configuration: Option<&HashMap<String, SourceApiConfiguration>>,
        connectors: HashMap<Arc<String>, &Connector>,
    ) -> Result<Self, BoxError> {
        let http_connectors = connectors
            .into_iter()
            .map(|(name, connector)| {
                HTTPConnector::new(
                    schema.clone(),
                    connector.clone(),
                    configuration.and_then(|c| c.get(&connector.api)),
                )
                .map(|connector| (name, connector))
            })
            .collect::<Result<HashMap<_, _>, _>>()?;
        Ok(Self { http_connectors })
    }
}

impl tower::Service<SubgraphRequest> for SubgraphConnector {
    type Response = SubgraphResponse;
    type Error = BoxError;
    type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn poll_ready(&mut self, _cx: &mut std::task::Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: SubgraphRequest) -> Self::Future {
        let http_connector = request
            .subgraph_name
            .as_ref()
            .and_then(|name| self.http_connectors.get(name))
            .cloned();

        Box::pin(async move {
            match http_connector {
                Some(service) => {
                    let res = service.call(request).await?;
                    Ok(res)
                }
                None => Err(BoxError::from("HTTP connector not found")),
            }
        })
    }
}

/// INNER (CONNECTOR) SUBGRAPH SERVICE

#[derive(Clone)]
pub(crate) struct HTTPConnector {
    schema: Arc<Valid<Schema>>,
    connector: Connector,
    client: hyper::Client<HttpsConnector<HttpConnector<AsyncHyperResolver>>>,
}

impl HTTPConnector {
    pub(crate) fn new(
        schema: Arc<Valid<Schema>>,
        mut connector: Connector,
        configuration: Option<&SourceApiConfiguration>,
    ) -> Result<Self, BoxError> {
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

        if let Some(url) = configuration.and_then(|c| c.override_url.clone()) {
            connector.override_base_url(url);
        }

        Ok(Self {
            schema,
            connector,
            client,
        })
    }
}

impl HTTPConnector {
    async fn call(&self, request: SubgraphRequest) -> Result<SubgraphResponse, BoxError> {
        let schema = self.schema.clone();
        let connector = self.connector.clone();
        let context = request.context.clone();
        let client = self.client.clone();
        let document = request.subgraph_request.body().query.clone();

        let display_body = request
            .context
            .get(LOGGING_DISPLAY_BODY)
            .unwrap_or_default()
            .unwrap_or_default();
        let display_headers = request
            .context
            .get(LOGGING_DISPLAY_HEADERS)
            .unwrap_or_default()
            .unwrap_or_default();
        let connector_name = connector.display_name();

        let connector_fetch_span = tracing::info_span!(
            CONNECTOR_FETCH,
            "connector.name" = %connector_name,
        );

        let requests = make_requests(request, &connector, schema.clone())
            .map_err(|e| make_requests_error(&connector, e))?;

        let tasks = requests.into_iter().map(|(req, res_params)| {
            let url = req.uri().clone();
            let method = req.method().clone();
            let url_str = url.to_string();

            let http_request_span = tracing::info_span!(
                CONNECTOR_HTTP_REQUEST,
                "connector.name" = %connector_name,
                "url.full" = %url_str,
                "http.request.method" = method.to_string(),
                "otel.kind" = "CLIENT",
                "otel.status_code" = ::tracing::field::Empty,
                "http.response.status_code" = ::tracing::field::Empty,
            );

            if display_headers {
                tracing::info!(http.request.headers = ?req.headers(), url.full = ?url_str, method = ?req.method().to_string(), "Request headers sent to REST endpoint");
            }
            if display_body {
                tracing::info!(http.request.body = ?req.body(), url.full = ?url_str, method = ?req.method().to_string(), "Request body sent to REST endpoint");
            }

            async {
                let span = Span::current();
                let mut res = match client.request(req).await {
                    Ok(res) => {
                        span.record(OTEL_STATUS_CODE, "Ok");
                        span.record("http.response.status_code", res.status().as_u16());
                        Ok(res)
                    }
                    e => {
                        span.record(OTEL_STATUS_CODE, "Error");
                        e
                    }
                }?;
                let extensions = res.extensions_mut();
                extensions.insert(res_params);  extensions.insert(url);
                extensions.insert(method);
                Ok::<_, BoxError>(res)
            }
            .instrument(http_request_span)
        });

        let results = futures::future::try_join_all(tasks)
            .instrument(connector_fetch_span)
            .await;

        let responses = match results {
            Ok(responses) => responses,
            Err(e) => return Err(BoxError::from(e)),
        };

        let mut response_diagnostics = Vec::new();
        let subgraph_response = handle_responses(
            &schema,
            document,
            context,
            &connector,
            responses,
            &mut response_diagnostics,
        )
        .await
        .map_err(|e| handle_responses_error(&connector, e))?;

        for diagnostic in response_diagnostics {
            diagnostic.log();
        }

        Ok(subgraph_response)
    }
}

fn make_requests_error(connector: &Connector, err: MakeRequestError) -> BoxError {
    format!(
        "Failed to create requests for connector `{}`: {}",
        connector.display_name(),
        err
    )
    .into()
}

fn handle_responses_error(connector: &Connector, err: HandleResponseError) -> BoxError {
    format!(
        "Failed to map responses for connector `{}`: {}",
        connector.display_name(),
        err
    )
    .into()
}

#[cfg(test)]
mod tests {
    use std::convert::Infallible;
    use std::net::SocketAddr;
    use std::net::TcpListener;
    use std::sync::Arc;

    use http::header::CONTENT_TYPE;
    use http::Method;
    use http::StatusCode;
    use hyper::service::make_service_fn;
    use hyper::service::service_fn;
    use hyper::Body;
    use hyper::Server;
    use mime::APPLICATION_JSON;
    use tower::ServiceExt;

    use crate::metrics::FutureMetricsExt;
    use crate::router_factory::YamlRouterFactory;
    use crate::services::new_service::ServiceFactory;
    use crate::services::supergraph;

    const SCHEMA: &str = include_str!("../../../tests/fixtures/connectors/supergraph.graphql");

    async fn emulate_rest_connector(listener: TcpListener) {
        async fn handle(
            request: http::Request<Body>,
        ) -> Result<http::Response<String>, Infallible> {
            /*
            type IP
              @join__type(graph: NETWORK, key: "ip")
              @sourceType(
                graph: "network"
                api: "ipinfo"
                http: { GET: "/json" }
                selection: "ip hostname"
              ) {
              ip: ID!
              hostname: String
            }
            */

            let res = if request.method() == Method::GET
            /*&& request.uri().path() == "/json"*/
            {
                let value = serde_json::json! {{
                    "ip": "1.2.3.4",
                    "hostname": "hello"
                }};
                Ok(http::Response::builder()
                    .header(CONTENT_TYPE, APPLICATION_JSON.essence_str())
                    .status(StatusCode::OK)
                    .body(serde_json::to_string(&value).expect("always valid"))
                    .unwrap())
            } else {
                Ok(http::Response::builder()
                    .header(CONTENT_TYPE, APPLICATION_JSON.essence_str())
                    .status(StatusCode::BAD_REQUEST)
                    .body(String::new())
                    .unwrap())
            };
            tracing::debug!("generated service response: {res:?}");
            res
        }

        let make_svc = make_service_fn(|_conn| async { Ok::<_, Infallible>(service_fn(handle)) });
        let server = Server::from_tcp(listener).unwrap().serve(make_svc);
        server.await.unwrap();
    }

    #[tokio::test]
    async fn nullability_formatting() {
        let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0))).unwrap();
        let address = listener.local_addr().unwrap();
        let _spawned_task = tokio::task::spawn(emulate_rest_connector(listener));

        // we cannot use Testharness because the subgraph connectors are actually extracted in YamlRouterFactory
        let mut factory = YamlRouterFactory;
        use crate::router_factory::RouterSuperServiceFactory;
        let router_creator = factory
            .create(
                false,
                Arc::new(
                    serde_json::from_value(serde_json::json!({
                        "include_subgraph_errors": { "all": true },
                        "preview_connectors": {
                            "subgraphs": {
                                "network": {
                                    "ipinfo": {
                                      "override_url": format!("http://127.0.0.1:{}/", address.port())
                                    }
                                }
                            }
                        }
                    }))
                    .unwrap(),
                ),
                SCHEMA.to_string(),
                None,
                None,
            )
            .await
            .unwrap();
        let service = router_creator.create();

        let request = supergraph::Request::fake_builder()
            .query("query { serverNetworkInfo { ip hostname } }")
            // Request building here
            .build()
            .unwrap()
            .try_into()
            .unwrap();

        async {
            let response = service
                .oneshot(request)
                .await
                .unwrap()
                .next_response()
                .await
                .unwrap()
                .unwrap();
            let response: serde_json::Value = serde_json::from_slice(&response).unwrap();

            insta::assert_json_snapshot!(response);

            assert_counter!(
                "apollo.router.operations.source.rest",
                1,
                "rest.response.api" = "ipinfo",
                "rest.response.status_code" = 200
            );
        }
        .with_metrics()
        .await;
    }
}
