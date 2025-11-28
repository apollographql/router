//! Structures for externalised data, communicating the state of the router pipeline at the
//! different stages.

use std::collections::HashMap;
use std::fmt::Debug;
use std::sync::Arc;
use std::time::Duration;

use http::HeaderMap;
use http::HeaderValue;
use http::Method;
use http::StatusCode;
use http::header::ACCEPT;
use http::header::CONTENT_TYPE;
#[cfg(unix)]
use hyperlocal;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use serde::de::DeserializeOwned;
use strum_macros::Display;
use tower::BoxError;
use tower::Service;

use super::subgraph::SubgraphRequestId;
use crate::Context;
use crate::query_planner::QueryPlan;
use crate::services::router;
use crate::services::router::body::RouterBody;

pub(crate) const DEFAULT_EXTERNALIZATION_TIMEOUT: Duration = Duration::from_secs(1);

/// Version of our externalised data. Rev this if it changes
pub(crate) const EXTERNALIZABLE_VERSION: u8 = 1;

/// Extension to pass Unix socket path information to HttpClientService for proper span attributes.
/// Uses Arc<str> for efficient sharing without cloning the actual string data.
#[derive(Clone, Debug)]
pub(crate) struct UnixSocketPath(pub(crate) Arc<str>);

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

impl From<PipelineStep> for opentelemetry::Value {
    fn from(val: PipelineStep) -> Self {
        val.to_string().into()
    }
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) has_next: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    query_plan: Option<Arc<QueryPlan>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) subgraph_request_id: Option<SubgraphRequestId>,
}

#[buildstructor::buildstructor]
impl<T> Externalizable<T>
where
    T: Debug + DeserializeOwned + Serialize + Send + Sync,
{
    /// This is the constructor (or builder) to use when constructing a Router
    /// `Externalizable`.
    #[builder(visibility = "pub(crate)")]
    fn router_new(
        stage: PipelineStep,
        control: Option<Control>,
        id: String,
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
            id: Some(id),
            headers,
            body,
            context,
            status_code,
            sdl,
            uri: None,
            path,
            method,
            service_name: None,
            has_next: None,
            query_plan: None,
            subgraph_request_id: None,
        }
    }

    /// This is the constructor (or builder) to use when constructing a Supergraph
    /// `Externalizable`.
    #[builder(visibility = "pub(crate)")]
    fn supergraph_new(
        stage: PipelineStep,
        control: Option<Control>,
        id: String,
        headers: Option<HashMap<String, Vec<String>>>,
        body: Option<T>,
        context: Option<Context>,
        status_code: Option<u16>,
        method: Option<String>,
        sdl: Option<String>,
        has_next: Option<bool>,
    ) -> Self {
        assert!(matches!(
            stage,
            PipelineStep::SupergraphRequest | PipelineStep::SupergraphResponse
        ));
        Externalizable {
            version: EXTERNALIZABLE_VERSION,
            stage: stage.to_string(),
            control,
            id: Some(id),
            headers,
            body,
            context,
            status_code,
            sdl,
            uri: None,
            path: None,
            method,
            service_name: None,
            has_next,
            query_plan: None,
            subgraph_request_id: None,
        }
    }

    /// This is the constructor (or builder) to use when constructing an Execution
    /// `Externalizable`.
    #[builder(visibility = "pub(crate)")]
    fn execution_new(
        stage: PipelineStep,
        control: Option<Control>,
        id: String,
        headers: Option<HashMap<String, Vec<String>>>,
        body: Option<T>,
        context: Option<Context>,
        status_code: Option<u16>,
        method: Option<String>,
        sdl: Option<String>,
        has_next: Option<bool>,
        query_plan: Option<Arc<QueryPlan>>,
    ) -> Self {
        assert!(matches!(
            stage,
            PipelineStep::ExecutionRequest | PipelineStep::ExecutionResponse
        ));
        Externalizable {
            version: EXTERNALIZABLE_VERSION,
            stage: stage.to_string(),
            control,
            id: Some(id),
            headers,
            body,
            context,
            status_code,
            sdl,
            uri: None,
            path: None,
            method,
            service_name: None,
            has_next,
            query_plan,
            subgraph_request_id: None,
        }
    }

    /// This is the constructor (or builder) to use when constructing a Subgraph
    /// `Externalizable`.
    #[builder(visibility = "pub(crate)")]
    fn subgraph_new(
        stage: PipelineStep,
        control: Option<Control>,
        id: String,
        headers: Option<HashMap<String, Vec<String>>>,
        body: Option<T>,
        context: Option<Context>,
        status_code: Option<u16>,
        method: Option<String>,
        service_name: Option<String>,
        uri: Option<String>,
        subgraph_request_id: Option<SubgraphRequestId>,
    ) -> Self {
        assert!(matches!(
            stage,
            PipelineStep::SubgraphRequest | PipelineStep::SubgraphResponse
        ));
        Externalizable {
            version: EXTERNALIZABLE_VERSION,
            stage: stage.to_string(),
            control,
            id: Some(id),
            headers,
            body,
            context,
            status_code,
            sdl: None,
            uri,
            path: None,
            method,
            service_name,
            has_next: None,
            query_plan: None,
            subgraph_request_id,
        }
    }

    pub(crate) async fn call<C>(self, mut client: C, uri: &str) -> Result<Self, BoxError>
    where
        C: Service<
                http::Request<RouterBody>,
                Response = http::Response<RouterBody>,
                Error = BoxError,
            > + Clone
            + Send
            + Sync
            + 'static,
    {
        tracing::debug!("forwarding json: {}", serde_json::to_string(&self)?);

        // Handle Unix socket URL conversion
        // Standard http::Uri doesn't support unix:// URLs, so we need to convert them
        // using hyperlocal which encodes the socket path in a way the Unix connector understands
        #[cfg(unix)]
        let (converted_uri, unix_socket_path) = if let Some(path) = uri.strip_prefix("unix://") {
            let socket_path: Arc<str> = path.into();
            let hyperlocal_uri: http::Uri = hyperlocal::Uri::new(path, "/").into();
            (hyperlocal_uri, Some(socket_path))
        } else {
            (uri.parse()?, None)
        };
        #[cfg(not(unix))]
        let (converted_uri, unix_socket_path): (http::Uri, Option<Arc<str>>) = (uri.parse()?, None);

        let mut request = http::Request::builder()
            .uri(converted_uri)
            .method(Method::POST)
            .header(ACCEPT, "application/json")
            .header(CONTENT_TYPE, "application/json")
            .body(router::body::from_bytes(serde_json::to_vec(&self)?))?;

        // Add Unix socket path as an extension so HttpClientService can use it for span attributes
        // We use Arc<str> so HttpClientService can share the path without cloning the string data
        if let Some(socket_path) = unix_socket_path {
            request.extensions_mut().insert(UnixSocketPath(socket_path));
        }

        let response = client.call(request).await?;
        router::body::into_bytes(response.into_body())
            .await
            .map_err(BoxError::from)
            .and_then(|bytes| serde_json::from_slice(&bytes).map_err(BoxError::from))
    }
}

