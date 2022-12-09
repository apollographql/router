//! Externalization plugin
// With regards to ELv2 licensing, this entire file is license key functionality

use std::collections::HashMap;
use std::fmt;
use std::ops::ControlFlow;
use std::str::FromStr;
use std::sync::Arc;

use bytes::Bytes;
use http::header::HeaderName;
use http::HeaderMap;
use http::HeaderValue;
use http::StatusCode;
use hyper::body;
use hyper::Body;
use schemars::JsonSchema;
use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde::Serialize;
use tower::util::MapFutureLayer;
use tower::BoxError;
use tower::ServiceBuilder;
use tower::ServiceExt;

use crate::error::Error;
use crate::external::Externalizable;
use crate::external::PipelineStep;
use crate::layers::async_checkpoint::AsyncCheckpointLayer;
use crate::layers::ServiceBuilderExt;
use crate::plugin::Plugin;
use crate::plugin::PluginInit;
use crate::register_plugin;
use crate::services::router;
use crate::Context;

pub(crate) const EXTERNAL_SPAN_NAME: &str = "external plugin";

#[derive(Debug)]
struct ExternalPlugin {
    configuration: Conf,
    sdl: Arc<String>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize, JsonSchema)]
struct BaseConf {
    #[serde(default)]
    headers: bool,
    #[serde(default)]
    context: bool,
    #[serde(default)]
    body: bool,
    #[serde(default)]
    sdl: bool,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize, JsonSchema)]
struct RouterStage {
    #[serde(default)]
    request: Option<BaseConf>,
    #[serde(default)]
    response: Option<BaseConf>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize, JsonSchema)]
struct Stages {
    #[serde(default)]
    router: Option<RouterStage>,
}

#[derive(Clone, Debug, Default, Deserialize, JsonSchema)]
struct Conf {
    // Put your plugin configuration here. It will automatically be deserialized from JSON.
    url: String, // The url you'd like to offload processing to
    #[serde(default)]
    stages: Option<Stages>,
}

#[async_trait::async_trait]
impl Plugin for ExternalPlugin {
    type Config = Conf;

    async fn new(init: PluginInit<Self::Config>) -> Result<Self, BoxError> {
        Ok(ExternalPlugin {
            configuration: init.config,
            sdl: init.supergraph_sdl,
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

                        // Second, call our co-processor and get a reply.
                        let co_processor_output = call_external(
                            proto_url,
                            PipelineStep::RouterRequest,
                            headers,
                            payload,
                            context,
                            sdl,
                        )
                        .await?;

                        tracing::debug!(?co_processor_output, "co-processor returned");

                        // Third, process our reply and act on the contents. Our processing logic is
                        // that we replace "bits" of our incoming request with the updated bits if they
                        // are present in our co_processor_output.
                        //

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

                        // Finally, we need to interpret the HTTP status codes which may have been
                        // updated by our co-processor and decide if we should proceed or stop.

                        let code = StatusCode::from_u16(co_processor_output.http.status)?;

                        if !code.is_success() {
                            let res = router::Response::error_builder()
                                .errors(vec![Error {
                                    message: co_processor_output.http.message,
                                    ..Default::default()
                                }])
                                .status_code(code)
                                .context(request.context)
                                .build()?;
                            Ok(ControlFlow::Break(res))
                        } else {
                            Ok(ControlFlow::Continue(request))
                        }
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
                    let co_processor_output = call_external(
                        proto_url,
                        PipelineStep::RouterResponse,
                        headers,
                        payload,
                        context,
                        sdl,
                    )
                    .await?;

                    tracing::debug!(?co_processor_output, "co-processor returned");

                    // Third, process our reply and act on the contents. Our processing logic is
                    // that we replace "bits" of our incoming response with the updated bits if they
                    // are present in our co_processor_output.

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
            .buffer(20_000)
            .service(service)
            .boxed()
    }
}

type ExternalParams<'a> = (
    Option<&'a HeaderMap<HeaderValue>>,
    Option<serde_json::Value>,
    Option<Context>,
    Option<String>,
);

fn prepare_external_params<'a>(
    config: &'a BaseConf,
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

    if config.body || config.headers {
        if config.body {
            payload_opt = Some(serde_json::from_slice(bytes)?);
        }
        if config.headers {
            headers_opt = Some(headers);
        }
    }
    if config.context {
        context_opt = Some(context.clone());
    }
    if config.sdl {
        sdl_opt = Some(sdl);
    }
    Ok((headers_opt, payload_opt, context_opt, sdl_opt))
}

async fn call_external<T>(
    url: String,
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
    output.call(&url).await
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

// This macro allows us to use it in our plugin registry!
// register_plugin takes a group name, and a plugin name.
//
// In order to keep the plugin names consistent,
// we use using the `Reverse domain name notation`
register_plugin!("experimental", "external", ExternalPlugin);

#[cfg(test)]
mod tests {
    // If we run this test as follows: cargo test -- --nocapture
    // we will see the message "Hello Bob" printed to standard out
    #[tokio::test]
    async fn display_message() {
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
            .build()
            .await
            .unwrap();
    }
}
