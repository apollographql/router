//! Externalization plugin
// With regards to ELv2 licensing, this entire file is license key functionality

use std::collections::HashMap;
use std::fmt;
use std::ops::ControlFlow;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use http::header::HeaderName;
use http::HeaderMap;
use http::HeaderValue;
use hyper::body;
use hyper::client::HttpConnector;
use hyper::Body;
use hyper_rustls::ConfigBuilderExt;
use hyper_rustls::HttpsConnector;
use schemars::JsonSchema;
use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde::Serialize;
use tower::timeout::TimeoutLayer;
use tower::util::MapFutureLayer;
use tower::BoxError;
use tower::Service;
use tower::ServiceBuilder;
use tower::ServiceExt;

use crate::error::Error;
use crate::layers::async_checkpoint::AsyncCheckpointLayer;
use crate::layers::ServiceBuilderExt;
use crate::plugin::Plugin;
use crate::plugin::PluginInit;
use crate::register_plugin;
use crate::services::external::Control;
use crate::services::external::Externalizable;
use crate::services::external::PipelineStep;
use crate::services::external::DEFAULT_EXTERNALIZATION_TIMEOUT;
use crate::services::external::EXTERNALIZABLE_VERSION;
use crate::services::{execution, router, subgraph, supergraph};
use crate::tracer::TraceId;
use crate::Context;

pub(crate) const EXTERNAL_SPAN_NAME: &str = "external_plugin";

type HTTPClientService = tower::timeout::Timeout<hyper::Client<HttpsConnector<HttpConnector>>>;

#[async_trait::async_trait]
impl Plugin for ExternalPlugin<HTTPClientService> {
    type Config = Conf;

    async fn new(init: PluginInit<Self::Config>) -> Result<Self, BoxError> {
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
        let request_sdl = self.sdl.clone();
        let response_sdl = self.sdl.clone();

        let request_full_config = self.configuration.clone();
        let response_full_config = self.configuration.clone();

        let request_layer = if self
            .configuration
            .stages
            .as_ref()
            .and_then(|x| x.router.as_ref())
            .and_then(|x| x.request.as_ref())
            .is_some()
        {
            // Safe to unwrap here because we just confirmed that all optional elements are present
            let request_config = request_full_config
                .stages
                .unwrap()
                .router
                .unwrap()
                .request
                .unwrap();
            Some(AsyncCheckpointLayer::new(
                move |mut request: router::Request| {
                    let my_sdl = request_sdl.to_string();
                    let proto_url = request_full_config.url.clone();
                    let timeout = request_full_config.timeout;
                    let request_config = request_config.clone();
                    async move {
                        // Call into our out of process processor with a body of our body
                        // First, extract the data we need from our request and prepare our
                        // external call. Use our configuration to figure out which data to send.

                        let (parts, body) = request.router_request.into_parts();
                        let b_bytes = body::to_bytes(body).await?;

                        let (headers, payload, context, sdl) = prepare_external_params(
                            &request_config,
                            &parts.headers,
                            &b_bytes,
                            &request.context,
                            my_sdl,
                        )?;

                        request.context.enter_active_request().await;

                        // Second, call our co-processor and get a reply.
                        let res = call_external(
                            proto_url,
                            timeout,
                            PipelineStep::RouterRequest,
                            headers,
                            payload,
                            context,
                            sdl,
                        )
                        .await;

                        request.context.leave_active_request().await;

                        let co_processor_output = res?;

                        tracing::debug!(?co_processor_output, "co-processor returned");

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
                            None => Body::from(b_bytes),
                        };

                        request.router_request = http::Request::from_parts(parts, new_body);

                        if let Some(context) = co_processor_output.context {
                            request.context = context;
                        }

                        if let Some(headers) = co_processor_output.headers {
                            *request.router_request.headers_mut() =
                                internalize_header_map(headers)?;
                        }

                        Ok(ControlFlow::Continue(request))
                    }
                },
            ))
        } else {
            None
        };

