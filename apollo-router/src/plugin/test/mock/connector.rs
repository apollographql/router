//! Mock connector implementation

#![allow(missing_docs)] // FIXME

use std::collections::HashMap;
use std::sync::Arc;
use std::task::Poll;

use apollo_federation::connectors::runtime::http_json_transport::TransportRequest;
use apollo_json::Value;
use futures::future;
use http::HeaderMap;
use http::HeaderName;
use http::HeaderValue;
use serde_json_bytes::json;
use tower::BoxError;
use tower::Service;

use crate::services::connector::request_service::Request as ConnectorRequest;
use crate::services::connector::request_service::Response as ConnectorResponse;

type MockResponses = HashMap<String, String>;

#[derive(Default, Clone)]
pub struct MockConnector {
    // using an arc to improve efficiency when service is cloned
    mocks: Arc<MockResponses>,
    extensions: Option<Value>,
    map_request_fn:
        Option<Arc<dyn (Fn(ConnectorRequest) -> ConnectorRequest) + Send + Sync + 'static>>,
    headers: HeaderMap,
}

impl MockConnector {
    pub fn new(mocks: MockResponses) -> Self {
        Self {
            mocks: Arc::new(mocks.into_iter().collect()),
            extensions: None,
            map_request_fn: None,
            headers: HeaderMap::new(),
        }
    }

    /// Starts building a mock connector, one mocked request/response pair at a time.
    pub fn builder() -> MockConnectorBuilder {
        MockConnectorBuilder::default()
    }

    /// Sets the GraphQL error extensions carried by the mock. `extensions` must be an object.
    pub fn with_extensions(mut self, extensions: Value) -> Self {
        self.extensions = Some(extensions);
        self
    }
}

/// Builder for `MockConnector`
#[derive(Default, Clone)]
pub struct MockConnectorBuilder {
    mocks: MockResponses,
    extensions: Option<Value>,
    headers: HeaderMap,
}
impl MockConnectorBuilder {
    /// Sets the GraphQL error extensions carried by the mock. `extensions` must be an object.
    pub fn with_extensions(mut self, extensions: Value) -> Self {
        self.extensions = Some(extensions);
        self
    }

    /// adds a mocked response for a request
    ///
    /// the arguments must deserialize to `crate::graphql::Request` and `crate::graphql::Response`
    pub fn with_json(mut self, request: serde_json::Value, response: serde_json::Value) -> Self {
        let request = serde_json::from_value(request).unwrap();
        self.mocks
            .insert(request, serde_json::from_value(response).unwrap());
        self
    }

    pub fn with_header(mut self, name: HeaderName, value: HeaderValue) -> Self {
        self.headers.insert(name, value);
        self
    }

    pub fn build(self) -> MockConnector {
        MockConnector {
            mocks: Arc::new(self.mocks),
            extensions: self.extensions,
            map_request_fn: None,
            headers: self.headers,
        }
    }
}

impl Service<ConnectorRequest> for MockConnector {
    type Response = ConnectorResponse;

    type Error = BoxError;

    type Future = future::Ready<Result<Self::Response, Self::Error>>;

    fn poll_ready(&mut self, _cx: &mut std::task::Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, mut req: ConnectorRequest) -> Self::Future {
        if let Some(map_request_fn) = &self.map_request_fn {
            req = map_request_fn.clone()(req);
        }
        let TransportRequest::Http(http) = req.transport_request else {
            panic!("expected Http transport request");
        };
        let body = http.inner.body();

        let response = if let Some(response) = self.mocks.get(body) {
            let response_key = req.key;
            let data = json!(response);
            let headers = self.headers.clone();

            ConnectorResponse::test_new(
                req.context.clone(),
                response_key,
                vec![],
                data,
                Some(headers),
            )
        } else {
            let error_message = format!(
                "couldn't find mock for query {}",
                serde_json::to_string(&body).unwrap()
            );
            let response_key = req.key;
            let data = json!(error_message);
            let headers = self.headers.clone();

            ConnectorResponse::test_new(
                req.context.clone(),
                response_key,
                vec![],
                data,
                Some(headers),
            )
        };
        future::ok(response)
    }
}
