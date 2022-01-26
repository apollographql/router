use crate::prelude::graphql::*;
use futures::prelude::*;
use std::sync::Arc;
use tokio::sync::{RwLock, RwLockReadGuard, RwLockWriteGuard};

#[derive(Clone)]
pub struct Context<T = Arc<http::Request<Request>>> {
    /// Original request to the Router.
    pub request: T,

    // Allows adding custom extensions to the context.
    extensions: Arc<RwLock<Extensions>>,
}

impl Context<()> {
    pub(crate) fn new() -> Self {
        Context {
            request: (),
            extensions: Default::default(),
        }
    }

    pub(crate) fn with_request<T>(self, request: T) -> Context<T> {
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

impl<T> Context<T> {
    pub fn extensions(&self) -> impl Future<Output = RwLockReadGuard<Extensions>> {
        self.extensions.read()
    }

    pub fn extensions_mut(&self) -> impl Future<Output = RwLockWriteGuard<Extensions>> {
        self.extensions.write()
    }
}