        let response_layer = if self
            .configuration
            .stages
            .as_ref()
            .and_then(|x| x.router.as_ref())
            .and_then(|x| x.response.as_ref())
            .is_some()
        {
            // Safe to unwrap here because we just confirmed that all optional elements are present
            let response_config = response_full_config
                .stages
                .unwrap()
                .router
                .unwrap()
                .response
                .unwrap();
            Some(MapFutureLayer::new(move |fut| {
                let my_sdl = response_sdl.to_string();
                let proto_url = response_full_config.url.clone();
                let timeout = response_full_config.timeout;
                let response_config = response_config.clone();
                async move {
                    let mut response: router::Response = fut.await?;

                    // Call into our out of process processor with a body of our body
                    // First, extract the data we need from our response and prepare our
                    // external call. Use our configuration to figure out which data to send.

                    let (parts, body) = response.response.into_parts();
                    let b_bytes = body::to_bytes(body).await?;

                    let (headers, payload, context, sdl) = prepare_external_params(
                        &response_config,
                        &parts.headers,
                        &b_bytes,
                        &response.context,
                        my_sdl,
                    )?;

                    // Second, call our co-processor and get a reply.
                    response.context.enter_active_request().await;
                    let co_processor_result = call_external(
                        proto_url,
                        timeout,
                        PipelineStep::RouterResponse,
                        headers,
                        payload,
                        context,
                        sdl,
                    )
                    .await;
                    response.context.leave_active_request().await;
                    tracing::debug!(?co_processor_result, "co-processor returned");
                    let co_processor_output = co_processor_result?;

                    // Third, process our reply and act on the contents. Our processing logic is
                    // that we replace "bits" of our incoming response with the updated bits if they
                    // are present in our co_processor_output. If they aren't present, just use the
                    // bits that we sent to the co_processor.

                    let new_body = match co_processor_output.body {
                        Some(bytes) => Body::from(serde_json::to_vec(&bytes)?),
                        None => Body::from(b_bytes),
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
            }))
        } else {
            None
        };

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

    fn supergraph_service(&self, service: supergraph::BoxService) -> supergraph::BoxService {
        service
    }

    fn execution_service(&self, service: execution::BoxService) -> execution::BoxService {
        service
    }

    fn subgraph_service(&self, _name: &str, service: subgraph::BoxService) -> subgraph::BoxService {
        if let Some(stages) = &self.configuration.stages {
            if let Some(subgraph_stage) = &stages.subgraph {
                subgraph_stage.as_service(
                    self.http_client.clone(),
                    service,
                    self.configuration.url.clone(),
                    self.configuration.timeout,
                )
            } else {
                service
            }
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

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize, JsonSchema)]
#[serde(default)]
struct RouterStage {
    /// The request configuration
    request: Option<RouterConf>,
    /// The response configuration
    response: Option<RouterConf>,
}

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
        timeout: Option<Duration>,
    ) -> subgraph::BoxService
    where
        C: Service<hyper::Request<Body>, Response = hyper::Response<Body>, Error = BoxError>
            + Clone
            + Send
            + Sync
            + 'static,
        <C as tower::Service<http::Request<hyper::Body>>>::Future: Send + Sync + 'static,
    {
        let request_layer = self.request.as_ref().map(|request_config| {
            let request_config = request_config.clone();
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
                    let co_processor_result = payload
                        .call_with_client(http_client, &coprocessor_url)
                        .await;
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

        let response_layer = self.response.as_ref().map(|response_config| {
            let response_config = response_config.clone();
            let coprocessor_url = coprocessor_url.clone();

            MapFutureLayer::new(move |fut| {
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
                        stage: PipelineStep::SubgraphRequest.to_string(),
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
                    let co_processor_result = payload.call(&coprocessor_url, timeout).await;
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

type ExternalParams<'a> = (
    Option<&'a HeaderMap<HeaderValue>>,
    Option<serde_json::Value>,
    Option<Context>,
    Option<String>,
);

fn prepare_external_params<'a>(
    config: &'a RouterConf,
    headers: &'a HeaderMap<HeaderValue>,
    bytes: &'a Bytes,
    context: &'a Context,
    sdl: String,
) -> Result<ExternalParams<'a>, BoxError> {
    let mut headers_opt = None;
    // Note: We have to specify the json type or it won't deserialize correctly...
    // let mut payload_opt: Option<serde_json::Value> = None;
    let mut payload_opt: Option<serde_json::Value> = None;
    let mut context_opt = None;
    let mut sdl_opt = None;

    if config.body {
        payload_opt = Some(serde_json::from_slice(bytes)?);
    }
    if config.headers {
        headers_opt = Some(headers);
    }

    if config.context {
        context_opt = Some(context.clone());
    }
    if config.sdl {
        sdl_opt = Some(sdl);
    }
    Ok((headers_opt, payload_opt, context_opt, sdl_opt))
}

// TODO: exploit the PipelineStage enum for this,
// it has all of the relevant configuration
async fn call_external<T>(
    url: String,
    timeout: Option<Duration>,
    stage: PipelineStep,
    headers: Option<&HeaderMap<HeaderValue>>,
    payload: Option<T>,
    context: Option<Context>,
    sdl: Option<String>,
) -> Result<Externalizable<T>, BoxError>
where
    T: fmt::Debug + DeserializeOwned + Serialize + Send + Sync + 'static,
{
    let mut converted_headers = None;
    if let Some(hdrs) = headers {
        converted_headers = Some(externalize_header_map(hdrs)?);
    };
    let output = Externalizable::new(stage, converted_headers, payload, context, sdl);
    tracing::debug!(?output, "externalized output");
    output.call(&url, timeout).await
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
