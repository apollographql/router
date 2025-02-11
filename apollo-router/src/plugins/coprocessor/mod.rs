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
use http_body_util::BodyExt;
use hyper_rustls::ConfigBuilderExt;
use hyper_rustls::HttpsConnector;
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::rt::TokioExecutor;
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

use crate::configuration::shared::Client;
use crate::error::Error;
use crate::graphql;
use crate::layers::async_checkpoint::AsyncCheckpointLayer;
use crate::layers::ServiceBuilderExt;
use crate::plugin::Plugin;
use crate::plugin::PluginInit;
use crate::plugins::telemetry::config_new::conditions::Condition;
use crate::plugins::telemetry::config_new::selectors::RouterSelector;
use crate::plugins::telemetry::config_new::selectors::SubgraphSelector;
use crate::plugins::traffic_shaping::Http2Config;
use crate::register_plugin;
use crate::services;
use crate::services::external::externalize_header_map;
use crate::services::external::Control;
use crate::services::external::Externalizable;
use crate::services::external::PipelineStep;
use crate::services::external::DEFAULT_EXTERNALIZATION_TIMEOUT;
use crate::services::external::EXTERNALIZABLE_VERSION;
use crate::services::hickory_dns_connector::new_async_http_connector;
use crate::services::hickory_dns_connector::AsyncHyperResolver;
use crate::services::router;
use crate::services::router::body::RouterBody;
use crate::services::subgraph;

#[cfg(test)]
mod test;

mod execution;
mod supergraph;

pub(crate) const EXTERNAL_SPAN_NAME: &str = "external_plugin";
const POOL_IDLE_TIMEOUT_DURATION: Option<Duration> = Some(Duration::from_secs(5));
const COPROCESSOR_ERROR_EXTENSION: &str = "ERROR";
const COPROCESSOR_DESERIALIZATION_ERROR_EXTENSION: &str = "EXTERNAL_DESERIALIZATION_ERROR";

type MapFn = fn(http::Response<hyper::body::Incoming>) -> http::Response<RouterBody>;

type HTTPClientService = tower::util::MapResponse<
    tower::timeout::Timeout<
        hyper_util::client::legacy::Client<
            HttpsConnector<HttpConnector<AsyncHyperResolver>>,
            RouterBody,
        >,
    >,
    MapFn,
>;

#[async_trait::async_trait]
impl Plugin for CoprocessorPlugin<HTTPClientService> {
    type Config = Conf;