/// Convert a HeaderMap into a HashMap
pub(crate) fn externalize_header_map(
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

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn it_will_build_router_externalizable_correctly() {
        Externalizable::<String>::router_builder()
            .stage(PipelineStep::RouterRequest)
            .id(String::default())
            .build();
        Externalizable::<String>::router_builder()
            .stage(PipelineStep::RouterResponse)
            .id(String::default())
            .build();
    }

    #[test]
    #[should_panic]
    fn it_will_not_build_router_externalizable_incorrectly() {
        Externalizable::<String>::router_builder()
            .stage(PipelineStep::SubgraphRequest)
            .id(String::default())
            .build();
        Externalizable::<String>::router_builder()
            .stage(PipelineStep::SubgraphResponse)
            .id(String::default())
            .build();
    }

    #[test]
    #[should_panic]
    fn it_will_not_build_router_externalizable_incorrectl_supergraph() {
        Externalizable::<String>::router_builder()
            .stage(PipelineStep::SupergraphRequest)
            .id(String::default())
            .build();
        Externalizable::<String>::router_builder()
            .stage(PipelineStep::SupergraphResponse)
            .id(String::default())
            .build();
    }

    #[test]
    fn it_will_build_subgraph_externalizable_correctly() {
        Externalizable::<String>::subgraph_builder()
            .stage(PipelineStep::SubgraphRequest)
            .id(String::default())
            .build();
        Externalizable::<String>::subgraph_builder()
            .stage(PipelineStep::SubgraphResponse)
            .id(String::default())
            .build();
    }

    #[test]
    #[should_panic]
    fn it_will_not_build_subgraph_externalizable_incorrectly() {
        Externalizable::<String>::subgraph_builder()
            .stage(PipelineStep::RouterRequest)
            .id(String::default())
            .build();
        Externalizable::<String>::subgraph_builder()
            .stage(PipelineStep::RouterResponse)
            .id(String::default())
            .build();
    }
}
