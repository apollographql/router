//! Externalization plugin
// With regards to ELv2 licensing, this entire file is license key functionality

use std::collections::HashMap;
use std::ops::ControlFlow;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use http::header::HeaderName;
use http::HeaderMap;
use http::HeaderValue;
use hyper::client::HttpConnector;
use hyper::Body;
use hyper_rustls::ConfigBuilderExt;
use hyper_rustls::HttpsConnector;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use tower::timeout::TimeoutLayer;
use tower::util::MapFutureLayer;
use tower::BoxError;
use tower::Service;
use tower::ServiceBuilder;
use tower::ServiceExt;

use crate::error::Error;
use crate::error::LicenseError;
use crate::layers::async_checkpoint::AsyncCheckpointLayer;
use crate::layers::ServiceBuilderExt;
use crate::plugin::Plugin;
use crate::plugin::PluginInit;
use crate::register_plugin;
use crate::services::apollo_graph_reference;
use crate::services::execution;
use crate::services::external::Control;
use crate::services::external::Externalizable;
use crate::services::external::PipelineStep;
use crate::services::external::DEFAULT_EXTERNALIZATION_TIMEOUT;
use crate::services::external::EXTERNALIZABLE_VERSION;
use crate::services::router;
use crate::services::subgraph;
use crate::services::supergraph;
use crate::tracer::TraceId;

pub(crate) const EXTERNAL_SPAN_NAME: &str = "external_plugin";

type HTTPClientService = tower::timeout::Timeout<hyper::Client<HttpsConnector<HttpConnector>>>;

#[async_trait::async_trait]
impl Plugin for ExternalPlugin<HTTPClientService> {
    type Config = Conf;

    async fn new(init: PluginInit<Self::Config>) -> Result<Self, BoxError> {
        if init.config.stages.is_some() {
            apollo_graph_reference().ok_or(LicenseError::MissingGraphReference)?;
        }

        let mut http_connector = HttpConnector::new();
        http_connector.set_nodelay(true);
        http_connector.set_keepalive(Some(std::time::Duration::from_secs(60)));
        http_connector.enforce_http(false);

        // todo: grab tls config from configuration
        let tls_config = rustls::ClientConfig::builder()
            .with_safe_defaults()
            .with_native_roots()
            .with_no_client_auth();

        let connector = hyper_rustls::HttpsConnectorBuilder::new()
            .with_tls_config(tls_config)
            .https_or_http()
            .enable_http1()
            .enable_http2()
            .wrap_connector(http_connector);

        let http_client = ServiceBuilder::new()
            .layer(TimeoutLayer::new(
                init.config
                    .timeout
                    .unwrap_or(DEFAULT_EXTERNALIZATION_TIMEOUT),
            ))
            .service(hyper::Client::builder().build(connector));

        ExternalPlugin::new(http_client, init.config, init.supergraph_sdl)
    }

    fn router_service(&self, service: router::BoxService) -> router::BoxService {
        self.router_service(service)
    }

    fn supergraph_service(&self, service: supergraph::BoxService) -> supergraph::BoxService {
        self.supergraph_service(service)
    }

    fn execution_service(&self, service: execution::BoxService) -> execution::BoxService {
        self.execution_service(service)
    }

    fn subgraph_service(&self, name: &str, service: subgraph::BoxService) -> subgraph::BoxService {
        self.subgraph_service(name, service)
    }
}

// This macro allows us to use it in our plugin registry!
// register_plugin takes a group name, and a plugin name.
//
// In order to keep the plugin names consistent,
// we use using the `Reverse domain name notation`
register_plugin!(
    "experimental",
    "external",
    ExternalPlugin<HTTPClientService>
);

// -------------------------------------------------------------------------------------------------------

/// This is where the real implementation happens.
/// The structure above calls the functions defined below.
///
/// This structure is generic over the HTTP Service so we can test the plugin seamlessly.
#[derive(Debug)]
struct ExternalPlugin<C>
where
    C: Service<hyper::Request<Body>, Response = hyper::Response<Body>, Error = BoxError>
        + Clone
        + Send
        + Sync
        + 'static,
    <C as tower::Service<http::Request<hyper::Body>>>::Future: Send + Sync + 'static,
{
    http_client: C,
    configuration: Conf,
    sdl: Arc<String>,
}

