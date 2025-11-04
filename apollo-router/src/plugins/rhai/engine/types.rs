use std::sync::Arc;

use bytes::Bytes;
use parking_lot::Mutex;

use crate::context::Context;
use crate::graphql::Response;
use crate::http_ext;

/// Helper trait for safely working with Option-wrapped values in shared mutable state.
pub(crate) trait OptionDance<T> {
    fn with_mut<R>(&self, f: impl FnOnce(&mut T) -> R) -> R;

    fn replace(&self, f: impl FnOnce(T) -> T);

    fn take_unwrap(self) -> T;
}

/// Shared mutable state wrapped in Option for safe access from Rhai scripts.
pub(crate) type SharedMut<T> = rhai::Shared<Mutex<Option<T>>>;

impl<T> OptionDance<T> for SharedMut<T> {
    fn with_mut<R>(&self, f: impl FnOnce(&mut T) -> R) -> R {
        let mut guard = self.lock();
        f(guard.as_mut().expect("re-entrant option dance"))
    }

    fn replace(&self, f: impl FnOnce(T) -> T) {
        let mut guard = self.lock();
        *guard = Some(f(guard.take().expect("re-entrant option dance")))
    }

    fn take_unwrap(self) -> T {
        match Arc::try_unwrap(self) {
            Ok(mutex) => mutex.into_inner(),
            // TODO: Should we assume the Arc refcount is 1
            // and use `try_unwrap().expect("shared ownership")` instead of this fallback ?
            Err(arc) => arc.lock().take(),
        }
        .expect("re-entrant option dance")
    }
}

/// Router stage first request wrapper for Rhai.
#[derive(Default)]
pub(crate) struct RhaiRouterFirstRequest {
    pub(crate) context: Context,
    pub(crate) request: http::Request<()>,
}

/// Router stage chunked request wrapper for Rhai.
#[allow(dead_code)]
#[derive(Default)]
pub(crate) struct RhaiRouterChunkedRequest {
    pub(crate) context: Context,
    pub(crate) request: Bytes,
}

/// Router stage response wrapper for Rhai.
#[derive(Default)]
pub(crate) struct RhaiRouterResponse {
    pub(crate) context: Context,
    pub(crate) response: http::Response<()>,
}

/// Router stage chunked response wrapper for Rhai.
#[allow(dead_code)]
#[derive(Default)]
pub(crate) struct RhaiRouterChunkedResponse {
    pub(crate) context: Context,
    pub(crate) response: Bytes,
}

/// Supergraph stage response wrapper for Rhai.
#[derive(Default)]
pub(crate) struct RhaiSupergraphResponse {
    pub(crate) context: Context,
    pub(crate) response: http_ext::Response<Response>,
}

/// Supergraph stage deferred response wrapper for Rhai.
#[derive(Default)]
pub(crate) struct RhaiSupergraphDeferredResponse {
    pub(crate) context: Context,
    pub(crate) response: Response,
}

/// Execution stage response wrapper for Rhai.
#[derive(Default)]
pub(crate) struct RhaiExecutionResponse {
    pub(crate) context: Context,
    pub(crate) response: http_ext::Response<Response>,
}

/// Execution stage deferred response wrapper for Rhai.
#[derive(Default)]
pub(crate) struct RhaiExecutionDeferredResponse {
    pub(crate) context: Context,
    pub(crate) response: Response,
}
