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
        if init.config.stages != Default::default() {
            apollo_graph_reference().ok_or(LicenseError::MissingGraphReference)?;
        }

        let mut http_connector = HttpConnector::new();
        http_connector.set_nodelay(true);
        http_connector.set_keepalive(Some(std::time::Duration::from_secs(60)));
        http_connector.enforce_http(false);

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
            .layer(TimeoutLayer::new(init.config.timeout))
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
        self.configuration.stages.router.as_service(
            self.http_client.clone(),
            service,
            self.configuration.url.clone(),
            self.sdl.clone(),
        )
    }

    fn supergraph_service(&self, service: supergraph::BoxService) -> supergraph::BoxService {
        service
    }

    fn execution_service(&self, service: execution::BoxService) -> execution::BoxService {
        service
    }

    fn subgraph_service(&self, name: &str, service: subgraph::BoxService) -> subgraph::BoxService {
        self.configuration.stages.subgraph.as_service(
            self.http_client.clone(),
            service,
            self.configuration.url.clone(),
            name.to_string(),
        )
    }
}
/// What information is passed to a router request/response stage
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
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
#[serde(default, deny_unknown_fields)]
struct SubgraphConf {
    /// Send the headers
    headers: bool,
    /// Send the context
    context: bool,
    /// Send the body
    body: bool,
    /// Send the service name
    service: bool,
    /// Send the subgraph URI
    uri: bool,
    /// Send the service name
    service_name: bool,
}

/// The stages request/response configuration
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
struct Stages {
    /// The router stage
    router: RouterStage,
    /// The subgraph stage
    subgraph: SubgraphStage,
}

/// Configures the externalization plugin
#[derive(Clone, Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct Conf {
    /// The url you'd like to offload processing to
    url: String,
    /// The timeout for external requests
    #[serde(deserialize_with = "humantime_serde::deserialize")]
    #[schemars(with = "String", default = "default_timeout")]
    #[serde(default = "default_timeout")]
    timeout: Duration,
    /// The stages request/response configuration
    #[serde(default)]
    stages: Stages,
}

fn default_timeout() -> Duration {
    DEFAULT_EXTERNALIZATION_TIMEOUT
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize, JsonSchema)]
#[serde(default)]
struct RouterStage {
    /// The request configuration
    request: RouterConf,
    /// The response configuration
    response: RouterConf,
}

