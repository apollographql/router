//! Mock subgraph implementation

#![allow(missing_docs)] // FIXME

use std::collections::HashMap;
use std::sync::Arc;
use std::task::Poll;

use apollo_compiler::ast::Definition;
use apollo_compiler::ast::Document;
use apollo_json::DocumentBuilder;
use apollo_json::JsonKind;
use apollo_json::Value;
use futures::future;
use http::HeaderMap;
use http::HeaderName;
use http::HeaderValue;
use http::StatusCode;
use tower::BoxError;
use tower::Service;

use crate::graphql;
use crate::graphql::Request;
use crate::graphql::Response;
use crate::json_ext::ObjectExt;
use crate::plugins::subscription::notification::Handle;
use crate::services::SubgraphRequest;
use crate::services::SubgraphResponse;

type MockResponses = HashMap<Request, Response>;

#[derive(Default, Clone)]
pub struct MockSubgraph {
    // using an arc to improve efficiency when service is cloned
    mocks: Arc<MockResponses>,
    extensions: Option<Value>,
    subscription_stream: Option<Handle<String, graphql::Response>>,
    map_request_fn:
        Option<Arc<dyn (Fn(SubgraphRequest) -> SubgraphRequest) + Send + Sync + 'static>>,
    headers: HeaderMap,
}

impl MockSubgraph {
    pub fn new(mocks: MockResponses) -> Self {
        Self {
            mocks: Arc::new(
                mocks
                    .into_iter()
                    .map(|(mut req, res)| {
                        normalize(&mut req);
                        (req, res)
                    })
                    .collect(),
            ),
            extensions: None,
            subscription_stream: None,
            map_request_fn: None,
            headers: HeaderMap::new(),
        }
    }

    /// Starts building a mock subgraph, one mocked request/response pair at a time.
    pub fn builder() -> MockSubgraphBuilder {
        MockSubgraphBuilder::default()
    }

    /// Sets the GraphQL error extensions the mock answers with when no mocked
    /// response matches the incoming request. `extensions` must be an object.
    pub fn with_extensions(mut self, extensions: Value) -> Self {
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

    #[cfg(test)]
    pub(crate) fn with_map_request<F>(mut self, map_request_fn: F) -> Self
    where
        F: (Fn(SubgraphRequest) -> SubgraphRequest) + Send + Sync + 'static,
    {
        self.map_request_fn = Some(Arc::new(map_request_fn));
        self
    }
}

/// Builder for `MockSubgraph`
#[derive(Default, Clone)]
pub struct MockSubgraphBuilder {
    mocks: MockResponses,
    extensions: Option<Value>,
    subscription_stream: Option<Handle<String, graphql::Response>>,
    headers: HeaderMap,
}
impl MockSubgraphBuilder {
    /// Sets the GraphQL error extensions the mock answers with when no mocked
    /// response matches the incoming request. `extensions` must be an object.
    pub fn with_extensions(mut self, extensions: Value) -> Self {
        self.extensions = Some(extensions);
        self
    }

    /// adds a mocked response for a request
    ///
    /// the arguments must deserialize to `crate::graphql::Request` and `crate::graphql::Response`
    pub fn with_json(mut self, request: serde_json::Value, response: serde_json::Value) -> Self {
        let mut request = deserialize_fixture(request);
        normalize(&mut request);
        self.mocks.insert(request, deserialize_fixture(response));
        self
    }

    pub fn with_subscription_stream(
        mut self,
        subscription_stream: Handle<String, graphql::Response>,
    ) -> Self {
        self.subscription_stream = Some(subscription_stream);
        self
    }

    pub fn with_header(mut self, name: HeaderName, value: HeaderValue) -> Self {
        self.headers.insert(name, value);
        self
    }