impl<C> ExternalPlugin<C>
where
    C: Service<hyper::Request<Body>, Response = hyper::Response<Body>, Error = BoxError>
        + Clone
        + Send
        + Sync
        + 'static,
    <C as tower::Service<http::Request<hyper::Body>>>::Future: Send + Sync + 'static,
{
    fn new(http_client: C, configuration: Conf, sdl: Arc<String>) -> Result<Self, BoxError> {
        Ok(Self {
            http_client,
            configuration,
            sdl,
        })
    }

    fn router_service(&self, service: router::BoxService) -> router::BoxService {
        if let Some(router_stage) = self
            .configuration
            .stages
            .as_ref()
            .and_then(|stages| stages.router.as_ref())
        {
            router_stage.as_service(
                self.http_client.clone(),
                service,
                self.configuration.url.clone(),
                self.sdl.clone(),
            )
        } else {
            service
        }
    }

    fn supergraph_service(&self, service: supergraph::BoxService) -> supergraph::BoxService {
        service
    }

    fn execution_service(&self, service: execution::BoxService) -> execution::BoxService {
        service
    }

    fn subgraph_service(&self, _name: &str, service: subgraph::BoxService) -> subgraph::BoxService {
        if let Some(subgraph_stage) = self
            .configuration
            .stages
            .as_ref()
            .and_then(|stages| stages.subgraph.as_ref())
        {
            subgraph_stage.as_service(
                self.http_client.clone(),
                service,
                self.configuration.url.clone(),
            )
        } else {
            service
        }
    }
}
/// What information is passed to a router request/response stage
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize, JsonSchema)]
#[serde(default)]
struct RouterConf {
    /// Send the headers
    headers: bool,
    /// Send the context
    context: bool,
    /// Send the body
    body: bool,
    /// Send the SDL
    sdl: bool,
}

/// What information is passed to a subgraph request/response stage
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize, JsonSchema)]
#[serde(default)]
struct SubgraphConf {
    /// Send the headers
    headers: bool,
    /// Send the context
    context: bool,
    /// Send the body
    body: bool,
    /// Send the service name
    service: bool,
    /// Send the subgraph URL
    url: bool,
}

/// The stages request/response configuration
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize, JsonSchema)]
#[serde(default)]
struct Stages {
    /// The router stage
    router: Option<RouterStage>,
    /// The subgraph stage
    subgraph: Option<SubgraphStage>,
}

/// Configures the externalization plugin
#[derive(Clone, Debug, Default, Deserialize, JsonSchema)]
struct Conf {
    /// The url you'd like to offload processing to
    url: String,
    /// The timeout for external requests
    #[serde(deserialize_with = "humantime_serde::deserialize", default)]
    #[schemars(with = "String", default)]
    timeout: Option<Duration>,
    /// The stages request/response configuration
    #[serde(default)]
    stages: Option<Stages>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize, JsonSchema)]
#[serde(default)]
struct RouterStage {
    /// The request configuration
    request: Option<RouterConf>,
    /// The response configuration
    response: Option<RouterConf>,
}

