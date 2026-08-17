//! APQ retry/error-detection Tower layer for subgraph calls.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering::Relaxed;
use std::task::Poll;

use futures::future::BoxFuture;
use serde_json_bytes::json;
use tower::BoxError;
use tower::Layer;
use tower::Service;
use tower::ServiceExt;

use super::calculate_hash_for_query;
use crate::graphql;
use crate::services::SubgraphRequest;
use crate::services::SubgraphResponse;

const PERSISTED_QUERY_NOT_FOUND_EXTENSION_CODE: &str = "PERSISTED_QUERY_NOT_FOUND";
const PERSISTED_QUERY_NOT_SUPPORTED_EXTENSION_CODE: &str = "PERSISTED_QUERY_NOT_SUPPORTED";
const PERSISTED_QUERY_NOT_FOUND_MESSAGE: &str = "PersistedQueryNotFound";
const PERSISTED_QUERY_NOT_SUPPORTED_MESSAGE: &str = "PersistedQueryNotSupported";
const CODE_STRING: &str = "code";
pub(crate) const PERSISTED_QUERY_KEY: &str = "persistedQuery";
const HASH_VERSION_KEY: &str = "version";
const HASH_VERSION_VALUE: i32 = 1;
const HASH_KEY: &str = "sha256Hash";

enum ApqError {
    PersistedQueryNotSupported,
    PersistedQueryNotFound,
    Other,
}

fn get_apq_error(gql_response: &graphql::Response) -> ApqError {
    for error in &gql_response.errors {
        match error.message.as_str() {
            PERSISTED_QUERY_NOT_FOUND_MESSAGE => return ApqError::PersistedQueryNotFound,
            PERSISTED_QUERY_NOT_SUPPORTED_MESSAGE => return ApqError::PersistedQueryNotSupported,
            _ => {}
        }
        if let Some(value) = error.extensions.get(CODE_STRING) {
            if value == PERSISTED_QUERY_NOT_FOUND_EXTENSION_CODE {
                return ApqError::PersistedQueryNotFound;
            } else if value == PERSISTED_QUERY_NOT_SUPPORTED_EXTENSION_CODE {
                return ApqError::PersistedQueryNotSupported;
            }
        }
    }
    ApqError::Other
}

/// Tower [`Layer`] that adds APQ retry logic for subgraph calls.
///
/// On the first request it sends only the query hash. If the subgraph
/// returns `PERSISTED_QUERY_NOT_FOUND`, it retries with the full query and hash.
/// If it returns `PERSISTED_QUERY_NOT_SUPPORTED`, APQ is disabled for this instance
/// and the retry is sent as a plain request without the `persistedQuery` extension.
pub(crate) struct SubgraphApqLayer {
    enabled: bool,
}

impl SubgraphApqLayer {
    pub(crate) fn new(enabled: bool) -> Self {
        Self { enabled }
    }
}

impl<S> Layer<S> for SubgraphApqLayer {
    type Service = SubgraphApqService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        SubgraphApqService {
            inner,
            enabled: Arc::new(AtomicBool::new(self.enabled)),
        }
    }
}

/// Tower service wrapping an inner subgraph service with APQ retry logic.
pub(crate) struct SubgraphApqService<S> {
    inner: S,
    /// Whether APQs are enabled for this subgraph. We automatically flip it to disabled for future requests
    /// if the subgraph reports that PQs are not supported.
    pub(crate) enabled: Arc<AtomicBool>,
}

impl<S: Clone> Clone for SubgraphApqService<S> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            enabled: self.enabled.clone(),
        }
    }
}

