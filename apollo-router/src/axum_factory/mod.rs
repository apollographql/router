//! axum factory is useful to create an [`AxumHttpServerFactory`] which implements [`crate::http_server_factory::HttpServerFactory`]
mod axum_http_server_factory;
mod compression;
mod listeners;
#[cfg(test)]
pub(crate) mod tests;
pub(crate) mod utils;

pub(crate) use axum_http_server_factory::AxumHttpServerFactory;
pub(crate) use listeners::ListenAddrAndRouter;
