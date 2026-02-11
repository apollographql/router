//! HTTP-layer service types for the `http_service` plugin hook.
//!
//! # Why the HTTP layer exists
//!
//! The server receives a raw HTTP request (method, URI, headers, body). The rest of the
//! pipeline ([`router::Request`](crate::services::router::Request), [`router::Response`](crate::services::router::Response), etc.)
//! operate on GraphQL-oriented types (context, streamable body). The HTTP layer is the boundary where:
//!
//! 1. **Conversion:** The inner handler turns a raw HTTP request into [`router::Request`](crate::services::router::Request),
//!    calls the router pipeline, and turns the response back into an HTTP response.
//! 2. **Plugin hook:** The `http_service` hook lets plugins (and coprocessors) run at this
//!    boundary—seeing and mutating raw HTTP (method, URI, headers, body as [`Bytes`])—instead
//!    of only at router/supergraph level. Use cases include custom auth from headers, body
//!    inspection or modification, and response header/status changes.
//! 3. **Single body read:** The request body is read once into [`Bytes`] before the plugin
//!    stack, so every plugin can read or replace it without consuming a stream the router
//!    still needs.
//!
//! # Body read/mutate contract
//!
//! - **Single read:** The incoming request body is read once into [`Bytes`] before
//!   the plugin stack. No double consumption.
//! - **Request:** Plugins receive [`HttpRequest`] (method, URI, headers, body as [`Bytes`]).
//!   They can return a modified request (e.g. different headers or body) to the next layer.
//! - **Response:** Plugins can receive and mutate the response (status, headers, body as [`Bytes`])
//!   before it is converted back to the wire format.
//! - The inner handler builds [`router::Request`](crate::services::router::Request) from the (possibly plugin-mutated) bytes and
//!   calls the router pipeline; the router response is then converted to [`HttpResponse`]
//!   (body as [`Bytes`]) and passed back through the plugin stack.

use bytes::Bytes;
use http::Request;
use http::Response;
use tower::BoxError;
use tower::Service;

/// Request type for the HTTP-layer plugin hook.
///
/// The body has already been read once into [`Bytes`], so plugins can read and
/// mutate it without further consumption. Build a new `HttpRequest` to
/// pass a modified body (or headers, method, uri) to the next layer.
pub type HttpRequest = Request<Bytes>;

/// Response type for the HTTP-layer plugin hook.
///
/// The body is represented as [`Bytes`] so plugins can read and mutate the
/// response before it is converted back to a streaming body for the client.
pub type HttpResponse = Response<Bytes>;

/// Boxed service type for the HTTP layer.
///
/// Plugins implement `http_service` by wrapping this service. The inner service
/// (closest to the router) converts `HttpRequest` to `router::Request`,
/// calls the router, and converts the response to `HttpResponse`.
pub type BoxService = tower::util::BoxService<HttpRequest, HttpResponse, BoxError>;

/// Extension trait to allow cloning the service type for use in layers.
#[allow(dead_code)]
pub(crate) trait HttpLayerService:
    Service<HttpRequest, Response = HttpResponse, Error = BoxError> + Send + 'static
{
}

impl<T> HttpLayerService for T
where
    T: Service<HttpRequest, Response = HttpResponse, Error = BoxError> + Send + 'static,
    T::Future: Send,
{
}
