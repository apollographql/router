// With regards to ELv2 licensing, this entire file is license key functionality
#![allow(missing_docs)] // FIXME

use std::collections::HashMap;
use std::fmt::Debug;
use std::time::Duration;

use http::header::ACCEPT;
use http::header::CONTENT_TYPE;
use http::Method;
use http::StatusCode;
use hyper::Body;
use opentelemetry::global::get_text_map_propagator;
use schemars::JsonSchema;
use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde::Serialize;
use strum_macros::Display;
use tower::BoxError;
use tower::Service;
use tracing_opentelemetry::OpenTelemetrySpanExt;

use crate::Context;

pub(crate) const DEFAULT_EXTERNALIZATION_TIMEOUT: Duration = Duration::from_secs(1);

/// Version of our externalised data. Rev this if it changes
pub(crate) const EXTERNALIZABLE_VERSION: u8 = 1;

#[derive(Clone, Debug, Display, Deserialize, PartialEq, Serialize, JsonSchema)]
pub(crate) enum PipelineStep {
    RouterRequest,
    RouterResponse,
    SupergraphRequest,
    SupergraphResponse,
    ExecutionRequest,
    ExecutionResponse,
    SubgraphRequest,
    SubgraphResponse,
}

#[derive(Clone, Debug, Display, Deserialize, PartialEq, Serialize, JsonSchema)]
pub(crate) enum Control {
    Continue,
    Break(u16),
}

impl Default for Control {
    fn default() -> Self {
        Control::Continue
    }
}

impl Control {
    #[allow(dead_code)]
    fn new(status: u16) -> Self {
        Control::Break(status)
    }

    pub(crate) fn get_http_status(&self) -> Result<StatusCode, BoxError> {
        match self {
            Control::Continue => Ok(StatusCode::OK),
            Control::Break(code) => StatusCode::from_u16(*code).map_err(|e| e.into()),
        }
    }
}

// TODO: Builder
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Externalizable<T> {
    pub(crate) version: u8,
    pub(crate) stage: String,
    pub(crate) control: Option<Control>,
    pub(crate) id: Option<String>,
    pub(crate) headers: Option<HashMap<String, Vec<String>>>,
    pub(crate) body: Option<T>,
    pub(crate) context: Option<Context>,
    pub(crate) sdl: Option<String>,
    pub(crate) uri: Option<String>,
    pub(crate) service_name: Option<String>,
}

impl<T> Externalizable<T>
where
    T: Debug + DeserializeOwned + Serialize + Send + Sync,
{
    pub(crate) async fn call<C>(self, mut client: C, uri: &str) -> Result<Self, BoxError>
    where
        C: Service<hyper::Request<Body>, Response = hyper::Response<Body>, Error = BoxError>
            + Clone
            + Send
            + Sync
            + 'static,
    {
        tracing::debug!("forwarding json: {}", serde_json::to_string(&self)?);

        let mut request = hyper::Request::builder()
            .uri(uri)
            .method(Method::POST)
            .header(ACCEPT, "application/json")
            .header(CONTENT_TYPE, "application/json")
            .body(serde_json::to_vec(&self)?.into())?;

        get_text_map_propagator(|propagator| {
            propagator.inject_context(
                &tracing::span::Span::current().context(),
                &mut opentelemetry_http::HeaderInjector(request.headers_mut()),
            );
        });

        let response = client.call(request).await?;
        hyper::body::to_bytes(response.into_body())
            .await
            .map_err(BoxError::from)
            .and_then(|bytes| serde_json::from_slice(&bytes).map_err(BoxError::from))
    }
}
