use std::collections::HashMap;
use std::fmt;
use std::ops::ControlFlow;
use std::str::FromStr;
use std::sync::Arc;

use crate::external::Externalizable;
use crate::external::PipelineStep;
use crate::graphql;
use crate::layers::ServiceBuilderExt;
use crate::plugin::Plugin;
use crate::plugin::PluginInit;
use crate::register_plugin;
use crate::services::router;
use crate::Context;

use http::header::HeaderName;
use http::HeaderMap;
use http::HeaderValue;
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

#[derive(Debug, Default, Deserialize, JsonSchema)]
struct Conf {
    // Put your plugin configuration here. It will automatically be deserialized from JSON.
    url: String, // The url you'd like to offload processing to
}

#[derive(Debug, Deserialize, Serialize)]
struct Output {
    context: Context,
    sdl: Arc<String>,
    body: graphql::Request,
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
        let proto_url = self.configuration.url.clone();
        let sdl = self.sdl.clone();
        ServiceBuilder::new()
            .checkpoint_async(move |mut request: router::Request| {
                let proto_url = proto_url.clone();
                let my_sdl = sdl.to_string();

                async move {
                    // Call into our out of process processor with a body of our body

                    // First, convert our request into an "Externalizable" which we can pass to our
                    // external co-processor.
                    let (parts, body) = request.router_request.into_parts();
                    let b_bytes = body::to_bytes(body).await?;
                    let b_json: serde_json::Value = serde_json::from_slice(&b_bytes)?;
                    let context = request.context.clone();

                    // Second, call our co-processor and get a response.
                    let modified_output = call_external(
                        proto_url,
                        PipelineStep::SupergraphRequest,
                        None,
                        Some(b_json),
                        Some(context),
                        Some(my_sdl),
                    )
                    .await?;

                    // Third, process our response and act on the contents.
                    tracing::info!("modified output: {:?}", modified_output);
                    // *request.router_request.body_mut() = modified_output.body.unwrap();
                    request.context = modified_output.context.unwrap();

                    // Figure out a way to allow our external processor to interact with
                    // headers and extensions. Probably don't want to allow other things
                    // to be changed (version, etc...)
                    // None of these things can be serialized just now.
                    /*
                    let hdrs = serde_json::to_string(&request.supergraph_request.headers())?;
                    let extensions =
                        serde_json::to_string(&request.supergraph_request.extensions())?;
                    */
                    let new_body = Body::from(serde_json::to_vec(&modified_output.body.unwrap())?);
                    request.router_request = http::Request::from_parts(parts, new_body);
                    *request.router_request.headers_mut() =
                        internalize_header_map(modified_output.headers.unwrap())?;

                    Ok(ControlFlow::Continue(request))
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
register_plugin!("apollo", "external", ExternalPlugin);

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
