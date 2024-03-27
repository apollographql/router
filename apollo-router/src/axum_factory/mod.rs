//! axum factory is useful to create an [`AxumHttpServerFactory`] which implements [`crate::http_server_factory::HttpServerFactory`]
mod axum_http_server_factory;
pub(crate) mod compression;
mod listeners;
#[cfg(test)]
pub(crate) mod tests;
pub(crate) mod utils;

use std::sync::Arc;
use std::sync::OnceLock;

use axum::Router;
pub(crate) use axum_http_server_factory::span_mode;
pub(crate) use axum_http_server_factory::AxumHttpServerFactory;
pub(crate) use axum_http_server_factory::CanceledRequest;
pub(crate) use listeners::ListenAddrAndRouter;

static ENDPOINT_CALLBACK: OnceLock<Arc<dyn Fn(Router) -> Router + Send + Sync>> = OnceLock::new();

/// Set a callback that may wrap or mutate `axum::Router` as they are added to the main router.
/// Although part of the public API, this is not intended for use by end users, and may change at any time.
#[doc(hidden)]
pub fn unsupported_set_axum_router_callback(
    callback: impl Fn(Router) -> Router + Send + Sync + 'static,
) -> axum::response::Result<(), &'static str> {
    ENDPOINT_CALLBACK
        .set(Arc::new(callback))
        .map_err(|_| "endpoint decorator was already set")
}