impl<S> Service<SubgraphRequest> for SubgraphApqService<S>
where
    S: Service<SubgraphRequest, Response = SubgraphResponse, Error = BoxError>
        + Clone
        + Send
        + 'static,
    S::Future: Send + 'static,
{
    type Response = SubgraphResponse;
    type Error = BoxError;
    type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn poll_ready(&mut self, cx: &mut std::task::Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, mut request: SubgraphRequest) -> Self::Future {
        let enabled = self.enabled.clone();
        let inner = self.inner.clone();
        let mut inner = std::mem::replace(&mut self.inner, inner);

        Box::pin(async move {
            if !enabled.load(Relaxed) {
                return inner.call(request).await;
            }

            // APQ: send only the hash on the first attempt, preserving all other body fields.
            let body = request.subgraph_request.body_mut();
            let original_query = body.query.take();
            let hash_value =
                calculate_hash_for_query(original_query.as_deref().unwrap_or_default());
            body.extensions.insert(
                PERSISTED_QUERY_KEY,
                json!({
                    HASH_VERSION_KEY: HASH_VERSION_VALUE,
                    HASH_KEY: hash_value
                }),
            );

            let response = inner.call(request.clone()).await?;

            match get_apq_error(response.response.body()) {
                ApqError::PersistedQueryNotSupported => {
                    enabled.store(false, Relaxed);
                    let body = request.subgraph_request.body_mut();
                    body.query = original_query;
                    body.extensions.remove(PERSISTED_QUERY_KEY);
                    inner.ready().await?.call(request).await
                }
                ApqError::PersistedQueryNotFound => {
                    request.subgraph_request.body_mut().query = original_query;
                    inner.ready().await?.call(request).await
                }
                ApqError::Other => Ok(response),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicUsize;
    use std::sync::atomic::Ordering::Relaxed;

    use serde_json_bytes::ByteString;
    use serde_json_bytes::Value;
    use tower::ServiceExt;

    use super::*;
    use crate::Context;
    use crate::graphql::Error;
    use crate::services::SubgraphRequest;
    use crate::services::SubgraphResponse;

    fn make_request() -> SubgraphRequest {
        SubgraphRequest::fake_builder()
            .subgraph_request(
                http::Request::builder()
                    .body(crate::graphql::Request::builder().query("query").build())
                    .unwrap(),
            )
            .context(Context::new())
            .build()
    }

    fn success_response(context: Context) -> SubgraphResponse {
        SubgraphResponse::fake_builder()
            .data(Value::String(ByteString::from("test")))
            .context(context)
            .build()
    }

    fn pqnf_message_response(context: Context) -> SubgraphResponse {
        SubgraphResponse::fake_builder()
            .errors(vec![
                Error::request_error_builder()
                    .message(PERSISTED_QUERY_NOT_FOUND_MESSAGE)
                    .build(),
            ])
            .context(context)
            .build()
    }

    fn pqns_message_response(context: Context) -> SubgraphResponse {
        SubgraphResponse::fake_builder()
            .errors(vec![
                Error::request_error_builder()
                    .message(PERSISTED_QUERY_NOT_SUPPORTED_MESSAGE)
                    .build(),
            ])
            .context(context)
            .build()
    }

    fn pqnf_extension_code_response(context: Context) -> SubgraphResponse {
        SubgraphResponse::fake_builder()
            .errors(vec![
                Error::request_error_builder()
                    .message("Random message")
                    .extension_code(PERSISTED_QUERY_NOT_FOUND_EXTENSION_CODE)
                    .build(),
            ])
            .context(context)
            .build()
    }

    fn pqns_extension_code_response(context: Context) -> SubgraphResponse {
        SubgraphResponse::fake_builder()
            .errors(vec![
                Error::request_error_builder()
                    .message("Random message")
                    .extension_code(PERSISTED_QUERY_NOT_SUPPORTED_EXTENSION_CODE)
                    .build(),
            ])
            .context(context)
            .build()
    }

    #[tokio::test]
    async fn test_apq_disabled_passes_query_through_unchanged() {
        let (mock, mut handle) = tower_test::mock::pair::<SubgraphRequest, SubgraphResponse>();
        let driver = tokio::spawn(async move {
            let (req, responder) = handle.next_request().await.unwrap();
            assert!(
                req.subgraph_request.body().query.is_some(),
                "query should be present when APQ is disabled"
            );
            assert!(
                !req.subgraph_request
                    .body()
                    .extensions
                    .contains_key(PERSISTED_QUERY_KEY),
                "no persistedQuery extension expected when APQ is disabled"
            );
            responder.send_response(success_response(req.context));
        });

        let svc = SubgraphApqLayer::new(false).layer(mock);
        let resp = svc.oneshot(make_request()).await.unwrap();
        crate::plugin::test::await_mock_driver(driver).await;
        assert_eq!(
            resp.response.body().data,
            Some(Value::String(ByteString::from("test")))
        );
    }

    #[tokio::test]
    async fn test_apq_enabled_sends_hash_only_on_first_attempt() {
        let (mock, mut handle) = tower_test::mock::pair::<SubgraphRequest, SubgraphResponse>();
        let driver = tokio::spawn(async move {
            let (req, responder) = handle.next_request().await.unwrap();
            assert!(
                req.subgraph_request.body().query.is_none(),
                "query should be absent on first APQ attempt"
            );
            assert!(
                req.subgraph_request
                    .body()
                    .extensions
                    .contains_key(PERSISTED_QUERY_KEY),
                "persistedQuery hash should be present"
            );
            responder.send_response(success_response(req.context));
        });

        let svc = SubgraphApqLayer::new(true).layer(mock);
        let resp = svc.oneshot(make_request()).await.unwrap();
        crate::plugin::test::await_mock_driver(driver).await;
        assert_eq!(
            resp.response.body().data,
            Some(Value::String(ByteString::from("test")))
        );
    }

    #[tokio::test]
    async fn test_persisted_query_not_found_message_retries_with_query() {
        let (mock, mut handle) = tower_test::mock::pair::<SubgraphRequest, SubgraphResponse>();
        let driver = tokio::spawn(async move {
            let (req, responder) = handle.next_request().await.unwrap();
            responder.send_response(pqnf_message_response(req.context));

            let (req, responder) = handle.next_request().await.unwrap();
            assert!(
                req.subgraph_request.body().query.is_some(),
                "retry should include the full query"
            );
            assert!(
                req.subgraph_request
                    .body()
                    .extensions
                    .contains_key(PERSISTED_QUERY_KEY),
                "retry should keep the persistedQuery hash"
            );
            responder.send_response(success_response(req.context));
        });

        let svc = SubgraphApqLayer::new(true).layer(mock);
        let resp = svc.oneshot(make_request()).await.unwrap();
        crate::plugin::test::await_mock_driver(driver).await;
        assert_eq!(
            resp.response.body().data,
            Some(Value::String(ByteString::from("test")))
        );
    }

    #[tokio::test]
    async fn test_persisted_query_not_found_extension_code_retries_with_query() {
        let (mock, mut handle) = tower_test::mock::pair::<SubgraphRequest, SubgraphResponse>();
        let driver = tokio::spawn(async move {
            let (req, responder) = handle.next_request().await.unwrap();
            responder.send_response(pqnf_extension_code_response(req.context));

            let (req, responder) = handle.next_request().await.unwrap();
            assert!(req.subgraph_request.body().query.is_some());
            responder.send_response(success_response(req.context));
        });

        let svc = SubgraphApqLayer::new(true).layer(mock);
        let resp = svc.oneshot(make_request()).await.unwrap();
        crate::plugin::test::await_mock_driver(driver).await;
        assert_eq!(
            resp.response.body().data,
            Some(Value::String(ByteString::from("test")))
        );
    }

    #[tokio::test]
    async fn test_persisted_query_not_supported_message_disables_apq() {
        let (mock, mut handle) = tower_test::mock::pair::<SubgraphRequest, SubgraphResponse>();
        let driver = tokio::spawn(async move {
            let (req, responder) = handle.next_request().await.unwrap();
            responder.send_response(pqns_message_response(req.context));

            let (req, responder) = handle.next_request().await.unwrap();
            assert!(
                req.subgraph_request.body().query.is_some(),
                "retry should include the full query"
            );
            assert!(
                !req.subgraph_request
                    .body()
                    .extensions
                    .contains_key(PERSISTED_QUERY_KEY),
                "persistedQuery extension should be removed for PQNS retry"
            );
            responder.send_response(success_response(req.context));
        });

        let layer = SubgraphApqLayer::new(true);
        let svc = layer.layer(mock);
        assert!(svc.enabled.load(Relaxed), "APQ should start enabled");

        let resp = svc.clone().oneshot(make_request()).await.unwrap();
        crate::plugin::test::await_mock_driver(driver).await;
        assert_eq!(
            resp.response.body().data,
            Some(Value::String(ByteString::from("test")))
        );
        assert!(
            !svc.enabled.load(Relaxed),
            "APQ should be disabled after PQNS"
        );
    }

    #[tokio::test]
    async fn test_persisted_query_not_supported_extension_code_disables_apq() {
        let (mock, mut handle) = tower_test::mock::pair::<SubgraphRequest, SubgraphResponse>();
        let driver = tokio::spawn(async move {
            let (req, responder) = handle.next_request().await.unwrap();
            responder.send_response(pqns_extension_code_response(req.context));

            let (req, responder) = handle.next_request().await.unwrap();
            assert!(req.subgraph_request.body().query.is_some());
            assert!(
                !req.subgraph_request
                    .body()
                    .extensions
                    .contains_key(PERSISTED_QUERY_KEY)
            );
            responder.send_response(success_response(req.context));
        });

        let layer = SubgraphApqLayer::new(true);
        let svc = layer.layer(mock);
        assert!(svc.enabled.load(Relaxed));

        let resp = svc.clone().oneshot(make_request()).await.unwrap();
        crate::plugin::test::await_mock_driver(driver).await;
        assert_eq!(
            resp.response.body().data,
            Some(Value::String(ByteString::from("test")))
        );
        assert!(!svc.enabled.load(Relaxed));
    }

    const APQ_TEST_QUERY: &str = "query MyOp($id: ID!) { thing(id: $id) { name } }";

    #[tokio::test]
    async fn test_apq_body_preserves_all_fields() {
        let (mock, mut handle) = tower_test::mock::pair::<SubgraphRequest, SubgraphResponse>();
        let driver = tokio::spawn(async move {
            let (req, responder) = handle.next_request().await.unwrap();
            let body = req.subgraph_request.body();
            assert!(
                body.query.is_none(),
                "APQ body should omit query on first attempt"
            );
            assert_eq!(body.operation_name.as_deref(), Some("MyOp"));
            assert_eq!(
                body.variables.get("id"),
                Some(&serde_json_bytes::json!("42"))
            );
            assert!(body.extensions.contains_key(PERSISTED_QUERY_KEY));
            assert!(
                body.extensions.contains_key("myExt"),
                "custom extensions should be preserved alongside persistedQuery"
            );
            responder.send_response(success_response(req.context));
        });

        let gql_body = crate::graphql::Request {
            query: Some(APQ_TEST_QUERY.to_string()),
            operation_name: Some("MyOp".to_string()),
            variables: serde_json_bytes::json!({"id": "42"})
                .as_object()
                .unwrap()
                .clone(),
            extensions: serde_json_bytes::json!({"myExt": "value"})
                .as_object()
                .unwrap()
                .clone(),
        };

        let request = SubgraphRequest::fake_builder()
            .subgraph_request(http::Request::builder().body(gql_body).unwrap())
            .context(Context::new())
            .build();

        let svc = SubgraphApqLayer::new(true).layer(mock);
        let resp = svc.oneshot(request).await.unwrap();
        crate::plugin::test::await_mock_driver(driver).await;
        assert!(resp.response.body().errors.is_empty());
    }

    #[tokio::test]
    async fn test_apq_not_found_retry_preserves_original_body() {
        let (mock, mut handle) = tower_test::mock::pair::<SubgraphRequest, SubgraphResponse>();
        let driver = tokio::spawn(async move {
            let (req, responder) = handle.next_request().await.unwrap();
            assert!(
                req.subgraph_request
                    .body()
                    .extensions
                    .contains_key(PERSISTED_QUERY_KEY),
                "both attempts should include the persistedQuery hash"
            );
            responder.send_response(pqnf_message_response(req.context));

            let (req, responder) = handle.next_request().await.unwrap();
            let body = req.subgraph_request.body();
            assert!(
                body.extensions.contains_key(PERSISTED_QUERY_KEY),
                "both attempts should include the persistedQuery hash"
            );
            assert_eq!(
                body.query.as_deref(),
                Some(APQ_TEST_QUERY),
                "retry should send the original query string"
            );
            assert_eq!(
                body.operation_name.as_deref(),
                Some("MyOp"),
                "operation_name should be preserved on retry"
            );
            assert_eq!(
                body.variables.get("id"),
                Some(&serde_json_bytes::json!("42")),
                "variables should be preserved on retry"
            );
            responder.send_response(success_response(req.context));
        });

        let gql_body = crate::graphql::Request {
            query: Some(APQ_TEST_QUERY.to_string()),
            operation_name: Some("MyOp".to_string()),
            variables: serde_json_bytes::json!({"id": "42"})
                .as_object()
                .unwrap()
                .clone(),
            extensions: serde_json_bytes::json!({"myExt": "value"})
                .as_object()
                .unwrap()
                .clone(),
        };

        let request = SubgraphRequest::fake_builder()
            .subgraph_request(http::Request::builder().body(gql_body).unwrap())
            .context(Context::new())
            .build();

        let svc = SubgraphApqLayer::new(true).layer(mock);
        let resp = svc.oneshot(request).await.unwrap();
        crate::plugin::test::await_mock_driver(driver).await;
        assert!(resp.response.body().errors.is_empty());
    }

    #[tokio::test]
    async fn test_apq_not_supported_retry_restores_original_body() {
        let (mock, mut handle) = tower_test::mock::pair::<SubgraphRequest, SubgraphResponse>();
        let driver = tokio::spawn(async move {
            let (req, responder) = handle.next_request().await.unwrap();
            responder.send_response(pqns_message_response(req.context));

            let (req, responder) = handle.next_request().await.unwrap();
            let body = req.subgraph_request.body();
            assert_eq!(
                body.query.as_deref(),
                Some(APQ_TEST_QUERY),
                "retry should send the original query string"
            );
            assert!(
                !body.extensions.contains_key(PERSISTED_QUERY_KEY),
                "persistedQuery extension should be removed on PQNS retry"
            );
            assert!(
                body.extensions.contains_key("myExt"),
                "other extensions should be preserved on retry"
            );
            responder.send_response(success_response(req.context));
        });

        let gql_body = crate::graphql::Request {
            query: Some(APQ_TEST_QUERY.to_string()),
            operation_name: Some("MyOp".to_string()),
            variables: serde_json_bytes::json!({"id": "42"})
                .as_object()
                .unwrap()
                .clone(),
            extensions: serde_json_bytes::json!({"myExt": "value"})
                .as_object()
                .unwrap()
                .clone(),
        };

        let request = SubgraphRequest::fake_builder()
            .subgraph_request(http::Request::builder().body(gql_body).unwrap())
            .context(Context::new())
            .build();

        let svc = SubgraphApqLayer::new(true).layer(mock);
        let resp = svc.oneshot(request).await.unwrap();
        crate::plugin::test::await_mock_driver(driver).await;
        assert!(resp.response.body().errors.is_empty());
    }

    mod http_tests {
        use std::convert::Infallible;
        use std::str::FromStr;
        use std::sync::Arc;

        use axum::body::Body;
        use bytes::Buf;
        use http::StatusCode;
        use http::Uri;
        use http::header::CONTENT_TYPE;
        use mime::APPLICATION_JSON;
        use serde_json_bytes::ByteString;
        use tokio::net::TcpListener;
        use tower::ServiceExt;

        use super::*;
        use crate::graphql::Response;
        use crate::query_planner::fetch::OperationKind;
        use crate::services::SubgraphService;
        use crate::services::http::HttpClientServiceFactory;
        use crate::services::router;

        async fn serve<Handler, Fut>(listener: TcpListener, handle: Handler) -> std::io::Result<()>
        where
            Handler: (Fn(http::Request<Body>) -> Fut) + Clone + Sync + Send + 'static,
            Fut: std::future::Future<Output = Result<http::Response<Body>, Infallible>>
                + Send
                + 'static,
        {
            use hyper::body::Incoming;
            use hyper_util::rt::TokioExecutor;
            use hyper_util::rt::TokioIo;

            loop {
                let (stream, _) = listener.accept().await?;
                let io = TokioIo::new(stream);
                let handle = handle.clone();
                tokio::spawn(async move {
                    let svc = hyper::service::service_fn(|request: http::Request<Incoming>| {
                        handle(request.map(Body::new))
                    });
                    if let Err(err) =
                        hyper_util::server::conn::auto::Builder::new(TokioExecutor::new())
                            .serve_connection_with_upgrades(io, svc)
                            .await
                    {
                        eprintln!("server error: {err}");
                    }
                });
            }
        }

        async fn parse_graphql_request(body: Body) -> crate::graphql::Request {
            let bytes = router::body::into_bytes(body)
                .await
                .expect("can read request body");
            serde_json::from_reader(bytes.reader()).expect("valid graphql request")
        }

        fn success_response() -> http::Response<Body> {
            http::Response::builder()
                .header(CONTENT_TYPE, APPLICATION_JSON.essence_str())
                .status(StatusCode::OK)
                .body(
                    serde_json::to_string(&Response {
                        data: Some(Value::String(ByteString::from("test"))),
                        ..Response::default()
                    })
                    .expect("always valid")
                    .into(),
                )
                .unwrap()
        }

        fn subgraph_request(uri: Uri, query: &str) -> SubgraphRequest {
            SubgraphRequest::builder()
                .supergraph_request(Arc::new(
                    http::Request::builder()
                        .body(crate::graphql::Request::builder().query(query).build())
                        .unwrap(),
                ))
                .subgraph_request(
                    http::Request::builder()
                        .header(CONTENT_TYPE, APPLICATION_JSON.essence_str())
                        .uri(uri)
                        .body(crate::graphql::Request::builder().query(query).build())
                        .unwrap(),
                )
                .operation_kind(OperationKind::Query)
                .subgraph_name(String::from("test"))
                .context(Context::new())
                .build()
        }

        fn layered_subgraph_service(
            name: &str,
            enable_apq: bool,
        ) -> SubgraphApqService<SubgraphService> {
            SubgraphApqLayer::new(enable_apq).layer(
                SubgraphService::new(name, HttpClientServiceFactory::for_test(name))
                    .expect("can create a SubgraphService"),
            )
        }

        #[tokio::test(flavor = "multi_thread")]
        async fn apq_enabled_sends_hash_only_over_http() {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let socket_addr = listener.local_addr().unwrap();
            tokio::task::spawn(async move {
                serve(listener, |request| async move {
                    let graphql_request = parse_graphql_request(request.into_body()).await;
                    assert!(
                        graphql_request.extensions.contains_key(PERSISTED_QUERY_KEY),
                        "persistedQuery expected when APQ is enabled"
                    );
                    assert!(
                        graphql_request.query.is_none(),
                        "query should be omitted on the first APQ attempt"
                    );
                    Ok(success_response())
                })
                .await
                .unwrap();
            });

            let url = Uri::from_str(&format!("http://{socket_addr}")).unwrap();
            let resp = layered_subgraph_service("test", true)
                .oneshot(subgraph_request(url, "query"))
                .await
                .unwrap();

            assert_eq!(
                resp.response.body().data,
                Some(Value::String(ByteString::from("test")))
            );
        }

        #[tokio::test(flavor = "multi_thread")]
        async fn apq_disabled_sends_full_query_over_http() {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let socket_addr = listener.local_addr().unwrap();
            tokio::task::spawn(async move {
                serve(listener, |request| async move {
                    let graphql_request = parse_graphql_request(request.into_body()).await;
                    assert!(
                        !graphql_request.extensions.contains_key(PERSISTED_QUERY_KEY),
                        "persistedQuery not expected when APQ is disabled"
                    );
                    assert!(
                        graphql_request.query.is_some(),
                        "query should be present when APQ is disabled"
                    );
                    Ok(success_response())
                })
                .await
                .unwrap();
            });

            let url = Uri::from_str(&format!("http://{socket_addr}")).unwrap();
            let resp = layered_subgraph_service("test", false)
                .oneshot(subgraph_request(url, "query"))
                .await
                .unwrap();

            assert_eq!(
                resp.response.body().data,
                Some(Value::String(ByteString::from("test")))
            );
        }

        #[tokio::test(flavor = "multi_thread")]
        async fn apq_not_found_retries_with_query_over_http() {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let socket_addr = listener.local_addr().unwrap();
            let call_count = Arc::new(AtomicUsize::new(0));
            let call_count_task = call_count.clone();
            tokio::task::spawn(async move {
                serve(listener, move |request| {
                    let call_count = call_count_task.clone();
                    async move {
                        let n = call_count.fetch_add(1, Relaxed);
                        let graphql_request = parse_graphql_request(request.into_body()).await;
                        assert!(
                            graphql_request.extensions.contains_key(PERSISTED_QUERY_KEY),
                            "both attempts should include the persistedQuery hash"
                        );

                        if n == 0 {
                            assert!(graphql_request.query.is_none());
                            let pqnf_response = Response {
                                errors: vec![
                                    Error::request_error_builder()
                                        .message(PERSISTED_QUERY_NOT_FOUND_MESSAGE)
                                        .build(),
                                ],
                                ..Response::default()
                            };
                            return Ok(http::Response::builder()
                                .header(CONTENT_TYPE, APPLICATION_JSON.essence_str())
                                .status(StatusCode::OK)
                                .body(serde_json::to_string(&pqnf_response).unwrap().into())
                                .unwrap());
                        }

                        assert_eq!(graphql_request.query.as_deref(), Some("query"));
                        Ok(success_response())
                    }
                })
                .await
                .unwrap();
            });

            let url = Uri::from_str(&format!("http://{socket_addr}")).unwrap();
            let resp = layered_subgraph_service("test", true)
                .oneshot(subgraph_request(url, "query"))
                .await
                .unwrap();

            assert_eq!(call_count.load(Relaxed), 2);
            assert_eq!(
                resp.response.body().data,
                Some(Value::String(ByteString::from("test")))
            );
        }
    }
}
