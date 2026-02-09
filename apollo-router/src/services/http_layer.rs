//! HTTP-layer service types for the `http_service` plugin hook.
//!
//! This module defines the request/response types and service type used by the
//! raw HTTP hook that runs in front of the router (after license_handler).
//!
//! # Body read/mutate contract
//!
//! - **Single read:** The incoming request body is read once into [`Bytes`] before
//!   the plugin stack. No double consumption.
//! - **Request:** Plugins receive [`HttpLayerRequest`] (method, URI, headers, body as [`Bytes`]).
//!   They can return a modified request (e.g. different headers or body) to the next layer.
//! - **Response:** Plugins can receive and mutate the response (status, headers, body as [`Bytes`])
//!   before it is converted back to the wire format.
//! - The inner handler builds `router::Request` from the (possibly plugin-mutated) bytes and
//!   calls the router pipeline; the router response is then converted to [`HttpLayerResponse`]
//!   (body as [`Bytes`]) and passed back through the plugin stack.

use bytes::Bytes;
use http::Request;
use http::Response;
use tower::BoxError;
use tower::Service;

/// Request type for the HTTP-layer plugin hook.
///
/// The body has already been read once into [`Bytes`], so plugins can read and
/// mutate it without further consumption. Build a new `HttpLayerRequest` to
/// pass a modified body (or headers, method, uri) to the next layer.
pub(crate) type HttpLayerRequest = Request<Bytes>;

/// Response type for the HTTP-layer plugin hook.
///
/// The body is represented as [`Bytes`] so plugins can read and mutate the
/// response before it is converted back to a streaming body for the client.
pub(crate) type HttpLayerResponse = Response<Bytes>;

/// Boxed service type for the HTTP layer.
///
/// Plugins implement `http_service` by wrapping this service. The inner service
/// (closest to the router) converts `HttpLayerRequest` to `router::Request`,
/// calls the router, and converts the response to `HttpLayerResponse`.
pub(crate) type BoxService = tower::util::BoxService<
    HttpLayerRequest,
    HttpLayerResponse,
    BoxError,
>;

/// Extension trait to allow cloning the service type for use in layers.
#[allow(dead_code)]
pub(crate) trait HttpLayerService:
    Service<HttpLayerRequest, Response = HttpLayerResponse, Error = BoxError> + Send + 'static
{
}

impl<T> HttpLayerService for T
where
    T: Service<HttpLayerRequest, Response = HttpLayerResponse, Error = BoxError> + Send + 'static,
    T::Future: Send,
{
}