impl RouterStage {
    pub(crate) fn as_service<C>(
        &self,
        http_client: C,
        service: router::BoxService,
        // TODO: put it where relevant
        coprocessor_url: String,
        sdl: Arc<String>,
    ) -> router::BoxService
    where
        C: Service<hyper::Request<Body>, Response = hyper::Response<Body>, Error = BoxError>
            + Clone
            + Send
            + Sync
            + 'static,
        <C as tower::Service<http::Request<hyper::Body>>>::Future: Send + 'static,
    {
        let request_layer = self.request.clone().map(|request_config| {
            let coprocessor_url = coprocessor_url.clone();
            let http_client = http_client.clone();
            let sdl = sdl.clone();

            AsyncCheckpointLayer::new(move |mut request: router::Request| {
                let request_config = request_config.clone();
                let coprocessor_url = coprocessor_url.clone();
                let http_client = http_client.clone();
                let sdl = sdl.clone();

                async move {
                    // Call into our out of process processor with a body of our body
                    // First, extract the data we need from our request and prepare our
                    // external call. Use our configuration to figure out which data to send.
                    let (parts, body) = request.router_request.into_parts();
                    let bytes = hyper::body::to_bytes(body).await?;

                    let headers_to_send = request_config
                        .headers
                        .then(|| externalize_header_map(&parts.headers))
                        .transpose()?;
                    // TODO: why is it a serde_json::Value here ?
                    // is it because the request and the response should be the same?
                    let body_to_send = request_config
                        .body
                        .then(|| serde_json::from_slice::<serde_json::Value>(&bytes))
                        .transpose()?;
                    let context_to_send = request_config.context.then(|| request.context.clone());
                    let sdl = request_config.sdl.then(|| sdl.clone().to_string());

                    let payload = Externalizable {
                        version: EXTERNALIZABLE_VERSION,
                        stage: PipelineStep::RouterRequest.to_string(),
                        control: Control::default(),
                        id: TraceId::maybe_new().map(|id| id.to_string()),
                        headers: headers_to_send,
                        body: body_to_send,
                        context: context_to_send,
                        sdl,
                        uri: None,
                    };

                    tracing::debug!(?payload, "externalized output");
                    request.context.enter_active_request().await;
                    let co_processor_result = payload.call(http_client, &coprocessor_url).await;
                    request.context.leave_active_request().await;
                    tracing::debug!(?co_processor_result, "co-processor returned");
                    let co_processor_output = co_processor_result?;

                    // Thirdly, we need to interpret the control flow which may have been
                    // updated by our co-processor and decide if we should proceed or stop.

                    if matches!(co_processor_output.control, Control::Break(_)) {
                        // Ensure the code is a valid http status code
                        let code = co_processor_output.control.get_http_status()?;

                        let res = if !code.is_success() {
                            router::Response::error_builder()
                                .errors(vec![Error {
                                    message: co_processor_output
                                        .body
                                        .unwrap_or(serde_json::Value::Null)
                                        .to_string(),
                                    ..Default::default()
                                }])
                                .status_code(code)
                                .context(request.context)
                                .build()?
                        } else {
                            router::Response::builder()
                                .data(
                                    co_processor_output
                                        .body
                                        .unwrap_or(serde_json::Value::Null)
                                        .to_string(),
                                )
                                .status_code(code)
                                .context(request.context)
                                .build()?
                        };
                        return Ok(ControlFlow::Break(res));
                    }

                    // Finally, process our reply and act on the contents. Our processing logic is
                    // that we replace "bits" of our incoming request with the updated bits if they
                    // are present in our co_processor_output.

                    let new_body = match co_processor_output.body {
                        Some(bytes) => Body::from(serde_json::to_vec(&bytes)?),
                        None => Body::from(bytes),
                    };

                    request.router_request = http::Request::from_parts(parts, new_body);

                    if let Some(context) = co_processor_output.context {
                        request.context = context;
                    }

                    if let Some(headers) = co_processor_output.headers {
                        *request.router_request.headers_mut() = internalize_header_map(headers)?;
                    }

                    Ok(ControlFlow::Continue(request))
                }
            })
        });

        let response_layer = self.response.clone().map(|response_config| {
            MapFutureLayer::new(move |fut| {
                let sdl = sdl.clone();
                let coprocessor_url = coprocessor_url.clone();
                let http_client = http_client.clone();
                async move {
                    let mut response: router::Response = fut.await?;

                    // Call into our out of process processor with a body of our body
                    // First, extract the data we need from our response and prepare our
                    // external call. Use our configuration to figure out which data to send.
                    let (parts, body) = response.response.into_parts();
                    let bytes = hyper::body::to_bytes(body).await?;

                    let headers_to_send = response_config
                        .headers
                        .then(|| externalize_header_map(&parts.headers))
                        .transpose()?;
                    let body_to_send = response_config
                        .body
                        .then(|| serde_json::from_slice::<serde_json::Value>(&bytes))
                        .transpose()?;
                    let context_to_send = response_config.context.then(|| response.context.clone());
                    let sdl = response_config.sdl.then(|| sdl.clone().to_string());

                    // TODO: why is it a serde_json::Value here ?
                    // is it because the request and the response should be the same?
                    let payload = Externalizable {
                        version: EXTERNALIZABLE_VERSION,
                        stage: PipelineStep::RouterResponse.to_string(),
                        control: Control::default(),
                        id: TraceId::maybe_new().map(|id| id.to_string()),
                        headers: headers_to_send,
                        body: body_to_send,
                        context: context_to_send,
                        sdl,
                        uri: None,
                    };

                    // Second, call our co-processor and get a reply.
                    tracing::debug!(?payload, "externalized output");
                    response.context.enter_active_request().await;
                    let co_processor_result = payload.call(http_client, &coprocessor_url).await;
                    response.context.leave_active_request().await;
                    tracing::debug!(?co_processor_result, "co-processor returned");
                    let co_processor_output = co_processor_result?;

                    // Third, process our reply and act on the contents. Our processing logic is
                    // that we replace "bits" of our incoming response with the updated bits if they
                    // are present in our co_processor_output. If they aren't present, just use the
                    // bits that we sent to the co_processor.

                    let new_body = match co_processor_output.body {
                        Some(bytes) => Body::from(serde_json::to_vec(&bytes)?),
                        None => Body::from(bytes),
                    };

                    response.response = http::Response::from_parts(parts, new_body);

                    if let Some(context) = co_processor_output.context {
                        response.context = context;
                    }

                    if let Some(headers) = co_processor_output.headers {
                        *response.response.headers_mut() = internalize_header_map(headers)?;
                    }

                    Ok::<router::Response, BoxError>(response)
                }
            })
        });

        fn external_service_span() -> impl Fn(&router::Request) -> tracing::Span + Clone {
            move |_request: &router::Request| {
                tracing::info_span!(
                    EXTERNAL_SPAN_NAME,
                    "external service" = stringify!(router::Request),
                    "otel.kind" = "INTERNAL"
                )
            }
        }

        ServiceBuilder::new()
            .instrument(external_service_span())
            .option_layer(request_layer)
            .option_layer(response_layer)
            .buffered()
            .service(service)
            .boxed()
    }
}

