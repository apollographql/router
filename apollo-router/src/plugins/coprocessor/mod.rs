//! Externalization plugin

use std::collections::HashMap;
use std::ops::ControlFlow;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;
use std::time::Instant;

use bytes::Bytes;
use futures::future::ready;
use futures::stream::once;
use futures::StreamExt;
use futures::TryStreamExt;
use http::header;
use http::HeaderMap;
use http::HeaderName;
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
use url::Url;

use crate::error::Error;
use crate::layers::async_checkpoint::OneShotAsyncCheckpointLayer;
use crate::layers::ServiceBuilderExt;
use crate::plugin::Plugin;
use crate::plugin::PluginInit;
use crate::register_plugin;
use crate::services;
use crate::services::external::externalize_header_map;
use crate::services::external::Control;
use crate::services::external::Externalizable;
use crate::services::external::PipelineStep;
use crate::services::external::DEFAULT_EXTERNALIZATION_TIMEOUT;
use crate::services::external::EXTERNALIZABLE_VERSION;
use crate::services::router;
use crate::services::subgraph;
use crate::services::trust_dns_connector::new_async_http_connector;
use crate::services::trust_dns_connector::AsyncHyperResolver;

#[cfg(test)]
mod test;

mod execution;
mod supergraph;

pub(crate) const EXTERNAL_SPAN_NAME: &str = "external_plugin";
const POOL_IDLE_TIMEOUT_DURATION: Option<Duration> = Some(Duration::from_secs(5));
const COPROCESSOR_ERROR_EXTENSION: &str = "ERROR";
const COPROCESSOR_DESERIALIZATION_ERROR_EXTENSION: &str = "EXTERNAL_DESERIALIZATION_ERROR";

type HTTPClientService =
    tower::timeout::Timeout<hyper::Client<HttpsConnector<HttpConnector<AsyncHyperResolver>>, Body>>;

#[async_trait::async_trait]
impl Plugin for CoprocessorPlugin<HTTPClientService> {
    type Config = Conf;

    async fn new(init: PluginInit<Self::Config>) -> Result<Self, BoxError> {
        let mut http_connector = new_async_http_connector()?;
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
            .service(
                hyper::Client::builder()
                    .pool_idle_timeout(POOL_IDLE_TIMEOUT_DURATION)
                    .build(connector),
            );

        CoprocessorPlugin::new(http_client, init.config, init.supergraph_sdl)
    }

    fn router_service(&self, service: router::BoxService) -> router::BoxService {
        self.router_service(service)
    }

    fn supergraph_service(
        &self,
        service: services::supergraph::BoxService,
    ) -> services::supergraph::BoxService {
        self.supergraph_service(service)
    }

