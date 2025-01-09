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

use once_cell::sync::Lazy;
use regex::Regex;

static ENDPOINT_CALLBACK: OnceLock<Arc<dyn Fn(Router) -> Router + Send + Sync>> = OnceLock::new();
static NAMED_PARAMETER_REGEX: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"/(:[^/]+)").expect("valid regex"));
static NAMED_WILDCARD_REGEX: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"/(\*[^/]+)").expect("valid regex"));

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

/// Axum 0.8 upgraded its `matchit` dependency to 0.8.4, which included a breaking change requiring
/// parameters and wildcards to be wrapped in curly braces. So, we do a simple rewrite to allow
/// the previous syntax in the supergraph config.
/// Named param: /foo/:bar/baz => /foo/{:bar}/baz
/// Final wildcard: /foo/*wild => /foo/{*wild}
pub(crate) fn rewrite_path_for_axum_0_8(path: &str) -> String {
    let path = NAMED_PARAMETER_REGEX.replace_all(&path, "/{$1}");
    let path = NAMED_WILDCARD_REGEX.replace_all(&path, "/{$1}");
    path.to_string()
}