// -----------------------------------------------------------------------------------------

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize, JsonSchema)]
struct SubgraphStage {
    #[serde(default)]
    request: Option<SubgraphConf>,
    #[serde(default)]
    response: Option<SubgraphConf>,
}

impl SubgraphStage {
    pub(crate) fn as_service<C>(
        &self,
        http_client: C,
        service: subgraph::BoxService,
        // TODO: put it where relevant
        coprocessor_url: String,
    ) -> subgraph::BoxService
    where
        C: Service<hyper::Request<Body>, Response = hyper::Response<Body>, Error = BoxError>
            + Clone
            + Send
            + Sync
            + 'static,
        <C as tower::Service<http::Request<hyper::Body>>>::Future: Send + Sync + 'static,
    {
        let request_layer = self.request.clone().map(|request_config| {
            let http_client = http_client.clone();
            let coprocessor_url = coprocessor_url.clone();
            AsyncCheckpointLayer::new(move |mut request: subgraph::Request| {
                let http_client = http_client.clone();
                let coprocessor_url = coprocessor_url.clone();

                async move {
                    // Call into our out of process processor with a body of our body
                    // First, extract the data we need from our request and prepare our
                    // external call. Use our configuration to figure out which data to send.
                    let (parts, body) = request.subgraph_request.into_parts();
                    let bytes = Bytes::from(serde_json::to_vec(&body)?);

                    let headers_to_send = request_config
                        .headers
                        .then(|| externalize_header_map(&parts.headers))
                        .transpose()?;
                    // TODO: why is it a serde_json::Value here ?
                    // is it because the request and the response should be the same?
                    let body_to_send = request_config
                        .body
                        .then(|| serde_json::from_slice::<serde_json::Value>(&bytes))
                        .transpose()?;
                    let context_to_send = request_config.context.then(|| request.context.clone());
                    let uri = request_config.url.then(|| parts.uri.to_string());

                    let payload = Externalizable {
                        version: EXTERNALIZABLE_VERSION,
                        stage: PipelineStep::SubgraphRequest.to_string(),
                        control: Control::default(),
                        id: TraceId::maybe_new().map(|id| id.to_string()),
                        headers: headers_to_send,
                        body: body_to_send,
                        context: context_to_send,
                        sdl: None,
                        uri,
                    };

                    tracing::debug!(?payload, "externalized output");
                    request.context.enter_active_request().await;
                    let co_processor_result = payload.call(http_client, &coprocessor_url).await;
                    request.context.leave_active_request().await;
                    tracing::debug!(?co_processor_result, "co-processor returned");
                    let co_processor_output = co_processor_result?;

                    // Thirdly, we need to interpret the control flow which may have been
                    // updated by our co-processor and decide if we should proceed or stop.

                    if matches!(co_processor_output.control, Control::Break(_)) {
                        // Ensure the code is a valid http status code
                        let code = co_processor_output.control.get_http_status()?;

                        let res = if !code.is_success() {
                            subgraph::Response::error_builder()
                                .errors(vec![Error {
                                    message: co_processor_output
                                        .body
                                        .unwrap_or(serde_json::Value::Null)
                                        .to_string(),
                                    ..Default::default()
                                }])
                                .status_code(code)
                                .context(request.context)
                                .build()?
                        } else {
                            let graphql_response: crate::graphql::Response =
                                serde_json::from_value(
                                    co_processor_output.body.unwrap_or(serde_json::Value::Null),
                                )
                                .unwrap(); //todo
                            subgraph::Response {
                                response: http::Response::builder()
                                    .status(code)
                                    .body(graphql_response)?,
                                context: request.context,
                            }
                        };
                        return Ok(ControlFlow::Break(res));
                    }

                    // Finally, process our reply and act on the contents. Our processing logic is
                    // that we replace "bits" of our incoming request with the updated bits if they
                    // are present in our co_processor_output.

                    let new_body: crate::graphql::Request = match co_processor_output.body {
                        Some(value) => serde_json::from_value(value)?,
                        None => body,
                    };

                    request.subgraph_request = http::Request::from_parts(parts, new_body);

                    if let Some(context) = co_processor_output.context {
                        request.context = context;
                    }

                    if let Some(headers) = co_processor_output.headers {
                        *request.subgraph_request.headers_mut() = internalize_header_map(headers)?;
                    }

                    if let Some(uri) = co_processor_output.uri {
                        *request.subgraph_request.uri_mut() = uri.parse()?;
                    }

                    Ok(ControlFlow::Continue(request))
                }
            })
        });

        let response_layer = self.response.clone().map(|response_config| {
            let http_client = http_client.clone();
            MapFutureLayer::new(move |fut| {
                let http_client = http_client.clone();
                let coprocessor_url = coprocessor_url.clone();
                let response_config = response_config.clone();

                async move {
                    let mut response: subgraph::Response = fut.await?;

                    // Call into our out of process processor with a body of our body
                    // First, extract the data we need from our response and prepare our
                    // external call. Use our configuration to figure out which data to send.

                    let (parts, body) = response.response.into_parts();
                    let bytes = Bytes::from(serde_json::to_vec(&body)?);

                    let headers_to_send = response_config
                        .headers
                        .then(|| externalize_header_map(&parts.headers))
                        .transpose()?;
                    // TODO: why is it a serde_json::Value here ?
                    // is it because the request and the response should be the same?
                    let body_to_send = response_config
                        .body
                        .then(|| serde_json::from_slice::<serde_json::Value>(&bytes))
                        .transpose()?;
                    let context_to_send = response_config.context.then(|| response.context.clone());

                    let payload = Externalizable {
                        version: EXTERNALIZABLE_VERSION,
                        stage: PipelineStep::SubgraphResponse.to_string(),
                        control: Control::default(),
                        id: TraceId::maybe_new().map(|id| id.to_string()),
                        headers: headers_to_send,
                        body: body_to_send,
                        context: context_to_send,
                        sdl: None,
                        uri: None,
                    };

                    tracing::debug!(?payload, "externalized output");
                    response.context.enter_active_request().await;
                    let co_processor_result = payload.call(http_client, &coprocessor_url).await;
                    response.context.leave_active_request().await;
                    tracing::debug!(?co_processor_result, "co-processor returned");
                    let co_processor_output = co_processor_result?;

                    // Third, process our reply and act on the contents. Our processing logic is
                    // that we replace "bits" of our incoming response with the updated bits if they
                    // are present in our co_processor_output. If they aren't present, just use the
                    // bits that we sent to the co_processor.

                    let new_body: crate::graphql::Response = match co_processor_output.body {
                        Some(value) => serde_json::from_value(value)?,
                        None => body,
                    };

                    response.response = http::Response::from_parts(parts, new_body);

                    if let Some(context) = co_processor_output.context {
                        response.context = context;
                    }

                    if let Some(headers) = co_processor_output.headers {
                        *response.response.headers_mut() = internalize_header_map(headers)?;
                    }

                    Ok::<subgraph::Response, BoxError>(response)
                }
            })
        });

        fn external_service_span() -> impl Fn(&subgraph::Request) -> tracing::Span + Clone {
            move |_request: &subgraph::Request| {
                tracing::info_span!(
                    EXTERNAL_SPAN_NAME,
                    "external service" = stringify!(subgraph::Request),
                    "otel.kind" = "INTERNAL"
                )
            }
        }

        ServiceBuilder::new()
            .instrument(external_service_span())
            .option_layer(request_layer)
            .option_layer(response_layer)
            .buffer(20_000)
            .service(service)
            .boxed()
    }
}