    async fn new(init: PluginInit<Self::Config>) -> Result<Self, BoxError> {
        let client_config = init.config.client.clone().unwrap_or_default();
        let mut http_connector =
            new_async_http_connector(client_config.dns_resolution_strategy.unwrap_or_default())?;
        http_connector.set_nodelay(true);
        http_connector.set_keepalive(Some(std::time::Duration::from_secs(60)));
        http_connector.enforce_http(false);

        // Enable crypto
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

        let tls_config = rustls::ClientConfig::builder()
            .with_native_roots()?
            .with_no_client_auth();

        let builder = hyper_rustls::HttpsConnectorBuilder::new()
            .with_tls_config(tls_config)
            .https_or_http()
            .enable_http1();

        let experimental_http2 = client_config.experimental_http2.unwrap_or_default();
        let connector = if experimental_http2 != Http2Config::Disable {
            builder.enable_http2().wrap_connector(http_connector)
        } else {
            builder.wrap_connector(http_connector)
        };

        let http_client = ServiceBuilder::new()
            .map_response(
                |http_response: http::Response<hyper::body::Incoming>| -> http::Response<RouterBody> {
                    let (parts, body) = http_response.into_parts();
                    http::Response::from_parts(parts, body.map_err(axum::Error::new).boxed_unsync())
                } as MapFn,
            )
            .layer(TimeoutLayer::new(init.config.timeout))
            .service(
                hyper_util::client::legacy::Client::builder(TokioExecutor::new())
                    .http2_only(experimental_http2 == Http2Config::Http2Only)
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
    C: Service<http::Request<RouterBody>, Response = http::Response<RouterBody>, Error = BoxError>
        + Clone
        + Send
        + Sync
        + 'static,
    <C as tower::Service<http::Request<RouterBody>>>::Future: Send + 'static,
{
    http_client: C,
    configuration: Conf,
    sdl: Arc<String>,
}

impl<C> CoprocessorPlugin<C>
where
    C: Service<http::Request<RouterBody>, Response = http::Response<RouterBody>, Error = BoxError>
        + Clone
        + Send
        + Sync
        + 'static,
    <C as tower::Service<http::Request<RouterBody>>>::Future: Send + 'static,
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
    /// Condition to trigger this stage
    #[serde(skip_serializing)]
    pub(super) condition: Option<Condition<RouterSelector>>,
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
    /// The url you'd like to offload processing to
    #[schemars(with = "String")]
    pub(super) url: Option<Url>,
}

/// What information is passed to a router request/response stage
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub(super) struct RouterResponseConf {
    /// Condition to trigger this stage
    #[serde(skip_serializing)]
    pub(super) condition: Option<Condition<RouterSelector>>,
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
    /// The url you'd like to offload processing to
    #[schemars(with = "String")]
    pub(super) url: Option<Url>,
}

/// What information is passed to a subgraph request/response stage
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub(super) struct SubgraphRequestConf {
    /// Condition to trigger this stage
    #[serde(skip_serializing)]
    pub(super) condition: Option<Condition<SubgraphSelector>>,
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
    /// The url you'd like to offload processing to
    #[schemars(with = "String")]
    pub(super) url: Option<Url>,
    /// Send the subgraph request id
    pub(super) subgraph_request_id: bool,
}

/// What information is passed to a subgraph request/response stage
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub(super) struct SubgraphResponseConf {
    /// Condition to trigger this stage
    #[serde(skip_serializing)]
    pub(super) condition: Option<Condition<SubgraphSelector>>,
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
    /// The url you'd like to offload processing to
    #[schemars(with = "String")]
    pub(super) url: Option<Url>,
    /// Send the subgraph request id
    pub(super) subgraph_request_id: bool,
}

/// Configures the externalization plugin
#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct Conf {
    /// The url you'd like to offload processing to
    #[schemars(with = "String")]
    url: Url,
    client: Option<Client>,
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

fn record_coprocessor_duration(stage: PipelineStep, duration: Duration) {
    f64_histogram!(
        "apollo.router.operations.coprocessor.duration",
        "Time spent waiting for the coprocessor to answer, in seconds",
        duration.as_secs_f64(),
        coprocessor.stage = stage.to_string()
    );
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
        C: Service<
                http::Request<RouterBody>,
                Response = http::Response<RouterBody>,
                Error = BoxError,
            > + Clone
            + Send
            + Sync
            + 'static,
        <C as tower::Service<http::Request<RouterBody>>>::Future: Send + 'static,
    {
        let request_layer = (self.request != Default::default()).then_some({
            let request_config = self.request.clone();
            let coprocessor_url = request_config
                .url
                .clone()
                .unwrap_or_else(|| coprocessor_url.clone());
            let http_client = http_client.clone();
            let sdl = sdl.clone();

            AsyncCheckpointLayer::new(move |request: router::Request| {
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
            .buffered() // XXX: Added during backpressure fixing
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
        C: Service<
                http::Request<RouterBody>,
                Response = http::Response<RouterBody>,
                Error = BoxError,
            > + Clone
            + Send
            + Sync
            + 'static,
        <C as tower::Service<http::Request<RouterBody>>>::Future: Send + 'static,
    {
        let request_layer = (self.request != Default::default()).then_some({
            let request_config = self.request.clone();
            let http_client = http_client.clone();
            let coprocessor_url = request_config
                .url
                .clone()
                .unwrap_or_else(|| coprocessor_url.clone());
            let service_name = service_name.clone();
            AsyncCheckpointLayer::new(move |request: subgraph::Request| {
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
            .buffered() // XXX: Added during backpressure fixing
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
    mut request_config: RouterRequestConf,
) -> Result<ControlFlow<router::Response, router::Request>, BoxError>
where
    C: Service<http::Request<RouterBody>, Response = http::Response<RouterBody>, Error = BoxError>
        + Clone
        + Send
        + Sync
        + 'static,
    <C as tower::Service<http::Request<RouterBody>>>::Future: Send + 'static,
{
    let should_be_executed = request_config
        .condition
        .as_mut()
        .map(|c| c.evaluate_request(&request) == Some(true))
        .unwrap_or(true);
    if !should_be_executed {
        return Ok(ControlFlow::Continue(request));
    }
    // Call into our out of process processor with a body of our body
    // First, extract the data we need from our request and prepare our
    // external call. Use our configuration to figure out which data to send.
    let (parts, body) = request.router_request.into_parts();
    let bytes = router::body::into_bytes(body).await?;

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

    tracing::debug!(?payload, "externalized output");
    let start = Instant::now();
    let co_processor_result = payload.call(http_client, &coprocessor_url).await;
    let duration = start.elapsed();
    record_coprocessor_duration(PipelineStep::RouterRequest, duration);

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
        Some(bytes) => router::body::from_bytes(bytes),
        None => router::body::from_bytes(bytes),
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
    C: Service<http::Request<RouterBody>, Response = http::Response<RouterBody>, Error = BoxError>
        + Clone
        + Send
        + Sync
        + 'static,
    <C as tower::Service<http::Request<RouterBody>>>::Future: Send + 'static,
{
    let should_be_executed = response_config
        .condition
        .as_ref()
        .map(|c| c.evaluate_response(&response))
        .unwrap_or(true);
    if !should_be_executed {
        return Ok(response);
    }
    // split the response into parts + body
    let (parts, body) = response.response.into_parts();

    // we split the body (which is a stream) into first response + rest of responses,
    // for which we will implement mapping later
    let mut stream = body.into_data_stream();
    let first = stream.next().await.transpose()?;
    let rest = stream;

    // If first is None, or contains an error we return an error
    let bytes = match first {
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

    // Second, call our co-processor and get a reply.
    tracing::debug!(?payload, "externalized output");
    let start = Instant::now();
    let co_processor_result = payload.call(http_client.clone(), &coprocessor_url).await;
    let duration = start.elapsed();
    record_coprocessor_duration(PipelineStep::RouterResponse, duration);

    tracing::debug!(?co_processor_result, "co-processor returned");
    let co_processor_output = co_processor_result?;

    validate_coprocessor_output(&co_processor_output, PipelineStep::RouterResponse)?;

    // Third, process our reply and act on the contents. Our processing logic is
    // that we replace "bits" of our incoming response with the updated bits if they
    // are present in our co_processor_output. If they aren't present, just use the
    // bits that we sent to the co_processor.

    let new_body = match co_processor_output.body {
        Some(bytes) => router::body::from_bytes(bytes),
        None => router::body::from_bytes(bytes),
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
                let co_processor_result = payload
                    .call(generator_client, &generator_coprocessor_url)
                    .await;
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
    let bytes = router::body::into_bytes(body).await.map_err(BoxError::from);
    let final_stream = RouterBody::new(http_body_util::StreamBody::new(
        once(ready(bytes))
            .chain(mapped_stream)
            .map(|b| b.map(http_body::Frame::data).map_err(axum::Error::new)),
    ));

    // Finally, return a response which has a Body that wraps our stream of response chunks.
    Ok(router::Response {
        context,
        response: http::Response::from_parts(parts, final_stream),
    })
}
// -----------------------------------------------------------------------------------------------------

async fn process_subgraph_request_stage<C>(
    http_client: C,
    coprocessor_url: Url,
    service_name: String,
    mut request: subgraph::Request,
    mut request_config: SubgraphRequestConf,
) -> Result<ControlFlow<subgraph::Response, subgraph::Request>, BoxError>
where
    C: Service<http::Request<RouterBody>, Response = http::Response<RouterBody>, Error = BoxError>
        + Clone
        + Send
        + Sync
        + 'static,
    <C as tower::Service<http::Request<RouterBody>>>::Future: Send + 'static,
{
    let should_be_executed = request_config
        .condition
        .as_mut()
        .map(|c| c.evaluate_request(&request) == Some(true))
        .unwrap_or(true);
    if !should_be_executed {
        return Ok(ControlFlow::Continue(request));
    }
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
    let subgraph_name = service_name.clone();
    let service_name = request_config.service_name.then_some(service_name);
    let subgraph_request_id = request_config
        .subgraph_request_id
        .then_some(request.id.clone());

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
        .and_subgraph_request_id(subgraph_request_id)
        .build();

    tracing::debug!(?payload, "externalized output");
    let start = Instant::now();
    let co_processor_result = payload.call(http_client, &coprocessor_url).await;
    let duration = start.elapsed();
    record_coprocessor_duration(PipelineStep::SubgraphRequest, duration);

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
                subgraph_name,
                id: request.id,
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
    C: Service<http::Request<RouterBody>, Response = http::Response<RouterBody>, Error = BoxError>
        + Clone
        + Send
        + Sync
        + 'static,
    <C as tower::Service<http::Request<RouterBody>>>::Future: Send + 'static,
{
    let should_be_executed = response_config
        .condition
        .as_ref()
        .map(|c| c.evaluate_response(&response))
        .unwrap_or(true);
    if !should_be_executed {
        return Ok(response);
    }
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
    let subgraph_request_id = response_config
        .subgraph_request_id
        .then_some(response.id.clone());

    let payload = Externalizable::subgraph_builder()
        .stage(PipelineStep::SubgraphResponse)
        .id(response.context.id.clone())
        .and_headers(headers_to_send)
        .and_body(body_to_send)
        .and_context(context_to_send)
        .and_status_code(status_to_send)
        .and_service_name(service_name)
        .and_subgraph_request_id(subgraph_request_id)
        .build();

    tracing::debug!(?payload, "externalized output");
    let start = Instant::now();
    let co_processor_result = payload.call(http_client, &coprocessor_url).await;
    let duration = start.elapsed();
    record_coprocessor_duration(PipelineStep::SubgraphResponse, duration);

    tracing::debug!(?co_processor_result, "co-processor returned");
    let co_processor_output = co_processor_result?;

    validate_coprocessor_output(&co_processor_output, PipelineStep::SubgraphResponse)?;

    // Third, process our reply and act on the contents. Our processing logic is
    // that we replace "bits" of our incoming response with the updated bits if they
    // are present in our co_processor_output. If they aren't present, just use the
    // bits that we sent to the co_processor.

    let new_body: crate::graphql::Response =
        handle_graphql_response(body, co_processor_output.body)?;

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

pub(super) fn handle_graphql_response(
    original_response_body: graphql::Response,
    copro_response_body: Option<serde_json::Value>,
) -> Result<graphql::Response, BoxError> {
    let new_body: graphql::Response = match copro_response_body {
        Some(value) => {
            let mut new_body: graphql::Response = serde_json::from_value(value)?;
            // Needs to take back these 2 fields because it's skipped by serde
            new_body.subscribed = original_response_body.subscribed;
            new_body.created_at = original_response_body.created_at;
            // Required because for subscription if data is Some(Null) it won't cut the subscription
            // And in some languages they don't have any differences between Some(Null) and Null
            if original_response_body.data == Some(serde_json_bytes::Value::Null)
                && new_body.data.is_none()
                && new_body.subscribed == Some(true)
            {
                new_body.data = Some(serde_json_bytes::Value::Null);
            }

            new_body
        }
        None => original_response_body,
    };

    Ok(new_body)
}
