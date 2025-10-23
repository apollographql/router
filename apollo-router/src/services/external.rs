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
use opentelemetry::global::get_text_map_propagator;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use serde::de::DeserializeOwned;
use strum_macros::Display;
use tower::BoxError;
use tower::Service;
use tracing::Instrument;

use super::subgraph::SubgraphRequestId;
use crate::Context;
use crate::plugins::telemetry::consts::HTTP_REQUEST_SPAN_NAME;
use crate::plugins::telemetry::otel::OpenTelemetrySpanExt;
use crate::plugins::telemetry::reload::otel::prepare_context;
use crate::query_planner::QueryPlan;
use crate::services::router;
use crate::services::router::body::RouterBody;

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

        let mut request = http::Request::builder()
            .uri(uri)
            .method(Method::POST)
            .header(ACCEPT, "application/json")
            .header(CONTENT_TYPE, "application/json")
            .body(router::body::from_bytes(serde_json::to_vec(&self)?))?;

        let schema_uri = request.uri();
        let host = schema_uri.host().unwrap_or_default();
        let port = schema_uri.port_u16().unwrap_or_else(|| {
            let scheme = schema_uri.scheme_str();
            if scheme == Some("https") {
                443
            } else if scheme == Some("http") {
                80
            } else {
                0
            }
        });
        let otel_name = format!("POST {schema_uri}");

        let http_req_span = tracing::info_span!(HTTP_REQUEST_SPAN_NAME,
            "otel.kind" = "CLIENT",
            "http.request.method" = "POST",
            "server.address" = %host,
            "server.port" = %port,
            "url.full" = %schema_uri,
            "otel.name" = %otel_name,
            "otel.original_name" = "http_request",
        );

        get_text_map_propagator(|propagator| {
            propagator.inject_context(
                &prepare_context(http_req_span.context()),
                &mut crate::otel_compat::HeaderInjector(request.headers_mut()),
            );
        });

        let response = client.call(request).instrument(http_req_span).await?;
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
    use http::Response;
    use tower::service_fn;
    use tracing_futures::WithSubscriber;

    use super::*;
    use crate::assert_snapshot_subscriber;

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

    #[tokio::test]
    async fn it_will_create_an_http_request_span() {
        async {
            // Create a mock service that returns a simple response
            let service = service_fn(|_req: http::Request<RouterBody>| async {
                tracing::info!("got request");
                Ok::<_, BoxError>(
                    Response::builder()
                        .status(200)
                        .body(router::body::from_bytes(vec![]))
                        .unwrap(),
                )
            });

            // Create an externalizable request
            let externalizable = Externalizable::<String>::router_builder()
                .stage(PipelineStep::RouterRequest)
                .id("test-id".to_string())
                .build();

            // Make the call which should create the HTTP request span
            let _ = externalizable
                .call(service, "http://example.com/test")
                .await;
        }
        .with_subscriber(assert_snapshot_subscriber!())
        .await;
    }
}
