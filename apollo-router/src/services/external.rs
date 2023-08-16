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

#[derive(Clone, Debug, Default, Display, Deserialize, PartialEq, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub(crate) enum Control {
    #[default]
    Continue,
    Break(u16),
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

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Externalizable<T> {
    pub(crate) version: u8,
    pub(crate) stage: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) control: Option<Control>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) headers: Option<HashMap<String, Vec<String>>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) body: Option<T>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) context: Option<Context>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) sdl: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) uri: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) method: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) service_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) status_code: Option<u16>,
}

#[buildstructor::buildstructor]
impl<T> Externalizable<T>
where
    T: Debug + DeserializeOwned + Serialize + Send + Sync,
{
    #[builder(visibility = "pub(crate)")]
    /// This is the constructor (or builder) to use when constructing a Router
    /// `Externalizable`.
    ///
    fn router_new(
        stage: PipelineStep,
        control: Option<Control>,
        id: Option<String>,
        headers: Option<HashMap<String, Vec<String>>>,
        body: Option<T>,
        context: Option<Context>,
        status_code: Option<u16>,
        method: Option<String>,
        path: Option<String>,
        sdl: Option<String>,
    ) -> Self {
        assert!(matches!(
            stage,
            PipelineStep::RouterRequest | PipelineStep::RouterResponse
        ));
        Externalizable {
            version: EXTERNALIZABLE_VERSION,
            stage: stage.to_string(),
            control,
            id,
            headers,
            body,
            context,
            status_code,
            sdl,
            uri: None,
            path,
            method,
            service_name: None,
        }
    }

    #[builder(visibility = "pub(crate)")]
    /// This is the constructor (or builder) to use when constructing a Supergraph
    /// `Externalizable`.
    ///
    fn supergraph_new(
        stage: PipelineStep,
        control: Option<Control>,
        id: Option<String>,
        headers: Option<HashMap<String, Vec<String>>>,
        body: Option<T>,
        context: Option<Context>,
        status_code: Option<u16>,
        method: Option<String>,
        sdl: Option<String>,
    ) -> Self {
        assert!(matches!(
            stage,
            PipelineStep::SupergraphRequest | PipelineStep::SupergraphResponse
        ));
        Externalizable {
            version: EXTERNALIZABLE_VERSION,
            stage: stage.to_string(),
            control,
            id,
            headers,
            body,
            context,
            status_code,
            sdl,
            uri: None,
            path: None,
            method,
            service_name: None,
        }
    }

    #[builder(visibility = "pub(crate)")]
    /// This is the constructor (or builder) to use when constructing a Subgraph
    /// `Externalizable`.
    ///
    fn subgraph_new(
        stage: PipelineStep,
        control: Option<Control>,
        id: Option<String>,
        headers: Option<HashMap<String, Vec<String>>>,
        body: Option<T>,
        context: Option<Context>,
        status_code: Option<u16>,
        method: Option<String>,
        service_name: Option<String>,
        uri: Option<String>,
    ) -> Self {
        assert!(matches!(
            stage,
            PipelineStep::SubgraphRequest | PipelineStep::SubgraphResponse
        ));
        Externalizable {
            version: EXTERNALIZABLE_VERSION,
            stage: stage.to_string(),
            control,
            id,
            headers,
            body,
            context,
            status_code,
            sdl: None,
            uri,
            path: None,
            method,
            service_name,
        }
    }

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

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn it_will_build_router_externalizable_correctly() {
        Externalizable::<String>::router_builder()
            .stage(PipelineStep::RouterRequest)
            .build();
        Externalizable::<String>::router_builder()
            .stage(PipelineStep::RouterResponse)
            .build();
    }

    #[test]
    #[should_panic]
    fn it_will_not_build_router_externalizable_incorrectly() {
        Externalizable::<String>::router_builder()
            .stage(PipelineStep::SubgraphRequest)
            .build();
        Externalizable::<String>::router_builder()
            .stage(PipelineStep::SubgraphResponse)
            .build();
    }

    #[test]
    fn it_will_build_subgraph_externalizable_correctly() {
        Externalizable::<String>::subgraph_builder()
            .stage(PipelineStep::SubgraphRequest)
            .build();
        Externalizable::<String>::subgraph_builder()
            .stage(PipelineStep::SubgraphResponse)
            .build();
    }

    #[test]
    #[should_panic]
    fn it_will_not_build_subgraph_externalizable_incorrectly() {
        Externalizable::<String>::subgraph_builder()
            .stage(PipelineStep::RouterRequest)
            .build();
        Externalizable::<String>::subgraph_builder()
            .stage(PipelineStep::RouterResponse)
            .build();
    }
}
