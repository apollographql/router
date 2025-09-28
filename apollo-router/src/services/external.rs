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

        // Handle Unix socket URL conversion (similar to subgraph implementation)
        #[cfg(unix)]
        let (converted_uri, is_unix_socket, unix_path) = if let Some(path) = uri.strip_prefix("unix://") {
            tracing::debug!("using Unix domain socket transport to coprocessor: {}", path);
            // Convert unix:// URL to hyperlocal format that UnixConnector can understand
            let hyperlocal_uri: http::Uri = hyperlocal::Uri::new(path, "/").into();
            (hyperlocal_uri, true, Some(path))
        } else {
            tracing::debug!("using HTTP transport to coprocessor: {}", uri);
            (uri.parse()?, false, None)
        };
        #[cfg(not(unix))]
        let (converted_uri, is_unix_socket, unix_path) = {
            tracing::debug!("using HTTP transport to coprocessor: {}", uri);
            (uri.parse()?, false, None::<&str>)
        };

        let mut request = http::Request::builder()
            .uri(converted_uri)
            .method(Method::POST)
            .header(ACCEPT, "application/json")
            .header(CONTENT_TYPE, "application/json")
            .body(router::body::from_bytes(serde_json::to_vec(&self)?))?;

        let schema_uri = request.uri();

        // For tracing, use more meaningful attributes depending on transport type
        let (span_host, span_port, span_transport, span_url) = if is_unix_socket {
            // For Unix sockets, show the socket path as the server address
            let socket_path = unix_path.unwrap_or("unknown");
            (socket_path, 0u16, "unix", format!("unix://{}", socket_path))
        } else {
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
            (host, port, "ip_tcp", uri.to_string())
        };

        let otel_name = format!("POST {}", span_url);

        let http_req_span = tracing::info_span!(HTTP_REQUEST_SPAN_NAME,
            "otel.kind" = "CLIENT",
            "http.request.method" = "POST",
            "server.address" = %span_host,
            "server.port" = %span_port,
            "url.full" = %span_url,
            "net.transport" = %span_transport,
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

    #[cfg(unix)]
    #[tokio::test]
    async fn it_will_create_unix_socket_request_span() {
        async {
            // Create a mock service that returns a simple response
            let service = service_fn(|req: http::Request<RouterBody>| async move {
                tracing::info!("got unix socket request");
                // Verify the URI was converted to hyperlocal format
                assert!(req.uri().host().is_some(), "Unix socket URI should have encoded host");
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

            // Make the call with a Unix socket URL which should create proper tracing
            let _ = externalizable
                .call(service, "unix:///tmp/test.sock")
                .await;
        }
        .with_subscriber(assert_snapshot_subscriber!())
        .await;
    }

    #[test]
    fn test_unix_socket_url_detection() {
        // Test Unix socket URL detection
        let unix_url = "unix:///tmp/socket.sock";
        assert!(unix_url.starts_with("unix://"));

        let http_url = "http://localhost:8080";
        assert!(!http_url.starts_with("unix://"));

        let https_url = "https://example.com/api";
        assert!(!https_url.starts_with("unix://"));
    }

    #[cfg(unix)]
    #[test]
    fn test_unix_socket_uri_conversion() {
        use hyperlocal::Uri as UnixUri;

        // Test that we can create hyperlocal URIs for valid paths
        let socket_path = "/tmp/test.sock";
        let hyperlocal_uri: http::Uri = UnixUri::new(socket_path, "/").into();

        // Verify the conversion produces a valid URI
        assert!(hyperlocal_uri.host().is_some());
        assert_eq!(hyperlocal_uri.path(), "/");
    }

    #[tokio::test]
    async fn test_http_url_preserves_original_behavior() {
        async {
            let service = service_fn(|req: http::Request<RouterBody>| async move {
                tracing::info!("got http request");
                // Verify HTTP URLs are processed normally
                assert_eq!(req.uri().host(), Some("example.com"));
                assert_eq!(req.uri().path(), "/test");
                Ok::<_, BoxError>(
                    Response::builder()
                        .status(200)
                        .body(router::body::from_bytes(vec![]))
                        .unwrap(),
                )
            });

            let externalizable = Externalizable::<String>::router_builder()
                .stage(PipelineStep::RouterRequest)
                .id("test-id".to_string())
                .build();

            // Verify HTTP URLs still work as before
            let _ = externalizable
                .call(service, "http://example.com/test")
                .await;
        }
        .with_subscriber(assert_snapshot_subscriber!())
        .await;
    }
}
