//! Mock connector implementation

#![allow(missing_docs)] // FIXME

use std::collections::HashMap;
use std::sync::Arc;
use std::task::Poll;

use apollo_federation::connectors::runtime::http_json_transport::TransportRequest;
use futures::future;
use http::HeaderMap;
use http::HeaderName;
use http::HeaderValue;
use serde_json_bytes::json;
use tower::BoxError;
use tower::Service;

use crate::json_ext::Object;
use crate::services::connector::request_service::Request as ConnectorRequest;
use crate::services::connector::request_service::Response as ConnectorResponse;

type MockResponses = HashMap<String, String>;

#[derive(Default, Clone)]
pub struct MockConnector {
    // using an arc to improve efficiency when service is cloned
    mocks: Arc<MockResponses>,
    extensions: Option<Object>,
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

    pub fn builder() -> MockConnectorBuilder {
        MockConnectorBuilder::default()
    }

    pub fn with_extensions(mut self, extensions: Object) -> Self {
        self.extensions = Some(extensions);
        self
    }
}

/// Builder for `MockConnector`
#[derive(Default, Clone)]
pub struct MockConnectorBuilder {
    mocks: MockResponses,
    extensions: Option<Object>,
    headers: HeaderMap,
}
impl MockConnectorBuilder {
    pub fn with_extensions(mut self, extensions: Object) -> Self {
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
        let TransportRequest::Http(http) = req.transport_request;
        let body = http.inner.body();

        let response = if let Some(response) = self.mocks.get(body) {
            let response_key = req.key;
            let data = json!(response);
            let headers = self.headers.clone();

            ConnectorResponse::test_new(response_key, Default::default(), data, Some(headers))
        } else {
            let error_message = format!(
                "couldn't find mock for query {}",
                serde_json::to_string(&body).unwrap()
            );
            let response_key = req.key;
            let data = json!(error_message);
            let headers = self.headers.clone();

            ConnectorResponse::test_new(response_key, Default::default(), data, Some(headers))
        };
        future::ok(response)
    }
}