    fn execution_service(
        &self,
        service: services::execution::BoxService,
    ) -> services::execution::BoxService {
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
    "apollo",
    "coprocessor",
    CoprocessorPlugin<HTTPClientService>
);

// -------------------------------------------------------------------------------------------------------

/// This is where the real implementation happens.
/// The structure above calls the functions defined below.
///
/// This structure is generic over the HTTP Service so we can test the plugin seamlessly.
#[derive(Debug)]
struct CoprocessorPlugin<C>
where
    C: Service<hyper::Request<Body>, Response = hyper::Response<Body>, Error = BoxError>
        + Clone
        + Send
        + Sync
        + 'static,
    <C as tower::Service<http::Request<Body>>>::Future: Send + Sync + 'static,
{
    http_client: C,
    configuration: Conf,
    sdl: Arc<String>,
}

impl<C> CoprocessorPlugin<C>
where
    C: Service<hyper::Request<Body>, Response = hyper::Response<Body>, Error = BoxError>
        + Clone
        + Send
        + Sync
        + 'static,
    <C as tower::Service<http::Request<Body>>>::Future: Send + Sync + 'static,
{
    fn new(http_client: C, configuration: Conf, sdl: Arc<String>) -> Result<Self, BoxError> {
        Ok(Self {
            http_client,
            configuration,
            sdl,
        })
    }

    fn router_service(&self, service: router::BoxService) -> router::BoxService {
        self.configuration.router.as_service(
            self.http_client.clone(),
            service,
            self.configuration.url.clone(),
            self.sdl.clone(),
        )
    }

    fn supergraph_service(
        &self,
        service: services::supergraph::BoxService,
    ) -> services::supergraph::BoxService {
        self.configuration.supergraph.as_service(
            self.http_client.clone(),
            service,
            self.configuration.url.clone(),
            self.sdl.clone(),
        )
    }

    fn execution_service(
        &self,
        service: services::execution::BoxService,
    ) -> services::execution::BoxService {
        self.configuration.execution.as_service(
            self.http_client.clone(),
            service,
            self.configuration.url.clone(),
            self.sdl.clone(),
        )
    }

    fn subgraph_service(&self, name: &str, service: subgraph::BoxService) -> subgraph::BoxService {
        self.configuration.subgraph.all.as_service(
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
pub(super) struct RouterRequestConf {
    /// Send the headers
    pub(super) headers: bool,
    /// Send the context
    pub(super) context: bool,
    /// Send the body
    pub(super) body: bool,
    /// Send the SDL
    pub(super) sdl: bool,
    /// Send the path
    pub(super) path: bool,
    /// Send the method
    pub(super) method: bool,
    /// Handles the request without waiting for the coprocessor to respond
    pub(super) detached: bool,
    /// The url you'd like to offload processing to
    #[schemars(with = "String")]
    pub(super) url: Option<Url>,
}

/// What information is passed to a router request/response stage
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub(super) struct RouterResponseConf {
    /// Send the headers
    pub(super) headers: bool,
    /// Send the context
    pub(super) context: bool,
    /// Send the body
    pub(super) body: bool,
    /// Send the SDL
    pub(super) sdl: bool,
    /// Send the HTTP status
    pub(super) status_code: bool,
    /// Handles the response without waiting for the coprocessor to respond
    pub(super) detached: bool,
    /// The url you'd like to offload processing to
    #[schemars(with = "String")]
    pub(super) url: Option<Url>,
}

/// What information is passed to a subgraph request/response stage
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub(super) struct SubgraphRequestConf {
    /// Send the headers
    pub(super) headers: bool,
    /// Send the context
    pub(super) context: bool,
    /// Send the body
    pub(super) body: bool,
    /// Send the subgraph URI
    pub(super) uri: bool,
    /// Send the method URI
    pub(super) method: bool,
    /// Send the service name
    pub(super) service_name: bool,
    /// Handles the request without waiting for the coprocessor to respond
    pub(super) detached: bool,
    /// The url you'd like to offload processing to
    #[schemars(with = "String")]
    pub(super) url: Option<Url>,
}

/// What information is passed to a subgraph request/response stage
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub(super) struct SubgraphResponseConf {
    /// Send the headers
    pub(super) headers: bool,
    /// Send the context
    pub(super) context: bool,
    /// Send the body
    pub(super) body: bool,
    /// Send the service name
    pub(super) service_name: bool,
    /// Send the http status
    pub(super) status_code: bool,
    /// Handles the response without waiting for the coprocessor to respond
    pub(super) detached: bool,
    /// The url you'd like to offload processing to
    #[schemars(with = "String")]
    pub(super) url: Option<Url>,
}

/// Configures the externalization plugin
#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct Conf {
    /// The url you'd like to offload processing to
    #[schemars(with = "String")]
    url: Url,
    /// The timeout for external requests
    #[serde(deserialize_with = "humantime_serde::deserialize")]
    #[schemars(with = "String", default = "default_timeout")]
    #[serde(default = "default_timeout")]
    timeout: Duration,
    /// The router stage request/response configuration
    #[serde(default)]
    router: RouterStage,
    /// The supergraph stage request/response configuration
    #[serde(default)]
    supergraph: supergraph::SupergraphStage,
    /// The execution stage request/response configuration
    #[serde(default)]
    execution: execution::ExecutionStage,
    /// The subgraph stage request/response configuration
    #[serde(default)]
    subgraph: SubgraphStages,
}

fn default_timeout() -> Duration {
    DEFAULT_EXTERNALIZATION_TIMEOUT
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize, JsonSchema)]
#[serde(default)]
pub(super) struct RouterStage {
    /// The request configuration
    pub(super) request: RouterRequestConf,
    /// The response configuration
    pub(super) response: RouterResponseConf,
}

impl RouterStage {
    pub(crate) fn as_service<C>(
        &self,
        http_client: C,
        service: router::BoxService,
        coprocessor_url: Url,
        sdl: Arc<String>,
    ) -> router::BoxService
    where
        C: Service<hyper::Request<Body>, Response = hyper::Response<Body>, Error = BoxError>
            + Clone
            + Send
            + Sync
            + 'static,
        <C as tower::Service<http::Request<Body>>>::Future: Send + 'static,
    {
        let request_layer = (self.request != Default::default()).then_some({
            let request_config = self.request.clone();
            let coprocessor_url = request_config
                .url
                .clone()
                .unwrap_or_else(|| coprocessor_url.clone());
            let http_client = http_client.clone();
            let sdl = sdl.clone();

            OneShotAsyncCheckpointLayer::new(move |request: router::Request| {
                let request_config = request_config.clone();
                let coprocessor_url = coprocessor_url.clone();
                let http_client = http_client.clone();
                let sdl = sdl.clone();

                async move {
                    let mut succeeded = true;
                    let result = process_router_request_stage(
                        http_client,
                        coprocessor_url,
                        sdl,
                        request,
                        request_config,
                    )
                    .await
                    .map_err(|error| {
                        succeeded = false;
                        tracing::error!(
                            "external extensibility: router request stage error: {error}"
                        );
                        error
                    });
                    u64_counter!(
                        "apollo.router.operations.coprocessor",
                        "Total operations with co-processors enabled",
                        1,
                        "coprocessor.stage" = PipelineStep::RouterRequest,
                        "coprocessor.succeeded" = succeeded
                    );
                    result
                }
            })
        });

        let response_layer = (self.response != Default::default()).then_some({
            let response_config = self.response.clone();
            MapFutureLayer::new(move |fut| {
                let sdl = sdl.clone();
                let coprocessor_url = response_config
                    .url
                    .clone()
                    .unwrap_or_else(|| coprocessor_url.clone());
                let http_client = http_client.clone();
                let response_config = response_config.clone();

                async move {
                    let response: router::Response = fut.await?;

                    let mut succeeded = true;
                    let result = process_router_response_stage(
                        http_client,
                        coprocessor_url,
                        sdl,
                        response,
                        response_config,
                    )
                    .await
                    .map_err(|error| {
                        succeeded = false;
                        tracing::error!(
                            "external extensibility: router response stage error: {error}"
                        );
                        error
                    });
                    u64_counter!(
                        "apollo.router.operations.coprocessor",
                        "Total operations with co-processors enabled",
                        1,
                        "coprocessor.stage" = PipelineStep::RouterResponse,
                        "coprocessor.succeeded" = succeeded
                    );
                    result
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
            .service(service)
            .boxed()
    }
}

// -----------------------------------------------------------------------------------------

/// What information is passed to a subgraph request/response stage
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub(super) struct SubgraphStages {
    #[serde(default)]
    pub(super) all: SubgraphStage,
}

/// What information is passed to a subgraph request/response stage
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub(super) struct SubgraphStage {
    #[serde(default)]
    pub(super) request: SubgraphRequestConf,
    #[serde(default)]
    pub(super) response: SubgraphResponseConf,
}

impl SubgraphStage {
    pub(crate) fn as_service<C>(
        &self,
        http_client: C,
        service: subgraph::BoxService,
        coprocessor_url: Url,
        service_name: String,
    ) -> subgraph::BoxService
    where
        C: Service<hyper::Request<Body>, Response = hyper::Response<Body>, Error = BoxError>
            + Clone
            + Send
            + Sync
            + 'static,
        <C as tower::Service<http::Request<Body>>>::Future: Send + 'static,
    {
        let request_layer = (self.request != Default::default()).then_some({
            let request_config = self.request.clone();
            let http_client = http_client.clone();
            let coprocessor_url = request_config
                .url
                .clone()
                .unwrap_or_else(|| coprocessor_url.clone());
            let service_name = service_name.clone();
            OneShotAsyncCheckpointLayer::new(move |request: subgraph::Request| {
                let http_client = http_client.clone();
                let coprocessor_url = coprocessor_url.clone();
                let service_name = service_name.clone();
                let request_config = request_config.clone();

                async move {
                    let mut succeeded = true;
                    let result = process_subgraph_request_stage(
                        http_client,
                        coprocessor_url,
                        service_name,
                        request,
                        request_config,
                    )
                    .await
                    .map_err(|error| {
                        succeeded = false;
                        tracing::error!(
                            "external extensibility: subgraph request stage error: {error}"
                        );
                        error
                    });
                    u64_counter!(
                        "apollo.router.operations.coprocessor",
                        "Total operations with co-processors enabled",
                        1,
                        "coprocessor.stage" = PipelineStep::SubgraphRequest,
                        "coprocessor.succeeded" = succeeded
                    );
                    result
                }
            })
        });

        let response_layer = (self.response != Default::default()).then_some({
            let response_config = self.response.clone();

            MapFutureLayer::new(move |fut| {
                let http_client = http_client.clone();
                let response_config = response_config.clone();
                let coprocessor_url = response_config
                    .url
                    .clone()
                    .unwrap_or_else(|| coprocessor_url.clone());
                let service_name = service_name.clone();

                async move {
                    let response: subgraph::Response = fut.await?;

                    let mut succeeded = true;
                    let result = process_subgraph_response_stage(
                        http_client,
                        coprocessor_url,
                        service_name,
                        response,
                        response_config,
                    )
                    .await
                    .map_err(|error| {
                        succeeded = false;
                        tracing::error!(
                            "external extensibility: subgraph response stage error: {error}"
                        );
                        error
                    });
                    u64_counter!(
                        "apollo.router.operations.coprocessor",
                        "Total operations with co-processors enabled",
                        1,
                        "coprocessor.stage" = PipelineStep::SubgraphResponse,
                        "coprocessor.succeeded" = succeeded
                    );
                    result
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
            .service(service)
            .boxed()
    }
}

// -----------------------------------------------------------------------------------------
async fn process_router_request_stage<C>(
    http_client: C,
    coprocessor_url: Url,
    sdl: Arc<String>,
    mut request: router::Request,
    request_config: RouterRequestConf,
) -> Result<ControlFlow<router::Response, router::Request>, BoxError>
where
    C: Service<hyper::Request<Body>, Response = hyper::Response<Body>, Error = BoxError>
        + Clone
        + Send
        + Sync
        + 'static,
    <C as tower::Service<http::Request<Body>>>::Future: Send + 'static,
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

    // HTTP GET requests don't have a body
    let body_to_send = request_config
        .body
        .then(|| String::from_utf8(bytes.to_vec()))
        .transpose()
        .unwrap_or_default();

    let path_to_send = request_config.path.then(|| parts.uri.to_string());

    let context_to_send = request_config.context.then(|| request.context.clone());
    let sdl_to_send = request_config.sdl.then(|| sdl.clone().to_string());

    let payload = Externalizable::router_builder()
        .stage(PipelineStep::RouterRequest)
        .control(Control::default())
        .id(request.context.id.clone())
        .and_headers(headers_to_send)
        .and_body(body_to_send)
        .and_context(context_to_send)
        .and_sdl(sdl_to_send)
        .and_path(path_to_send)
        .method(parts.method.to_string())
        .build();

    if request_config.detached {
        tokio::task::spawn(async move {
            tracing::debug!(?payload, "externalized output");
            let start = Instant::now();
            let _ = payload.call(http_client, &coprocessor_url).await;
            let duration = start.elapsed().as_secs_f64();
            tracing::info!(
                histogram.apollo.router.operations.coprocessor.duration = duration,
                coprocessor.stage = %PipelineStep::RouterRequest,
            );
        });

        request.router_request = http::Request::from_parts(parts, Body::from(bytes));
        return Ok(ControlFlow::Continue(request));
    }

    tracing::debug!(?payload, "externalized output");
    let guard = request.context.enter_active_request();
    let start = Instant::now();
    let co_processor_result = payload.call(http_client, &coprocessor_url).await;
    let duration = start.elapsed().as_secs_f64();
    drop(guard);
    tracing::info!(
        histogram.apollo.router.operations.coprocessor.duration = duration,
        coprocessor.stage = %PipelineStep::RouterRequest,
    );

    tracing::debug!(?co_processor_result, "co-processor returned");
    let mut co_processor_output = co_processor_result?;

    validate_coprocessor_output(&co_processor_output, PipelineStep::RouterRequest)?;
    // unwrap is safe here because validate_coprocessor_output made sure control is available
    let control = co_processor_output.control.expect("validated above; qed");

    // Thirdly, we need to interpret the control flow which may have been
    // updated by our co-processor and decide if we should proceed or stop.

    if matches!(control, Control::Break(_)) {
        // Ensure the code is a valid http status code
        let code = control.get_http_status()?;

        // At this point our body is a String. Try to get a valid JSON value from it
        let body_as_value = co_processor_output
            .body
            .as_ref()
            .and_then(|b| serde_json::from_str(b).ok())
            .unwrap_or(serde_json::Value::Null);
        // Now we have some JSON, let's see if it's the right "shape" to create a graphql_response.
        // If it isn't, we create a graphql error response
        let graphql_response: crate::graphql::Response = match body_as_value {
            serde_json::Value::Null => crate::graphql::Response::builder()
                .errors(vec![Error::builder()
                    .message(co_processor_output.body.take().unwrap_or_default())
                    .extension_code(COPROCESSOR_ERROR_EXTENSION)
                    .build()])
                .build(),
            _ => serde_json::from_value(body_as_value).unwrap_or_else(|error| {
                crate::graphql::Response::builder()
                    .errors(vec![Error::builder()
                        .message(format!(
                            "couldn't deserialize coprocessor output body: {error}"
                        ))
                        .extension_code(COPROCESSOR_DESERIALIZATION_ERROR_EXTENSION)
                        .build()])
                    .build()
            }),
        };

        let res = router::Response::builder()
            .errors(graphql_response.errors)
            .extensions(graphql_response.extensions)
            .status_code(code)
            .context(request.context);

        let mut res = match (graphql_response.label, graphql_response.data) {
            (Some(label), Some(data)) => res.label(label).data(data).build()?,
            (Some(label), None) => res.label(label).build()?,
            (None, Some(data)) => res.data(data).build()?,
            (None, None) => res.build()?,
        };
        if let Some(headers) = co_processor_output.headers {
            *res.response.headers_mut() = internalize_header_map(headers)?;
        }

        if let Some(context) = co_processor_output.context {
            for (key, value) in context.try_into_iter()? {
                res.context.upsert_json_value(key, move |_current| value);
            }
        }

        return Ok(ControlFlow::Break(res));
    }

    // Finally, process our reply and act on the contents. Our processing logic is
    // that we replace "bits" of our incoming request with the updated bits if they
    // are present in our co_processor_output.

    let new_body = match co_processor_output.body {
        Some(bytes) => Body::from(bytes),
        None => Body::from(bytes),
    };

    request.router_request = http::Request::from_parts(parts, new_body);

    if let Some(context) = co_processor_output.context {
        for (key, value) in context.try_into_iter()? {
            request
                .context
                .upsert_json_value(key, move |_current| value);
        }
    }

    if let Some(headers) = co_processor_output.headers {
        *request.router_request.headers_mut() = internalize_header_map(headers)?;
    }

    Ok(ControlFlow::Continue(request))
}

async fn process_router_response_stage<C>(
    http_client: C,
    coprocessor_url: Url,
    sdl: Arc<String>,
    mut response: router::Response,
    response_config: RouterResponseConf,
) -> Result<router::Response, BoxError>
where
    C: Service<hyper::Request<Body>, Response = hyper::Response<Body>, Error = BoxError>
        + Clone
        + Send
        + Sync
        + 'static,
    <C as tower::Service<http::Request<Body>>>::Future: Send + 'static,
{
    // split the response into parts + body
    let (parts, body) = response.response.into_parts();

    // we split the body (which is a stream) into first response + rest of responses,
    // for which we will implement mapping later
    let (first, rest): (Option<Result<Bytes, hyper::Error>>, Body) = body.into_future().await;

    // If first is None, or contains an error we return an error
    let opt_first: Option<Bytes> = first.and_then(|f| f.ok());
    let bytes = match opt_first {
        Some(b) => b,
        None => {
            tracing::error!(
                "Coprocessor cannot convert body into future due to problem with first part"
            );
            return Err(BoxError::from(
                "Coprocessor cannot convert body into future due to problem with first part",
            ));
        }
    };

    // Now we process our first chunk of response
    // Encode headers, body, status, context, sdl to create a payload
    let headers_to_send = response_config
        .headers
        .then(|| externalize_header_map(&parts.headers))
        .transpose()?;
    let body_to_send = response_config
        .body
        .then(|| std::str::from_utf8(&bytes).map(|s| s.to_string()))
        .transpose()?;
    let status_to_send = response_config.status_code.then(|| parts.status.as_u16());
    let context_to_send = response_config.context.then(|| response.context.clone());
    let sdl_to_send = response_config.sdl.then(|| sdl.clone().to_string());

    let payload = Externalizable::router_builder()
        .stage(PipelineStep::RouterResponse)
        .id(response.context.id.clone())
        .and_headers(headers_to_send)
        .and_body(body_to_send)
        .and_context(context_to_send)
        .and_status_code(status_to_send)
        .and_sdl(sdl_to_send.clone())
        .build();

    if response_config.detached {
        let context = response.context.clone();

        let http_client2 = http_client.clone();
        let coprocessor_url2 = coprocessor_url.clone();
        // Second, call our co-processor and get a reply.
        tokio::task::spawn(async move {
            tracing::debug!(?payload, "externalized output");
            let guard = context.enter_active_request();
            let start = Instant::now();
            let _ = payload.call(http_client.clone(), &coprocessor_url).await;
            let duration = start.elapsed().as_secs_f64();
            drop(guard);
            tracing::info!(
                histogram.apollo.router.operations.coprocessor.duration = duration,
                coprocessor.stage = %PipelineStep::RouterResponse,
            );
        });

        let map_context = response.context.clone();
        // Map the rest of our body to process subsequent chunks of response
        let mapped_stream = rest.map(move |deferred_response| {
            let generator_client = http_client2.clone();
            let generator_coprocessor_url = coprocessor_url2.clone();
            let generator_map_context = map_context.clone();
            let generator_sdl_to_send = sdl_to_send.clone();
            let generator_id = map_context.id.clone();

            if let Ok(deferred_response) = &deferred_response {
                let bytes = deferred_response.to_vec();
                let body_to_send = response_config
                    .body
                    .then(|| String::from_utf8(bytes.clone()))
                    .transpose()
                    .unwrap();
                let context_to_send = response_config
                    .context
                    .then(|| generator_map_context.clone());

                // Note: We deliberately DO NOT send headers or status_code even if the user has
                // requested them. That's because they are meaningless on a deferred response and
                // providing them will be a source of confusion.
                let payload = Externalizable::router_builder()
                    .stage(PipelineStep::RouterResponse)
                    .id(generator_id)
                    .and_body(body_to_send)
                    .and_context(context_to_send)
                    .and_sdl(generator_sdl_to_send)
                    .build();
                tokio::task::spawn(async move {
                    // Second, call our co-processor and get a reply.
                    tracing::debug!(?payload, "externalized output");
                    let start = Instant::now();
                    let _ = payload
                        .call(generator_client, &generator_coprocessor_url)
                        .await;
                    let duration = start.elapsed().as_secs_f64();
                    tracing::info!(
                        histogram.apollo.router.operations.coprocessor.duration = duration,
                        coprocessor.stage = %PipelineStep::RouterResponse,
                    );
                });
            }

            deferred_response
        });

        // Create our response stream which consists of the bytes from our first body chained with the
        // rest of the responses in our mapped stream.
        let final_stream = once(ready(Ok(bytes))).chain(mapped_stream).boxed();

        response.response = http::Response::from_parts(parts, Body::wrap_stream(final_stream));
        return Ok(response);
    }

    // Second, call our co-processor and get a reply.
    tracing::debug!(?payload, "externalized output");
    let guard = response.context.enter_active_request();
    let start = Instant::now();
    let co_processor_result = payload.call(http_client.clone(), &coprocessor_url).await;
    let duration = start.elapsed().as_secs_f64();
    drop(guard);
    tracing::info!(
        histogram.apollo.router.operations.coprocessor.duration = duration,
        coprocessor.stage = %PipelineStep::RouterResponse,
    );

    tracing::debug!(?co_processor_result, "co-processor returned");
    let co_processor_output = co_processor_result?;

    validate_coprocessor_output(&co_processor_output, PipelineStep::RouterResponse)?;

    // Third, process our reply and act on the contents. Our processing logic is
    // that we replace "bits" of our incoming response with the updated bits if they
    // are present in our co_processor_output. If they aren't present, just use the
    // bits that we sent to the co_processor.

    let new_body = match co_processor_output.body {
        Some(bytes) => Body::from(bytes),
        None => Body::from(bytes),
    };

    response.response = http::Response::from_parts(parts, new_body);

    if let Some(control) = co_processor_output.control {
        *response.response.status_mut() = control.get_http_status()?
    }

    if let Some(context) = co_processor_output.context {
        for (key, value) in context.try_into_iter()? {
            response
                .context
                .upsert_json_value(key, move |_current| value);
        }
    }

    if let Some(headers) = co_processor_output.headers {
        *response.response.headers_mut() = internalize_header_map(headers)?;
    }

    // Now break our co-processor modified response back into parts
    let (parts, body) = response.response.into_parts();

    // Clone all the bits we need
    let context = response.context.clone();
    let map_context = response.context.clone();

    // Map the rest of our body to process subsequent chunks of response
    let mapped_stream = rest
        .map_err(BoxError::from)
        .and_then(move |deferred_response| {
            let generator_client = http_client.clone();
            let generator_coprocessor_url = coprocessor_url.clone();
            let generator_map_context = map_context.clone();
            let generator_sdl_to_send = sdl_to_send.clone();
            let generator_id = map_context.id.clone();

            async move {
                let bytes = deferred_response.to_vec();
                let body_to_send = response_config
                    .body
                    .then(|| String::from_utf8(bytes.clone()))
                    .transpose()?;
                let context_to_send = response_config
                    .context
                    .then(|| generator_map_context.clone());

                // Note: We deliberately DO NOT send headers or status_code even if the user has
                // requested them. That's because they are meaningless on a deferred response and
                // providing them will be a source of confusion.
                let payload = Externalizable::router_builder()
                    .stage(PipelineStep::RouterResponse)
                    .id(generator_id)
                    .and_body(body_to_send)
                    .and_context(context_to_send)
                    .and_sdl(generator_sdl_to_send)
                    .build();

                // Second, call our co-processor and get a reply.
                tracing::debug!(?payload, "externalized output");
                let guard = generator_map_context.enter_active_request();
                let co_processor_result = payload
                    .call(generator_client, &generator_coprocessor_url)
                    .await;
                drop(guard);
                tracing::debug!(?co_processor_result, "co-processor returned");
                let co_processor_output = co_processor_result?;

                validate_coprocessor_output(&co_processor_output, PipelineStep::RouterResponse)?;

                // Third, process our reply and act on the contents. Our processing logic is
                // that we replace "bits" of our incoming response with the updated bits if they
                // are present in our co_processor_output. If they aren't present, just use the
                // bits that we sent to the co_processor.
                let final_bytes: Bytes = match co_processor_output.body {
                    Some(bytes) => bytes.into(),
                    None => bytes.into(),
                };

                if let Some(context) = co_processor_output.context {
                    for (key, value) in context.try_into_iter()? {
                        generator_map_context.upsert_json_value(key, move |_current| value);
                    }
                }

                // We return the final_bytes into our stream of response chunks
                Ok(final_bytes)
            }
        });

    // Create our response stream which consists of the bytes from our first body chained with the
    // rest of the responses in our mapped stream.
    let bytes = hyper::body::to_bytes(body).await.map_err(BoxError::from);
    let final_stream = once(ready(bytes)).chain(mapped_stream).boxed();

    // Finally, return a response which has a Body that wraps our stream of response chunks.
    Ok(router::Response {
        context,
        response: http::Response::from_parts(parts, Body::wrap_stream(final_stream)),
    })
}
// -----------------------------------------------------------------------------------------------------

async fn process_subgraph_request_stage<C>(
    http_client: C,
    coprocessor_url: Url,
    service_name: String,
    mut request: subgraph::Request,
    request_config: SubgraphRequestConf,
) -> Result<ControlFlow<subgraph::Response, subgraph::Request>, BoxError>
where
    C: Service<hyper::Request<Body>, Response = hyper::Response<Body>, Error = BoxError>
        + Clone
        + Send
        + Sync
        + 'static,
    <C as tower::Service<http::Request<Body>>>::Future: Send + 'static,
{
    // Call into our out of process processor with a body of our body
    // First, extract the data we need from our request and prepare our
    // external call. Use our configuration to figure out which data to send.
    let (parts, body) = request.subgraph_request.into_parts();

    let headers_to_send = request_config
        .headers
        .then(|| externalize_header_map(&parts.headers))
        .transpose()?;

    let body_to_send = request_config
        .body
        .then(|| serde_json::to_value(&body))
        .transpose()?;
    let context_to_send = request_config.context.then(|| request.context.clone());
    let uri = request_config.uri.then(|| parts.uri.to_string());
    let service_name = request_config.service_name.then_some(service_name);

    let payload = Externalizable::subgraph_builder()
        .stage(PipelineStep::SubgraphRequest)
        .control(Control::default())
        .id(request.context.id.clone())
        .and_headers(headers_to_send)
        .and_body(body_to_send)
        .and_context(context_to_send)
        .method(parts.method.to_string())
        .and_service_name(service_name)
        .and_uri(uri)
        .build();

    if request_config.detached {
        tokio::task::spawn(async move {
            tracing::debug!(?payload, "externalized output");
            let start = Instant::now();
            let _ = payload.call(http_client, &coprocessor_url).await;
            let duration = start.elapsed().as_secs_f64();
            tracing::info!(
                histogram.apollo.router.operations.coprocessor.duration = duration,
                coprocessor.stage = %PipelineStep::SubgraphRequest,
            );
        });

        request.subgraph_request = http::Request::from_parts(parts, body);
        return Ok(ControlFlow::Continue(request));
    }

    tracing::debug!(?payload, "externalized output");
    let guard = request.context.enter_active_request();
    let start = Instant::now();
    let co_processor_result = payload.call(http_client, &coprocessor_url).await;
    let duration = start.elapsed().as_secs_f64();
    drop(guard);
    tracing::info!(
        histogram.apollo.router.operations.coprocessor.duration = duration,
        coprocessor.stage = %PipelineStep::SubgraphRequest,
    );

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
                match co_processor_output.body.unwrap_or(serde_json::Value::Null) {
                    serde_json::Value::String(s) => crate::graphql::Response::builder()
                        .errors(vec![Error::builder()
                            .message(s)
                            .extension_code(COPROCESSOR_ERROR_EXTENSION)
                            .build()])
                        .build(),
                    value => serde_json::from_value(value).unwrap_or_else(|error| {
                        crate::graphql::Response::builder()
                            .errors(vec![Error::builder()
                                .message(format!(
                                    "couldn't deserialize coprocessor output body: {error}"
                                ))
                                .extension_code(COPROCESSOR_DESERIALIZATION_ERROR_EXTENSION)
                                .build()])
                            .build()
                    }),
                };

            let mut http_response = http::Response::builder()
                .status(code)
                .body(graphql_response)?;
            if let Some(headers) = co_processor_output.headers {
                *http_response.headers_mut() = internalize_header_map(headers)?;
            }

            let subgraph_response = subgraph::Response {
                response: http_response,
                context: request.context,
            };

            if let Some(context) = co_processor_output.context {
                for (key, value) in context.try_into_iter()? {
                    subgraph_response
                        .context
                        .upsert_json_value(key, move |_current| value);
                }
            }

            subgraph_response
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
        for (key, value) in context.try_into_iter()? {
            request
                .context
                .upsert_json_value(key, move |_current| value);
        }
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
    coprocessor_url: Url,
    service_name: String,
    mut response: subgraph::Response,
    response_config: SubgraphResponseConf,
) -> Result<subgraph::Response, BoxError>
where
    C: Service<hyper::Request<Body>, Response = hyper::Response<Body>, Error = BoxError>
        + Clone
        + Send
        + Sync
        + 'static,
    <C as tower::Service<http::Request<Body>>>::Future: Send + 'static,
{
    // Call into our out of process processor with a body of our body
    // First, extract the data we need from our response and prepare our
    // external call. Use our configuration to figure out which data to send.

    let (parts, body) = response.response.into_parts();

    let headers_to_send = response_config
        .headers
        .then(|| externalize_header_map(&parts.headers))
        .transpose()?;

    let status_to_send = response_config.status_code.then(|| parts.status.as_u16());

    let body_to_send = response_config
        .body
        .then(|| serde_json::to_value(&body))
        .transpose()?;
    let context_to_send = response_config.context.then(|| response.context.clone());
    let service_name = response_config.service_name.then_some(service_name);

    let payload = Externalizable::subgraph_builder()
        .stage(PipelineStep::SubgraphResponse)
        .id(response.context.id.clone())
        .and_headers(headers_to_send)
        .and_body(body_to_send)
        .and_context(context_to_send)
        .and_status_code(status_to_send)
        .and_service_name(service_name)
        .build();

    if response_config.detached {
        tokio::task::spawn(async move {
            tracing::debug!(?payload, "externalized output");
            let start = Instant::now();
            let _ = payload.call(http_client, &coprocessor_url).await;
            let duration = start.elapsed().as_secs_f64();
            tracing::info!(
                histogram.apollo.router.operations.coprocessor.duration = duration,
                coprocessor.stage = %PipelineStep::SubgraphResponse,
            );
        });

        response.response = http::Response::from_parts(parts, body);
        return Ok(response);
    }

    tracing::debug!(?payload, "externalized output");
    let guard = response.context.enter_active_request();
    let start = Instant::now();
    let co_processor_result = payload.call(http_client, &coprocessor_url).await;
    let duration = start.elapsed().as_secs_f64();
    drop(guard);
    tracing::info!(
        histogram.apollo.router.operations.coprocessor.duration = duration,
        coprocessor.stage = %PipelineStep::SubgraphResponse,
    );

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

    if let Some(control) = co_processor_output.control {
        *response.response.status_mut() = control.get_http_status()?
    }

    if let Some(context) = co_processor_output.context {
        for (key, value) in context.try_into_iter()? {
            response
                .context
                .upsert_json_value(key, move |_current| value);
        }
    }

    if let Some(headers) = co_processor_output.headers {
        *response.response.headers_mut() = internalize_header_map(headers)?;
    }

    Ok(response)
}

// -----------------------------------------------------------------------------------------

fn validate_coprocessor_output<T>(
    co_processor_output: &Externalizable<T>,
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

/// Convert a HashMap into a HeaderMap
pub(super) fn internalize_header_map(
    input: HashMap<String, Vec<String>>,
) -> Result<HeaderMap<HeaderValue>, BoxError> {
    // better than nothing even though it doesnt account for the values len
    let mut output = HeaderMap::with_capacity(input.len());
    for (k, values) in input
        .into_iter()
        .filter(|(k, _)| k != header::CONTENT_LENGTH.as_str())
    {
        for v in values {
            let key = HeaderName::from_str(k.as_ref())?;
            let value = HeaderValue::from_str(v.as_ref())?;
            output.append(key, value);
        }
    }
    Ok(output)
}