    pub fn build(self) -> MockSubgraph {
        MockSubgraph {
            mocks: Arc::new(self.mocks),
            extensions: self.extensions,
            subscription_stream: self.subscription_stream,
            map_request_fn: None,
            headers: self.headers,
        }
    }
}

/// Deserializes a `serde_json` fixture. A [`crate::json_ext::Value`] inside `T`
/// can only be captured from apollo-json's own deserializers, so the fixture
/// crosses over as a document first.
// PERF(apollo-json): legacy bridge, revisit -- test fixtures arrive as `serde_json::Value`.
fn deserialize_fixture<T: serde::de::DeserializeOwned>(fixture: serde_json::Value) -> T {
    let value = crate::json_ext::from_legacy(&fixture.into());
    apollo_json::from_value(&value).unwrap()
}

// Normalize queries so that spaces and operation names
// don't have an impact on the cache
fn normalize(request: &mut Request) {
    if let Some(q) = &request.query {
        let mut doc = Document::parse(q.clone(), "request").unwrap();

        if let Some(Definition::OperationDefinition(op)) = doc.definitions.first_mut() {
            let o = op.make_mut();
            o.name.take();
        };

        request.query = Some(doc.serialize().no_indent().to_string());
        request.operation_name = None;
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
        if let Some(map_request_fn) = &self.map_request_fn {
            req = map_request_fn.clone()(req);
        }
        let body = req.subgraph_request.body_mut();

        let subscription_stream = self.subscription_stream.clone();
        if let Some(sub_stream) = &mut req.subscription_stream {
            sub_stream
                .try_send(Box::pin(
                    subscription_stream
                        .expect("must have a subscription stream set")
                        .into_stream(),
                ))
                .unwrap();
        }

        // Redact the callbackUrl and subscriptionId because it generates a subscription uuid
        if let Some(subscription_ext) = body
            .extensions
            .get("subscription")
            .filter(|extension| extension.kind() == JsonKind::Object)
        {
            let mut subscription_ext = subscription_ext.detach().edit();

            let callback_url = subscription_ext.value().get("callbackUrl").map(|url| {
                url.as_str()
                    .expect("callbackUrl extension must be a string")
                    .into_owned()
            });
            if let Some(callback_url) = callback_url {
                let mut cb_url =
                    url::Url::parse(&callback_url).expect("callbackUrl must be a valid URL");
                cb_url.path_segments_mut().unwrap().pop();
                cb_url.path_segments_mut().unwrap().push("subscription_id");

                subscription_ext
                    .set("callbackUrl", cb_url.to_string())
                    .expect("the subscription extension is an object");
            }
            if subscription_ext.value().get("subscriptionId").is_some() {
                subscription_ext
                    .set("subscriptionId", "subscriptionId")
                    .expect("the subscription extension is an object");
            }

            body.extensions
                .object_insert("subscription", subscription_ext.seal().root_handle());
        }

        normalize(body);
        let response = if let Some(response) = self.mocks.get(body) {
            // Build an http Response
            let mut http_response_builder = http::Response::builder().status(StatusCode::OK);
            if let Some(headers) = http_response_builder.headers_mut() {
                headers.extend(self.headers.iter().map(|(k, v)| (k.clone(), v.clone())));
            }
            let http_response = http_response_builder
                .body(response.clone())
                .expect("Response is serializable; qed");
            SubgraphResponse::new_from_response(
                http_response,
                req.context,
                "test".to_string(),
                req.id,
            )
        } else {
            let error = crate::error::Error::builder()
                .message(format!(
                    "couldn't find mock for query {}",
                    serde_json::to_string(body).unwrap()
                ))
                .extension_code("FETCH_ERROR".to_string())
                .extensions(
                    self.extensions
                        .clone()
                        .unwrap_or_else(|| DocumentBuilder::new().seal().root_handle()),
                )
                .build();
            SubgraphResponse::fake_builder()
                .error(error)
                .context(req.context)
                .subgraph_name(req.subgraph_name.clone())
                .id(req.id)
                .build()
        };
        future::ok(response)
    }
}
