//! axum factory is useful to create an [`AxumHttpServerFactory`] which implements [`crate::http_server_factory::HttpServerFactory`]
mod axum_http_server_factory;
mod compression;
mod listeners;
#[cfg(test)]
pub(crate) mod tests;
pub(crate) mod utils;

#[cfg(feature = "apollo_unsupported")]
pub use axum_http_server_factory::set_add_main_endpoint_layer;
pub(crate) use axum_http_server_factory::span_mode;
pub(crate) use axum_http_server_factory::AxumHttpServerFactory;
#[cfg(feature = "apollo_unsupported")]
pub use listeners::set_add_extra_endpoints_layer;
pub(crate) use listeners::ListenAddrAndRouter;
