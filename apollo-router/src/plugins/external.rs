//! Externalization plugin
// With regards to ELv2 licensing, this entire file is license key functionality

use std::collections::HashMap;
use std::fmt;
use std::ops::ControlFlow;
use std::str::FromStr;
use std::sync::Arc;

use crate::error::Error;
use crate::external::Externalizable;
use crate::external::PipelineStep;
use crate::layers::ServiceBuilderExt;
use crate::plugin::Plugin;
use crate::plugin::PluginInit;
use crate::register_plugin;
use crate::services::router;
use crate::Context;

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
use tower::BoxError;
use tower::ServiceBuilder;
use tower::ServiceExt;

#[derive(Debug)]
struct ExternalPlugin {
    configuration: Conf,
    sdl: Arc<String>,
}

#[derive(Clone, Debug, Default, Deserialize, JsonSchema)]
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

#[derive(Clone, Debug, Default, Deserialize, JsonSchema)]
struct Conf {
    // Put your plugin configuration here. It will automatically be deserialized from JSON.
    url: String, // The url you'd like to offload processing to
    #[serde(default)]
    request: BaseConf,
    #[serde(default)]
    response: BaseConf,
}

// This is a bare bones plugin that can be duplicated when creating your own.
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
        let sdl = self.sdl.clone();
        let config = self.configuration.clone();
        ServiceBuilder::new()
            .checkpoint_async(move |mut request: router::Request| {
                let proto_url = config.url.clone();
                let my_sdl = sdl.to_string();

                async move {
                    // Call into our out of process processor with a body of our body

                    // First, convert our request into an "Externalizable" which we can pass to our
                    // external co-processor. We examine our configuration and only send those
                    // parts which are configured.
                    let mut headers = None;
                    let mut context = None;
                    // Note: We have to specify the json type or it won't deserialize correctly...
                    let mut b_json: Option<serde_json::Value> = None;
                    let mut sdl = None;
                    // Inefficient to do this every request. Try to optimise later
                    let (parts, body) = request.router_request.into_parts();
                    let b_bytes = body::to_bytes(body).await?;

                    if config.request.body || config.request.headers {
                        if config.request.body {
                            b_json = Some(serde_json::from_slice(&b_bytes)?);
                        }
                        if config.request.headers {
                            headers = Some(&parts.headers);
                        }
                    }

                    if config.request.context {
                        context = Some(request.context.clone());
                    }

                    if config.request.sdl {
                        sdl = Some(my_sdl);
                    }

                    // Second, call our co-processor and get a response.
                    let co_processor_output = call_external(
                        proto_url,
                        PipelineStep::RouterRequest,
                        headers,
                        b_json,
                        context,
                        sdl,
                    )
                    .await?;

                    tracing::info!("co_processor output: {:?}", co_processor_output);

                    // Third, process our response and act on the contents. Our processing logic is
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
                        *request.router_request.headers_mut() = internalize_header_map(headers)?;
                    }

                    // Finally, if we get here, we need to interpret the HTTP status codes and
                    // decide if we should proceed or stop. TBD

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
            })
            // .map_response()
            // .rate_limit()
            // .checkpoint()
            // .timeout()
            .buffer(20_000)
            .service(service)
            .boxed()
    }
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
    tracing::info!("sending output: {:?}", output);
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
                "apollo.external": {
                    "url": "http://127.0.0.1:8081"
                }
            }
        });
        // Build a test harness. Usually we'd use this and send requests to
        // it, but in this case it's enough to build the harness to see our
        // output when our service registers.
        let _test_harness = apollo_router::TestHarness::builder()
            .configuration_json(config)
            .unwrap()
            .build()
            .await
            .unwrap();
    }
}
