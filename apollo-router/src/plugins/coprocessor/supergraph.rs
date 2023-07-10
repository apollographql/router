use std::ops::ControlFlow;
use std::sync::Arc;

use futures::future;
use futures::stream;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tower::{BoxError, ServiceBuilder};
use tower_service::Service;

use super::externalize_header_map;
use super::*;
use crate::layers::async_checkpoint::AsyncCheckpointLayer;
use crate::layers::ServiceBuilderExt;
use crate::plugins::coprocessor::EXTERNAL_SPAN_NAME;
use crate::services::supergraph;

/// What information is passed to a router request/response stage
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub(super) struct SupergraphRequestConf {
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
}

/*/// What information is passed to a router request/response stage
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub(super) struct SupergraphResponseConf {
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
}*/

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize, JsonSchema)]
#[serde(default)]
pub(super) struct SupergraphStage {
    /// The request configuration
    pub(super) request: SupergraphRequestConf,
    // /// The response configuration
    //pub(super) response: SupergraphResponseConf,
}

impl SupergraphStage {
    pub(crate) fn as_service<C>(
        &self,
        http_client: C,
        service: supergraph::BoxService,
        coprocessor_url: String,
        sdl: Arc<String>,
    ) -> supergraph::BoxService
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
            let coprocessor_url = coprocessor_url.clone();
            let http_client = http_client.clone();
            let sdl = sdl.clone();

            AsyncCheckpointLayer::new(move |request: supergraph::Request| {
                let request_config = request_config.clone();
                let coprocessor_url = coprocessor_url.clone();
                let http_client = http_client.clone();
                let sdl = sdl.clone();

                async move {
                    process_supergraph_request_stage(
                        http_client,
                        coprocessor_url,
                        sdl,
                        request,
                        request_config,
                    )
                    .await
                    .map_err(|error| {
                        tracing::error!(
                            "external extensibility: supergraph request stage error: {error}"
                        );
                        error
                    })
                }
            })
        });

        /*let response_layer = (self.response != Default::default()).then_some({
            let response_config = self.response.clone();
            MapFutureLayer::new(move |fut| {
                let sdl = sdl.clone();
                let coprocessor_url = coprocessor_url.clone();
                let http_client = http_client.clone();
                let response_config = response_config.clone();

                async move {
                    let response: supergraph::Response = fut.await?;

                    process_supergraph_response_stage(
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
        });*/

        fn external_service_span() -> impl Fn(&supergraph::Request) -> tracing::Span + Clone {
            move |_request: &supergraph::Request| {
                tracing::info_span!(
                    EXTERNAL_SPAN_NAME,
                    "external service" = stringify!(supergraph::Request),
                    "otel.kind" = "INTERNAL"
                )
            }
        }

        ServiceBuilder::new()
            .instrument(external_service_span())
            .option_layer(request_layer)
            //.option_layer(response_layer)
            .buffered()
            .service(service)
            .boxed()
    }
}

async fn process_supergraph_request_stage<C>(
    http_client: C,
    coprocessor_url: String,
    sdl: Arc<String>,
    mut request: supergraph::Request,
    request_config: SupergraphRequestConf,
) -> Result<ControlFlow<supergraph::Response, supergraph::Request>, BoxError>
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
    let (parts, body) = request.supergraph_request.into_parts();
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

    let payload = Externalizable::supergraph_builder()
        .stage(PipelineStep::SupergraphRequest)
        .control(Control::default())
        .and_id(TraceId::maybe_new().map(|id| id.to_string()))
        .and_headers(headers_to_send)
        .and_body(body_to_send)
        .and_context(context_to_send)
        .method(parts.method.to_string())
        .sdl(sdl.to_string())
        .build();

    tracing::debug!(?payload, "externalized output");
    let guard = request.context.enter_active_request();
    let co_processor_result = payload.call(http_client, &coprocessor_url).await;
    drop(guard);
    tracing::debug!(?co_processor_result, "co-processor returned");
    let co_processor_output = co_processor_result?;
    validate_coprocessor_output(&co_processor_output, PipelineStep::SupergraphRequest)?;
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
                                .extension_code("EXTERNAL_DESERIALIZATION_ERROR")
                                .build()])
                            .build()
                    });

            let mut http_response = http::Response::builder()
                .status(code)
                .body(stream::once(future::ready(graphql_response)).boxed())?;
            if let Some(headers) = co_processor_output.headers {
                *http_response.headers_mut() = internalize_header_map(headers)?;
            }

            let supergraph_response = supergraph::Response {
                response: http_response,
                context: request.context,
            };

            if let Some(context) = co_processor_output.context {
                for (key, value) in context.try_into_iter()? {
                    supergraph_response
                        .context
                        .upsert_json_value(key, move |_current| value);
                }
            }

            supergraph_response
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

    request.supergraph_request = http::Request::from_parts(parts, new_body);

    if let Some(context) = co_processor_output.context {
        for (key, value) in context.try_into_iter()? {
            request
                .context
                .upsert_json_value(key, move |_current| value);
        }
    }

    if let Some(headers) = co_processor_output.headers {
        *request.supergraph_request.headers_mut() = internalize_header_map(headers)?;
    }

    if let Some(uri) = co_processor_output.uri {
        *request.supergraph_request.uri_mut() = uri.parse()?;
    }

    Ok(ControlFlow::Continue(request))
}

/*
async fn process_supergraph_response_stage<C>(
    http_client: C,
    coprocessor_url: String,
    service_name: String,
    mut response: supergraph::Response,
    response_config: SupergraphResponseConf,
) -> Result<supergraph::Response, BoxError>
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
    let bytes = Bytes::from(serde_json::to_vec(&body)?);

    let headers_to_send = response_config
        .headers
        .then(|| externalize_header_map(&parts.headers))
        .transpose()?;

    let status_to_send = response_config.status_code.then(|| parts.status.as_u16());

    let body_to_send = response_config
        .body
        .then(|| serde_json::from_slice::<serde_json::Value>(&bytes))
        .transpose()?;
    let context_to_send = response_config.context.then(|| response.context.clone());

    let payload = Externalizable::subgraph_builder()
        .stage(PipelineStep::SupergraphResponse)
        .and_id(TraceId::maybe_new().map(|id| id.to_string()))
        .and_headers(headers_to_send)
        .and_body(body_to_send)
        .and_context(context_to_send)
        .and_status_code(status_to_send)
        .build();

    tracing::debug!(?payload, "externalized output");
    let guard = response.context.enter_active_request();
    let co_processor_result = payload.call(http_client, &coprocessor_url).await;
    drop(guard);
    tracing::debug!(?co_processor_result, "co-processor returned");
    let co_processor_output = co_processor_result?;

    validate_coprocessor_output(&co_processor_output, PipelineStep::SupergraphResponse)?;

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
*/