// -----------------------------------------------------------------------------------------------------

/// Convert a HeaderMap into a HashMap
fn externalize_header_map(
    input: &HeaderMap<HeaderValue>,
) -> Result<HashMap<String, Vec<String>>, BoxError> {
    let mut output = HashMap::new();
    for (k, v) in input {
        let k = k.as_str().to_owned();
        let v = String::from_utf8(v.as_bytes().to_vec()).map_err(|e| e.to_string())?;
        output.entry(k).or_insert_with(Vec::new).push(v)
    }
    Ok(output)
}

/// Convert a HashMap into a HeaderMap
fn internalize_header_map(
    input: HashMap<String, Vec<String>>,
) -> Result<HeaderMap<HeaderValue>, BoxError> {
    let mut output = HeaderMap::new();
    for (k, values) in input {
        for v in values {
            let key = HeaderName::from_str(k.as_ref())?;
            let value = HeaderValue::from_str(v.as_ref())?;
            output.append(key, value);
        }
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use crate::plugin::test::{MockHttpClientService, MockRouterService};
    use http::header::ACCEPT;
    use http::header::CONTENT_TYPE;
    use http::HeaderMap;
    use http::HeaderValue;
    use mime::APPLICATION_JSON;
    use mime::TEXT_HTML;

    use super::*;

    #[tokio::test]
    async fn load_plugin() {
        let config = serde_json::json!({
            "plugins": {
                "experimental.external": {
                    "url": "http://127.0.0.1:8081"
                }
            }
        });
        // Build a test harness. Usually we'd use this and send requests to
        // it, but in this case it's enough to build the harness to see our
        // output when our service registers.
        let _test_harness = crate::TestHarness::builder()
            .configuration_json(config)
            .unwrap()
            .build_router()
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn external_plugin_with_stages_wont_load_without_graph_ref() {
        let config = serde_json::json!({
            "plugins": {
                "experimental.external": {
                    "url": "http://127.0.0.1:8081",
                    "stages": {
                        "router": {}
                    },
                }
            }
        });
        // Build a test harness. Usually we'd use this and send requests to
        // it, but in this case it's enough to build the harness to see our
        // output when our service registers.
        assert!(crate::TestHarness::builder()
            .configuration_json(config)
            .unwrap()
            .build_router()
            .await
            .is_err());
    }

    #[tokio::test]
    async fn external_plugin_router_request() {
        let router_stage = RouterStage {
            request: None,
            response: None,
        };

        let mock_http_client = MockHttpClientService::new();

        let mock_service = MockRouterService::new().boxed();

        let service = router_stage.as_service(
            mock_http_client,
            mock_service,
            "http://test".to_string(),
            Arc::new("".to_string()),
        );
    }

    #[test]
    fn it_externalizes_headers() {
        // Build our expected HashMap
        let mut expected = HashMap::new();

        expected.insert(
            "content-type".to_string(),
            vec![APPLICATION_JSON.essence_str().to_string()],
        );

        expected.insert(
            "accept".to_string(),
            vec![
                APPLICATION_JSON.essence_str().to_string(),
                TEXT_HTML.essence_str().to_string(),
            ],
        );

        let mut external_form = HeaderMap::new();

        external_form.insert(
            CONTENT_TYPE,
            HeaderValue::from_static(APPLICATION_JSON.essence_str()),
        );

        external_form.insert(
            ACCEPT,
            HeaderValue::from_static(APPLICATION_JSON.essence_str()),
        );

        external_form.append(ACCEPT, HeaderValue::from_static(TEXT_HTML.essence_str()));

        let actual = externalize_header_map(&external_form).expect("externalized header map");

        assert_eq!(expected, actual);
    }

    #[test]
    fn it_internalizes_headers() {
        // Build our expected HeaderMap
        let mut expected = HeaderMap::new();

        expected.insert(
            ACCEPT,
            HeaderValue::from_static(APPLICATION_JSON.essence_str()),
        );

        expected.append(ACCEPT, HeaderValue::from_static(TEXT_HTML.essence_str()));

        let mut external_form = HashMap::new();

        external_form.insert(
            "accept".to_string(),
            vec![
                APPLICATION_JSON.essence_str().to_string(),
                TEXT_HTML.essence_str().to_string(),
            ],
        );

        let actual = internalize_header_map(external_form).expect("internalized header map");

        assert_eq!(expected, actual);
    }
}
