//! Mock subgraph implementation

#![allow(missing_docs)] // FIXME

use std::collections::HashMap;
use std::sync::Arc;
use std::task::Poll;

use futures::future;
use http::StatusCode;
use tower::BoxError;
use tower::Service;

use crate::graphql;
use crate::graphql::Request;
use crate::graphql::Response;
use crate::json_ext::Object;
use crate::notification::Handle;
use crate::services::SubgraphRequest;
use crate::services::SubgraphResponse;

type MockResponses = HashMap<Request, Response>;

#[derive(Clone, Default)]
pub struct MockSubgraph {
    // using an arc to improve efficiency when service is cloned
    mocks: Arc<MockResponses>,
    extensions: Option<Object>,
    subscription_stream: Option<Handle<String, graphql::Response>>,
}

impl MockSubgraph {
    pub fn new(mocks: MockResponses) -> Self {
        Self {
            mocks: Arc::new(mocks),
            extensions: None,
            subscription_stream: None,
        }
    }

    pub fn builder() -> MockSubgraphBuilder {
        MockSubgraphBuilder::default()
    }

    pub fn with_extensions(mut self, extensions: Object) -> Self {
        self.extensions = Some(extensions);
        self
    }

    pub fn with_subscription_stream(
        mut self,
        subscription_stream: Handle<String, graphql::Response>,
    ) -> Self {
        self.subscription_stream = Some(subscription_stream);
        self
    }
}

/// Builder for `MockSubgraph`
#[derive(Clone, Default)]
pub struct MockSubgraphBuilder {
    mocks: MockResponses,
    extensions: Option<Object>,
    subscription_stream: Option<Handle<String, graphql::Response>>,
}
impl MockSubgraphBuilder {
    pub fn with_extensions(mut self, extensions: Object) -> Self {
        self.extensions = Some(extensions);
        self
    }

    /// adds a mocked response for a request
    ///
    /// the arguments must deserialize to `crate::graphql::Request` and `crate::graphql::Response`
    pub fn with_json(mut self, request: serde_json::Value, response: serde_json::Value) -> Self {
        self.mocks.insert(
            serde_json::from_value(request).unwrap(),
            serde_json::from_value(response).unwrap(),
        );

        self
    }

    pub fn with_subscription_stream(
        mut self,
        subscription_stream: Handle<String, graphql::Response>,
    ) -> Self {
        self.subscription_stream = Some(subscription_stream);
        self
    }

    pub fn build(self) -> MockSubgraph {
        MockSubgraph {
            mocks: Arc::new(self.mocks),
            extensions: self.extensions,
            subscription_stream: self.subscription_stream,
        }
    }
}

impl Service<SubgraphRequest> for MockSubgraph {
    type Response = SubgraphResponse;

    type Error = BoxError;

    type Future = future::Ready<Result<Self::Response, Self::Error>>;

    fn poll_ready(&mut self, _cx: &mut std::task::Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, mut req: SubgraphRequest) -> Self::Future {
        let body = req.subgraph_request.body_mut();

        if let Some(sub_stream) = &mut req.subscription_stream {
            sub_stream
                .try_send(
                    self.subscription_stream
                        .take()
                        .expect("must have a subscription stream set")
                        .into_stream(),
                )
                .unwrap();
        }

        // Redact the callback url and subscription_id because it generates a subscription uuid
        if let Some(serde_json_bytes::Value::Object(subscription_ext)) =
            body.extensions.get_mut("subscription")
        {
            if let Some(callback_url) = subscription_ext.get_mut("callback_url") {
                let mut cb_url = url::Url::parse(
                    callback_url
                        .as_str()
                        .expect("callback_url extension must be a string"),
                )
                .expect("callback_url must be a valid URL");
                cb_url.path_segments_mut().unwrap().pop();
                cb_url.path_segments_mut().unwrap().push("subscription_id");

                *callback_url = serde_json_bytes::Value::String(cb_url.to_string().into());
            }
            if let Some(subscription_id) = subscription_ext.get_mut("subscription_id") {
                *subscription_id =
                    serde_json_bytes::Value::String("subscription_id".to_string().into());
            }
        }

        let response = if let Some(response) = self.mocks.get(body) {
            // Build an http Response
            let http_response = http::Response::builder()
                .status(StatusCode::OK)
                .body(response.clone())
                .expect("Response is serializable; qed");
            SubgraphResponse::new_from_response(http_response, req.context)
        } else {
            let error = crate::error::Error::builder()
                .message(format!(
                    "couldn't find mock for query {}",
                    serde_json::to_string(body).unwrap()
                ))
                .extension_code("FETCH_ERROR".to_string())
                .extensions(self.extensions.clone().unwrap_or_default())
                .build();
            SubgraphResponse::fake_builder()
                .error(error)
                .context(req.context)
                .build()
        };
        future::ok(response)
    }
}