impl RouterStage {
    pub(crate) fn as_service<C>(
        &self,
        http_client: C,
        service: router::BoxService,
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
        let request_layer = (self.request != Default::default()).then_some({
            let request_config = self.request.clone();
            let coprocessor_url = coprocessor_url.clone();
            let http_client = http_client.clone();
            let sdl = sdl.clone();

            AsyncCheckpointLayer::new(move |request: router::Request| {
                let request_config = request_config.clone();
                let coprocessor_url = coprocessor_url.clone();
                let http_client = http_client.clone();
                let sdl = sdl.clone();

                async move {
                    process_router_request_stage(
                        http_client,
                        coprocessor_url,
                        sdl,
                        request,
                        request_config,
                    )
                    .await
                    .map_err(|error| {
                        tracing::error!(
                            "external extensibility: router request stage error: {error}"
                        );
                        error
                    })
                }
            })
        });

        let response_layer = (self.response != Default::default()).then_some({
            let response_config = self.response.clone();
            MapFutureLayer::new(move |fut| {
                let sdl = sdl.clone();
                let coprocessor_url = coprocessor_url.clone();
                let http_client = http_client.clone();
                let response_config = response_config.clone();

                async move {
                    let response: router::Response = fut.await?;

                    process_router_response_stage(
                        http_client,
                        coprocessor_url,
                        sdl,
                        response,
                        response_config,
                    )
                    .await
                    .map_err(|error| {
                        tracing::error!(
                            "external extensibility: router response stage error: {error}"
                        );
                        error
                    })
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
    request: SubgraphConf,
    #[serde(default)]
    response: SubgraphConf,
}

impl SubgraphStage {
    pub(crate) fn as_service<C>(
        &self,
        http_client: C,
        service: subgraph::BoxService,
        coprocessor_url: String,
        service_name: String,
    ) -> subgraph::BoxService
    where
        C: Service<hyper::Request<Body>, Response = hyper::Response<Body>, Error = BoxError>
            + Clone
            + Send
            + Sync
            + 'static,
        <C as tower::Service<http::Request<hyper::Body>>>::Future: Send + 'static,
    {
        let request_layer = (self.request != Default::default()).then_some({
            let request_config = self.request.clone();
            let http_client = http_client.clone();
            let coprocessor_url = coprocessor_url.clone();
            let service_name = service_name.clone();
            AsyncCheckpointLayer::new(move |request: subgraph::Request| {
                let http_client = http_client.clone();
                let coprocessor_url = coprocessor_url.clone();
                let service_name = service_name.clone();
                let request_config = request_config.clone();

                async move {
                    process_subgraph_request_stage(
                        http_client,
                        coprocessor_url,
                        service_name,
                        request,
                        request_config,
                    )
                    .await
                    .map_err(|error| {
                        tracing::error!(
                            "external extensibility: subgraph request stage error: {error}"
                        );
                        error
                    })
                }
            })
        });

        let response_layer = (self.response != Default::default()).then_some({
            let response_config = self.response.clone();

            MapFutureLayer::new(move |fut| {
                let http_client = http_client.clone();
                let coprocessor_url = coprocessor_url.clone();
                let response_config = response_config.clone();
                let service_name = service_name.clone();

                async move {
                    let response: subgraph::Response = fut.await?;

                    process_subgraph_response_stage(
                        http_client,
                        coprocessor_url,
                        service_name,
                        response,
                        response_config,
                    )
                    .await
                    .map_err(|error| {
                        tracing::error!(
                            "external extensibility: subgraph response stage error: {error}"
                        );
                        error
                    })
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
            .buffered()
            .service(service)
            .boxed()
    }
}

// -----------------------------------------------------------------------------------------
async fn process_router_request_stage<C>(
    http_client: C,
    coprocessor_url: String,
    sdl: Arc<String>,
    mut request: router::Request,
    request_config: RouterConf,
) -> Result<ControlFlow<router::Response, router::Request>, BoxError>
where
    C: Service<hyper::Request<Body>, Response = hyper::Response<Body>, Error = BoxError>
        + Clone
        + Send
        + Sync
        + 'static,
    <C as tower::Service<http::Request<hyper::Body>>>::Future: Send + 'static,
{
    // Call into our out of process processor with a body of our body
    // First, extract the data we need from our request and prepare our
    // external call. Use our configuration to figure out which data to send.
    let (parts, body) = request.router_request.into_parts();
    let bytes = hyper::body::to_bytes(body).await?;

    let headers_to_send = request_config
        .headers
        .then(|| externalize_header_map(&parts.headers))
        .transpose()?;

    let body_to_send = request_config
        .body
        .then(|| serde_json::from_slice::<serde_json::Value>(&bytes))
        .transpose()?;
    let context_to_send = request_config.context.then(|| request.context.clone());
    let sdl = request_config.sdl.then(|| sdl.clone().to_string());

    let payload = Externalizable {
        version: EXTERNALIZABLE_VERSION,
        stage: PipelineStep::RouterRequest.to_string(),
        control: Some(Control::default()),
        id: TraceId::maybe_new().map(|id| id.to_string()),
        headers: headers_to_send,
        body: body_to_send,
        context: context_to_send,
        sdl,
        uri: None,
        service_name: None,
    };

    tracing::debug!(?payload, "externalized output");
    request.context.enter_active_request().await;
    let co_processor_result = payload.call(http_client, &coprocessor_url).await;
    request.context.leave_active_request().await;
    tracing::debug!(?co_processor_result, "co-processor returned");
    let co_processor_output = co_processor_result?;

    validate_coprocessor_output(&co_processor_output, PipelineStep::RouterRequest)?;
    // unwrap is safe here because validate_coprocessor_output made sure control is available
    let control = co_processor_output.control.expect("validated above; qed");

    // Thirdly, we need to interpret the control flow which may have been
    // updated by our co-processor and decide if we should proceed or stop.

    if matches!(control, Control::Break(_)) {
        // Ensure the code is a valid http status code
        let code = control.get_http_status()?;

        let graphql_response: crate::graphql::Response =
            serde_json::from_value(co_processor_output.body.unwrap_or(serde_json::Value::Null))
                .unwrap_or_else(|error| {
                    crate::graphql::Response::builder()
                        .errors(vec![Error::builder()
                            .message(format!(
                                "couldn't deserialize coprocessor output body: {error}"
                            ))
                            .extension_code("EXERNAL_DESERIALIZATION_ERROR")
                            .build()])
                        .build()
                });

        let res = router::Response::builder()
            .errors(graphql_response.errors)
            .extensions(graphql_response.extensions)
            .status_code(code)
            .context(request.context);

        let res = match (graphql_response.label, graphql_response.data) {
            (Some(label), Some(data)) => res.label(label).data(data).build()?,
            (Some(label), None) => res.label(label).build()?,
            (None, Some(data)) => res.data(data).build()?,
            (None, None) => res.build()?,
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

async fn process_router_response_stage<C>(
    http_client: C,
    coprocessor_url: String,
    sdl: Arc<String>,
    mut response: router::Response,
    response_config: RouterConf,
) -> Result<router::Response, BoxError>
where
    C: Service<hyper::Request<Body>, Response = hyper::Response<Body>, Error = BoxError>
        + Clone
        + Send
        + Sync
        + 'static,
    <C as tower::Service<http::Request<hyper::Body>>>::Future: Send + 'static,
{
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

    let payload = Externalizable {
        version: EXTERNALIZABLE_VERSION,
        stage: PipelineStep::RouterResponse.to_string(),
        control: None,
        id: TraceId::maybe_new().map(|id| id.to_string()),
        headers: headers_to_send,
        body: body_to_send,
        context: context_to_send,
        sdl,
        uri: None,
        service_name: None,
    };

    // Second, call our co-processor and get a reply.
    tracing::debug!(?payload, "externalized output");
    response.context.enter_active_request().await;
    let co_processor_result = payload.call(http_client, &coprocessor_url).await;
    response.context.leave_active_request().await;
    tracing::debug!(?co_processor_result, "co-processor returned");
    let co_processor_output = co_processor_result?;

    validate_coprocessor_output(&co_processor_output, PipelineStep::RouterResponse)?;

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

    Ok(response)
}
// -----------------------------------------------------------------------------------------------------

async fn process_subgraph_request_stage<C>(
    http_client: C,
    coprocessor_url: String,
    service_name: String,
    mut request: subgraph::Request,
    request_config: SubgraphConf,
) -> Result<ControlFlow<subgraph::Response, subgraph::Request>, BoxError>
where
    C: Service<hyper::Request<Body>, Response = hyper::Response<Body>, Error = BoxError>
        + Clone
        + Send
        + Sync
        + 'static,
    <C as tower::Service<http::Request<hyper::Body>>>::Future: Send + 'static,
{
    // Call into our out of process processor with a body of our body
    // First, extract the data we need from our request and prepare our
    // external call. Use our configuration to figure out which data to send.
    let (parts, body) = request.subgraph_request.into_parts();
    let bytes = Bytes::from(serde_json::to_vec(&body)?);

    let headers_to_send = request_config
        .headers
        .then(|| externalize_header_map(&parts.headers))
        .transpose()?;

    let body_to_send = request_config
        .body
        .then(|| serde_json::from_slice::<serde_json::Value>(&bytes))
        .transpose()?;
    let context_to_send = request_config.context.then(|| request.context.clone());
    let uri = request_config.uri.then(|| parts.uri.to_string());
    let service_name = request_config.service_name.then_some(service_name);

    let payload = Externalizable {
        version: EXTERNALIZABLE_VERSION,
        stage: PipelineStep::SubgraphRequest.to_string(),
        control: Some(Control::default()),
        id: TraceId::maybe_new().map(|id| id.to_string()),
        headers: headers_to_send,
        body: body_to_send,
        context: context_to_send,
        sdl: None,
        service_name,
        uri,
    };

    tracing::debug!(?payload, "externalized output");
    request.context.enter_active_request().await;
    let co_processor_result = payload.call(http_client, &coprocessor_url).await;
    request.context.leave_active_request().await;
    tracing::debug!(?co_processor_result, "co-processor returned");
    let co_processor_output = co_processor_result?;
    validate_coprocessor_output(&co_processor_output, PipelineStep::SubgraphRequest)?;
    // unwrap is safe here because validate_coprocessor_output made sure control is available
    let control = co_processor_output.control.expect("validated above; qed");

    // Thirdly, we need to interpret the control flow which may have been
    // updated by our co-processor and decide if we should proceed or stop.

    if matches!(control, Control::Break(_)) {
        // Ensure the code is a valid http status code
        let code = control.get_http_status()?;

        let res = {
            let graphql_response: crate::graphql::Response =
                serde_json::from_value(co_processor_output.body.unwrap_or(serde_json::Value::Null))
                    .unwrap_or_else(|error| {
                        crate::graphql::Response::builder()
                            .errors(vec![Error::builder()
                                .message(format!(
                                    "couldn't deserialize coprocessor output body: {error}"
                                ))
                                .extension_code("EXERNAL_DESERIALIZATION_ERROR")
                                .build()])
                            .build()
                    });

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

async fn process_subgraph_response_stage<C>(
    http_client: C,
    coprocessor_url: String,
    service_name: String,
    mut response: subgraph::Response,
    response_config: SubgraphConf,
) -> Result<subgraph::Response, BoxError>
where
    C: Service<hyper::Request<Body>, Response = hyper::Response<Body>, Error = BoxError>
        + Clone
        + Send
        + Sync
        + 'static,
    <C as tower::Service<http::Request<hyper::Body>>>::Future: Send + 'static,
{
    // Call into our out of process processor with a body of our body
    // First, extract the data we need from our response and prepare our
    // external call. Use our configuration to figure out which data to send.

    let (parts, body) = response.response.into_parts();
    let bytes = Bytes::from(serde_json::to_vec(&body)?);

    let headers_to_send = response_config
        .headers
        .then(|| externalize_header_map(&parts.headers))
        .transpose()?;

    let body_to_send = response_config
        .body
        .then(|| serde_json::from_slice::<serde_json::Value>(&bytes))
        .transpose()?;
    let context_to_send = response_config.context.then(|| response.context.clone());
    let service_name = response_config.service_name.then_some(service_name);

    let payload = Externalizable {
        version: EXTERNALIZABLE_VERSION,
        stage: PipelineStep::SubgraphResponse.to_string(),
        control: None,
        id: TraceId::maybe_new().map(|id| id.to_string()),
        headers: headers_to_send,
        body: body_to_send,
        context: context_to_send,
        sdl: None,
        uri: None,
        service_name,
    };

    tracing::debug!(?payload, "externalized output");
    response.context.enter_active_request().await;
    let co_processor_result = payload.call(http_client, &coprocessor_url).await;
    response.context.leave_active_request().await;
    tracing::debug!(?co_processor_result, "co-processor returned");
    let co_processor_output = co_processor_result?;

    validate_coprocessor_output(&co_processor_output, PipelineStep::SubgraphResponse)?;

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

    Ok(response)
}

// -----------------------------------------------------------------------------------------

fn validate_coprocessor_output(
    co_processor_output: &Externalizable<serde_json::Value>,
    expected_step: PipelineStep,
) -> Result<(), BoxError> {
    if co_processor_output.version != EXTERNALIZABLE_VERSION {
        return Err(BoxError::from(format!(
            "Coprocessor returned the wrong version: expected `{}` found `{}`",
            EXTERNALIZABLE_VERSION, co_processor_output.version,
        )));
    }
    if co_processor_output.stage != expected_step.to_string() {
        return Err(BoxError::from(format!(
            "Coprocessor returned the wrong stage: expected `{}` found `{}`",
            expected_step, co_processor_output.stage,
        )));
    }
    if co_processor_output.control.is_none() && co_processor_output.stage.ends_with("Request") {
        return Err(BoxError::from(format!(
            "Coprocessor response is missing the `control` parameter in the `{}` stage. You must specify \"control\": \"Continue\" or \"control\": \"Break\"",
            co_processor_output.stage,
        )));
    }
    Ok(())
}

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
    use futures::future::BoxFuture;
    use http::header::ACCEPT;
    use http::header::CONTENT_TYPE;
    use http::HeaderMap;
    use http::HeaderValue;
    use mime::APPLICATION_JSON;
    use mime::TEXT_HTML;
    use serde_json::json;

    use super::*;
    use crate::plugin::test::MockHttpClientService;
    use crate::plugin::test::MockRouterService;
    use crate::plugin::test::MockSubgraphService;
    use crate::services::router_service;

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
    async fn unknown_fields_are_denied() {
        let config = serde_json::json!({
            "plugins": {
                "experimental.external": {
                    "url": "http://127.0.0.1:8081",
                    "thisFieldDoesntExist": true
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
    async fn external_plugin_with_stages_wont_load_without_graph_ref() {
        let config = serde_json::json!({
            "plugins": {
                "experimental.external": {
                    "url": "http://127.0.0.1:8081",
                    "stages": {
                        "subgraph": {
                            "request": {
                                "uri": true
                            }
                        }
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
    async fn coprocessor_returning_the_wrong_version_should_fail() {
        let router_stage = RouterStage {
            request: RouterConf {
                headers: true,
                context: true,
                body: true,
                sdl: true,
            },
            response: Default::default(),
        };

        // This will never be called because we will fail at the coprocessor.
        let mock_router_service = MockRouterService::new();

        let mock_http_client = mock_with_callback(move |_: hyper::Request<Body>| {
            Box::pin(async {
                // Wrong version!
                Ok(hyper::Response::builder()
                    .body(Body::from(
                        r##"{
                    "version": 2,
                    "stage": "RouterRequest",
                    "control": "Continue",
                    "id": "1b19c05fdafc521016df33148ad63c1b",
                    "body": {
                      "query": "query Long {\n  me {\n  name\n}\n}"
                    },
                    "context": {
                        "entries": {}
                    },
                    "sdl": "the sdl shouldnt change"
                  }"##,
                    ))
                    .unwrap())
            })
        });

        let service = router_stage.as_service(
            mock_http_client,
            mock_router_service.boxed(),
            "http://test".to_string(),
            Arc::new("".to_string()),
        );

        let request = supergraph::Request::canned_builder().build().unwrap();

        assert_eq!(
            "Coprocessor returned the wrong version: expected `1` found `2`",
            service
                .oneshot(request.try_into().unwrap())
                .await
                .unwrap_err()
                .to_string()
        );
    }

    #[tokio::test]
    async fn coprocessor_returning_the_wrong_stage_should_fail() {
        let router_stage = RouterStage {
            request: RouterConf {
                headers: true,
                context: true,
                body: true,
                sdl: true,
            },
            response: Default::default(),
        };

        // This will never be called because we will fail at the coprocessor.
        let mock_router_service = MockRouterService::new();

        let mock_http_client = mock_with_callback(move |_: hyper::Request<Body>| {
            Box::pin(async {
                // Wrong stage!
                Ok(hyper::Response::builder()
                    .body(Body::from(
                        r##"{
                            "version": 1,
                            "stage": "RouterResponse",
                            "control": "Continue",
                            "id": "1b19c05fdafc521016df33148ad63c1b",
                            "body": {
                            "query": "query Long {\n  me {\n  name\n}\n}"
                            },
                            "context": {
                                "entries": {}
                            },
                            "sdl": "the sdl shouldnt change"
                        }"##,
                    ))
                    .unwrap())
            })
        });

        let service = router_stage.as_service(
            mock_http_client,
            mock_router_service.boxed(),
            "http://test".to_string(),
            Arc::new("".to_string()),
        );

        let request = supergraph::Request::canned_builder().build().unwrap();

        assert_eq!(
            "Coprocessor returned the wrong stage: expected `RouterRequest` found `RouterResponse`",
            service
                .oneshot(request.try_into().unwrap())
                .await
                .unwrap_err()
                .to_string()
        );
    }

    #[tokio::test]
    async fn coprocessor_missing_request_control_should_fail() {
        let router_stage = RouterStage {
            request: RouterConf {
                headers: true,
                context: true,
                body: true,
                sdl: true,
            },
            response: Default::default(),
        };

        // This will never be called because we will fail at the coprocessor.
        let mock_router_service = MockRouterService::new();

        let mock_http_client = mock_with_callback(move |_: hyper::Request<Body>| {
            Box::pin(async {
                // Wrong stage!
                Ok(hyper::Response::builder()
                    .body(Body::from(
                        r##"{
                            "version": 1,
                            "stage": "RouterRequest",
                            "id": "1b19c05fdafc521016df33148ad63c1b",
                            "body": {
                            "query": "query Long {\n  me {\n  name\n}\n}"
                            },
                            "context": {
                                "entries": {}
                            },
                            "sdl": "the sdl shouldnt change"
                        }"##,
                    ))
                    .unwrap())
            })
        });

        let service = router_stage.as_service(
            mock_http_client,
            mock_router_service.boxed(),
            "http://test".to_string(),
            Arc::new("".to_string()),
        );

        let request = supergraph::Request::canned_builder().build().unwrap();

        assert_eq!(
            "Coprocessor response is missing the `control` parameter in the `RouterRequest` stage. You must specify \"control\": \"Continue\" or \"control\": \"Break\"",
            service
                .oneshot(request.try_into().unwrap())
                .await
                .unwrap_err()
                .to_string()
        );
    }

    #[tokio::test]
    async fn coprocessor_subgraph_with_invalid_response_body_should_fail() {
        let subgraph_stage = SubgraphStage {
            request: SubgraphConf {
                headers: false,
                context: false,
                body: true,
                uri: false,
                service: false,
                service_name: false,
            },
            response: Default::default(),
        };

        // This will never be called because we will fail at the coprocessor.
        let mock_subgraph_service = MockSubgraphService::new();

        let mock_http_client = mock_with_callback(move |_: hyper::Request<Body>| {
            Box::pin(async {
                Ok(hyper::Response::builder()
                    .body(Body::from(
                        r##"{
                                "version": 1,
                                "stage": "SubgraphRequest",
                                "control": {
                                    "Break": 200
                                },
                                "id": "3a67e2dd75e8777804e4a8f42b971df7",
                                "body": {
                                    "errors": [{
                                        "body": "Errors need a message, this will fail to deserialize"
                                    }]
                                }
                            }"##,
                    ))
                    .unwrap())
            })
        });

        let service = subgraph_stage.as_service(
            mock_http_client,
            mock_subgraph_service.boxed(),
            "http://test".to_string(),
            "my_subgraph_service_name".to_string(),
        );

        let request = subgraph::Request::fake_builder().build();

        assert_eq!(
            "couldn't deserialize coprocessor output body: missing field `message`",
            service
                .oneshot(request)
                .await
                .unwrap()
                .response
                .into_body()
                .errors[0]
                .message
                .to_string()
        );
    }

    #[tokio::test]
    async fn external_plugin_subgraph_request() {
        let subgraph_stage = SubgraphStage {
            request: SubgraphConf {
                headers: false,
                context: false,
                body: true,
                uri: false,
                service: false,
                service_name: false,
            },
            response: Default::default(),
        };

        // This will never be called because we will fail at the coprocessor.
        let mut mock_subgraph_service = MockSubgraphService::new();

        mock_subgraph_service
            .expect_call()
            .returning(|req: subgraph::Request| {
                // Let's assert that the subgraph request has been transformed as it should have.
                assert_eq!(
                    req.subgraph_request.headers().get("cookie").unwrap(),
                    "tasty_cookie=strawberry"
                );

                assert_eq!(
                    req.context
                        .get::<&str, u8>("this-is-a-test-context")
                        .unwrap()
                        .unwrap(),
                    42
                );

                // The subgraph uri should have changed
                assert_eq!(
                    "http://thisurihaschanged/",
                    req.subgraph_request.uri().to_string()
                );

                // The query should have changed
                assert_eq!(
                    "query Long {\n  me {\n  name\n}\n}",
                    req.subgraph_request.into_body().query.unwrap()
                );

                Ok(subgraph::Response::builder()
                    .data(json!({ "test": 1234_u32 }))
                    .errors(Vec::new())
                    .extensions(crate::json_ext::Object::new())
                    .context(req.context)
                    .build())
            });

        let mock_http_client = mock_with_callback(move |_: hyper::Request<Body>| {
            Box::pin(async {
                Ok(hyper::Response::builder()
                    .body(Body::from(
                        r##"{
                                "version": 1,
                                "stage": "SubgraphRequest",
                                "control": "Continue",
                                "headers": {
                                    "cookie": [
                                      "tasty_cookie=strawberry"
                                    ],
                                    "content-type": [
                                      "application/json"
                                    ],
                                    "host": [
                                      "127.0.0.1:4000"
                                    ],
                                    "apollo-federation-include-trace": [
                                      "ftv1"
                                    ],
                                    "apollographql-client-name": [
                                      "manual"
                                    ],
                                    "accept": [
                                      "*/*"
                                    ],
                                    "user-agent": [
                                      "curl/7.79.1"
                                    ],
                                    "content-length": [
                                      "46"
                                    ]
                                  },
                                  "body": {
                                    "query": "query Long {\n  me {\n  name\n}\n}"
                                  },
                                  "context": {
                                    "entries": {
                                      "accepts-json": false,
                                      "accepts-wildcard": true,
                                      "accepts-multipart": false,
                                      "this-is-a-test-context": 42
                                    }
                                  },
                                  "serviceName": "service name shouldn't change",
                                  "uri": "http://thisurihaschanged"
                            }"##,
                    ))
                    .unwrap())
            })
        });

        let service = subgraph_stage.as_service(
            mock_http_client,
            mock_subgraph_service.boxed(),
            "http://test".to_string(),
            "my_subgraph_service_name".to_string(),
        );

        let request = subgraph::Request::fake_builder().build();

        assert_eq!(
            serde_json_bytes::json!({ "test": 1234_u32 }),
            service
                .oneshot(request)
                .await
                .unwrap()
                .response
                .into_body()
                .data
                .unwrap()
        );
    }

    #[tokio::test]
    async fn external_plugin_subgraph_request_controlflow_break() {
        let subgraph_stage = SubgraphStage {
            request: SubgraphConf {
                headers: false,
                context: false,
                body: true,
                uri: false,
                service: false,
                service_name: false,
            },
            response: Default::default(),
        };

        // This will never be called because we will fail at the coprocessor.
        let mock_subgraph_service = MockSubgraphService::new();

        let mock_http_client = mock_with_callback(move |_: hyper::Request<Body>| {
            Box::pin(async {
                Ok(hyper::Response::builder()
                    .body(Body::from(
                        r##"{
                                "version": 1,
                                "stage": "SubgraphRequest",
                                "control": {
                                    "Break": 200
                                },
                                "body": {
                                    "errors": [{ "message": "my error message" }]
                                }
                            }"##,
                    ))
                    .unwrap())
            })
        });

        let service = subgraph_stage.as_service(
            mock_http_client,
            mock_subgraph_service.boxed(),
            "http://test".to_string(),
            "my_subgraph_service_name".to_string(),
        );

        let request = subgraph::Request::fake_builder().build();

        assert_eq!(
            serde_json::json!({ "errors": [{ "message": "my error message" }] }),
            serde_json::to_value(service.oneshot(request).await.unwrap().response.into_body())
                .unwrap()
        );
    }

    #[tokio::test]
    async fn external_plugin_subgraph_response() {
        let subgraph_stage = SubgraphStage {
            request: Default::default(),
            response: SubgraphConf {
                headers: false,
                context: false,
                body: true,
                uri: false,
                service: false,
                service_name: false,
            },
        };

        // This will never be called because we will fail at the coprocessor.
        let mut mock_subgraph_service = MockSubgraphService::new();

        mock_subgraph_service
            .expect_call()
            .returning(|req: subgraph::Request| {
                Ok(subgraph::Response::builder()
                    .data(json!({ "test": 1234_u32 }))
                    .errors(Vec::new())
                    .extensions(crate::json_ext::Object::new())
                    .context(req.context)
                    .build())
            });

        let mock_http_client = mock_with_callback(move |_: hyper::Request<Body>| {
            Box::pin(async {
                Ok(hyper::Response::builder()
                    .body(Body::from(
                        r##"{
                                "version": 1,
                                "stage": "SubgraphResponse",
                                "headers": {
                                    "cookie": [
                                      "tasty_cookie=strawberry"
                                    ],
                                    "content-type": [
                                      "application/json"
                                    ],
                                    "host": [
                                      "127.0.0.1:4000"
                                    ],
                                    "apollo-federation-include-trace": [
                                      "ftv1"
                                    ],
                                    "apollographql-client-name": [
                                      "manual"
                                    ],
                                    "accept": [
                                      "*/*"
                                    ],
                                    "user-agent": [
                                      "curl/7.79.1"
                                    ],
                                    "content-length": [
                                      "46"
                                    ]
                                  },
                                  "body": {
                                    "data": {
                                        "test": 5678
                                    }
                                  },
                                  "context": {
                                    "entries": {
                                      "accepts-json": false,
                                      "accepts-wildcard": true,
                                      "accepts-multipart": false,
                                      "this-is-a-test-context": 42
                                    }
                                  }
                            }"##,
                    ))
                    .unwrap())
            })
        });

        let service = subgraph_stage.as_service(
            mock_http_client,
            mock_subgraph_service.boxed(),
            "http://test".to_string(),
            "my_subgraph_service_name".to_string(),
        );

        let request = subgraph::Request::fake_builder().build();

        let response = service.oneshot(request).await.unwrap();

        // Let's assert that the subgraph response has been transformed as it should have.
        assert_eq!(
            response.response.headers().get("cookie").unwrap(),
            "tasty_cookie=strawberry"
        );

        assert_eq!(
            response
                .context
                .get::<&str, u8>("this-is-a-test-context")
                .unwrap()
                .unwrap(),
            42
        );

        assert_eq!(
            serde_json_bytes::json!({ "test": 5678_u32 }),
            response.response.into_body().data.unwrap()
        );
    }

    #[tokio::test]
    async fn external_plugin_router_request() {
        let router_stage = RouterStage {
            request: RouterConf {
                headers: true,
                context: true,
                body: true,
                sdl: true,
            },
            response: Default::default(),
        };

        let mock_router_service = router_service::from_supergraph_mock_callback(move |req| {
            // Let's assert that the router request has been transformed as it should have.
            assert_eq!(
                req.supergraph_request.headers().get("cookie").unwrap(),
                "tasty_cookie=strawberry"
            );

            assert_eq!(
                req.context
                    .get::<&str, u8>("this-is-a-test-context")
                    .unwrap()
                    .unwrap(),
                42
            );

            // The query should have changed
            assert_eq!(
                "query Long {\n  me {\n  name\n}\n}",
                req.supergraph_request.into_body().query.unwrap()
            );

            Ok(supergraph::Response::builder()
                .data(json!({ "test": 1234_u32 }))
                .context(req.context)
                .build()
                .unwrap())
        })
        .await;

        let mock_http_client = mock_with_callback(move |req: hyper::Request<Body>| {
            Box::pin(async {
                let deserialized_request: Externalizable<serde_json::Value> =
                    serde_json::from_slice(&hyper::body::to_bytes(req.into_body()).await.unwrap())
                        .unwrap();

                assert_eq!(EXTERNALIZABLE_VERSION, deserialized_request.version);
                assert_eq!(
                    PipelineStep::RouterRequest.to_string(),
                    deserialized_request.stage
                );

                Ok(hyper::Response::builder()
                    .body(Body::from(
                        r##"{
                    "version": 1,
                    "stage": "RouterRequest",
                    "control": "Continue",
                    "id": "1b19c05fdafc521016df33148ad63c1b",
                    "headers": {
                      "cookie": [
                        "tasty_cookie=strawberry"
                      ],
                      "content-type": [
                        "application/json"
                      ],
                      "host": [
                        "127.0.0.1:4000"
                      ],
                      "apollo-federation-include-trace": [
                        "ftv1"
                      ],
                      "apollographql-client-name": [
                        "manual"
                      ],
                      "accept": [
                        "*/*"
                      ],
                      "user-agent": [
                        "curl/7.79.1"
                      ],
                      "content-length": [
                        "46"
                      ]
                    },
                    "body": {
                      "query": "query Long {\n  me {\n  name\n}\n}"
                    },
                    "context": {
                      "entries": {
                        "accepts-json": false,
                        "accepts-wildcard": true,
                        "accepts-multipart": false,
                        "this-is-a-test-context": 42
                      }
                    },
                    "sdl": "the sdl shouldnt change"
                  }"##,
                    ))
                    .unwrap())
            })
        });

        let service = router_stage.as_service(
            mock_http_client,
            mock_router_service.boxed(),
            "http://test".to_string(),
            Arc::new("".to_string()),
        );

        let request = supergraph::Request::canned_builder().build().unwrap();

        service.oneshot(request.try_into().unwrap()).await.unwrap();
    }

    #[tokio::test]
    async fn external_plugin_router_request_controlflow_break() {
        let router_stage = RouterStage {
            request: RouterConf {
                headers: true,
                context: true,
                body: true,
                sdl: true,
            },
            response: Default::default(),
        };

        let mock_router_service = MockRouterService::new();

        let mock_http_client = mock_with_callback(move |req: hyper::Request<Body>| {
            Box::pin(async {
                let deserialized_request: Externalizable<serde_json::Value> =
                    serde_json::from_slice(&hyper::body::to_bytes(req.into_body()).await.unwrap())
                        .unwrap();

                assert_eq!(EXTERNALIZABLE_VERSION, deserialized_request.version);
                assert_eq!(
                    PipelineStep::RouterRequest.to_string(),
                    deserialized_request.stage
                );

                Ok(hyper::Response::builder()
                    .body(Body::from(
                        r##"{
                    "version": 1,
                    "stage": "RouterRequest",
                    "control": {
                        "Break": 200
                    },
                    "id": "1b19c05fdafc521016df33148ad63c1b",
                    "body": {
                      "errors": [{ "message": "my error message" }]
                    }
                  }"##,
                    ))
                    .unwrap())
            })
        });

        let service = router_stage.as_service(
            mock_http_client,
            mock_router_service.boxed(),
            "http://test".to_string(),
            Arc::new("".to_string()),
        );

        let request = supergraph::Request::canned_builder().build().unwrap();

        let actual_response = serde_json::from_slice::<serde_json::Value>(
            &hyper::body::to_bytes(
                service
                    .oneshot(request.try_into().unwrap())
                    .await
                    .unwrap()
                    .response
                    .into_body(),
            )
            .await
            .unwrap(),
        )
        .unwrap();

        assert_eq!(
            serde_json::json!({
                "errors": [{
                   "message": "my error message"
                }]
            }),
            actual_response
        );
    }

    #[tokio::test]
    async fn external_plugin_router_response() {
        let router_stage = RouterStage {
            response: RouterConf {
                headers: true,
                context: true,
                body: true,
                sdl: true,
            },
            request: Default::default(),
        };

        let mock_router_service = router_service::from_supergraph_mock_callback(move |req| {
            Ok(supergraph::Response::builder()
                .data(json!({ "test": 1234_u32 }))
                .context(req.context)
                .build()
                .unwrap())
        })
        .await;

        let mock_http_client = mock_with_callback(move |res: hyper::Request<Body>| {
            Box::pin(async {
                let deserialized_response: Externalizable<serde_json::Value> =
                    serde_json::from_slice(&hyper::body::to_bytes(res.into_body()).await.unwrap())
                        .unwrap();

                assert_eq!(EXTERNALIZABLE_VERSION, deserialized_response.version);
                assert_eq!(
                    PipelineStep::RouterResponse.to_string(),
                    deserialized_response.stage
                );

                assert_eq!(
                    json!({ "data": { "test": 1234_u32 } }),
                    deserialized_response.body.unwrap()
                );

                Ok(hyper::Response::builder()
                    .body(Body::from(
                        r##"{
                    "version": 1,
                    "stage": "RouterResponse",
                    "id": "1b19c05fdafc521016df33148ad63c1b",
                    "headers": {
                      "cookie": [
                        "tasty_cookie=strawberry"
                      ],
                      "content-type": [
                        "application/json"
                      ],
                      "host": [
                        "127.0.0.1:4000"
                      ],
                      "apollo-federation-include-trace": [
                        "ftv1"
                      ],
                      "apollographql-client-name": [
                        "manual"
                      ],
                      "accept": [
                        "*/*"
                      ],
                      "user-agent": [
                        "curl/7.79.1"
                      ],
                      "content-length": [
                        "46"
                      ]
                    },
                    "body": {
                      "data": { "test": 42 }
                    },
                    "context": {
                      "entries": {
                        "accepts-json": false,
                        "accepts-wildcard": true,
                        "accepts-multipart": false,
                        "this-is-a-test-context": 42
                      }
                    },
                    "sdl": "the sdl shouldnt change"
                  }"##,
                    ))
                    .unwrap())
            })
        });

        let service = router_stage.as_service(
            mock_http_client,
            mock_router_service.boxed(),
            "http://test".to_string(),
            Arc::new("".to_string()),
        );

        let request = supergraph::Request::canned_builder().build().unwrap();

        let res = service.oneshot(request.try_into().unwrap()).await.unwrap();

        // Let's assert that the router request has been transformed as it should have.
        assert_eq!(
            res.response.headers().get("cookie").unwrap(),
            "tasty_cookie=strawberry"
        );

        assert_eq!(
            res.context
                .get::<&str, u8>("this-is-a-test-context")
                .unwrap()
                .unwrap(),
            42
        );

        // the body should have changed:
        assert_eq!(
            json!({ "data": { "test": 42_u32 } }),
            serde_json::from_slice::<serde_json::Value>(
                &hyper::body::to_bytes(res.response.into_body())
                    .await
                    .unwrap()
            )
            .unwrap()
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

    #[allow(clippy::type_complexity)]
    fn mock_with_callback(
        callback: fn(
            hyper::Request<Body>,
        ) -> BoxFuture<'static, Result<hyper::Response<Body>, BoxError>>,
    ) -> MockHttpClientService {
        let mut mock_http_client = MockHttpClientService::new();
        mock_http_client.expect_clone().returning(move || {
            let mut mock_http_client = MockHttpClientService::new();
            mock_http_client.expect_clone().returning(move || {
                let mut mock_http_client = MockHttpClientService::new();
                mock_http_client.expect_call().returning(callback);
                mock_http_client
            });
            mock_http_client
        });

        mock_http_client
    }
}
