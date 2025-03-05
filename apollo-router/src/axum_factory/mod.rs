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
pub(crate) use axum_http_server_factory::AxumHttpServerFactory;
pub(crate) use axum_http_server_factory::CanceledRequest;
pub(crate) use axum_http_server_factory::span_mode;
pub(crate) use listeners::ListenAddrAndRouter;

static ENDPOINT_CALLBACK: OnceLock<Arc<dyn Fn(Router) -> Router + Send + Sync>> = OnceLock::new();
