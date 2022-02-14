use crate::prelude::graphql::*;
use crate::services::http_compat;
use futures::Future;
use std::sync::Arc;
use tokio::sync::{RwLock, RwLockReadGuard, RwLockWriteGuard};

#[derive(Clone, Debug)]
pub struct Context<T = Arc<http_compat::Request<Request>>> {
    /// Original request to the Router.
    pub request: T,

    // Allows adding custom extensions to the context.
    extensions: Arc<RwLock<Object>>,
}

impl Context<()> {
    pub fn new() -> Self {
        Context {
            request: (),
            extensions: Default::default(),
        }
    }

    pub fn with_request<T>(self, request: T) -> Context<T> {
        // TODO this could be improved with this RFC https://github.com/rust-lang/rust/issues/86555
        let Self {
            request: _,
            extensions,
        } = self;
        Context {
            request,
            extensions,
        }
    }
}

impl From<Context<http_compat::Request<Request>>> for Context<Arc<http_compat::Request<Request>>> {
    fn from(ctx: Context<http_compat::Request<Request>>) -> Self {
        Self {
            request: Arc::new(ctx.request),
            extensions: ctx.extensions,
        }
    }
}

impl<T> Context<T> {
    pub fn extensions(&self) -> impl Future<Output = RwLockReadGuard<Object>> {
        self.extensions.read()
    }

    pub fn extensions_mut(&self) -> impl Future<Output = RwLockWriteGuard<Object>> {
        self.extensions.write()
    }
}

impl Default for Context<()> {
    fn default() -> Self {
        Self::new()
    }
}
