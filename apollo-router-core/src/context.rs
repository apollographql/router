use crate::prelude::graphql::*;
use futures::prelude::*;
use std::sync::Arc;
use tokio::sync::{RwLock, RwLockReadGuard, RwLockWriteGuard};

#[derive(Clone)]
pub struct Context {
    /// Original request to the Router.
    pub request: Arc<http::Request<Request>>,

    // Allows adding custom extensions to the context.
    extensions: Arc<RwLock<Extensions>>,
}

impl Context {
    pub fn new(request: Arc<http::Request<Request>>) -> Self {
        Self {
            request,
            extensions: Default::default(),
        }
    }

    pub fn extensions(&self) -> impl Future<Output = RwLockReadGuard<Extensions>> {
        self.extensions.read()
    }

    pub fn extensions_mut(&self) -> impl Future<Output = RwLockWriteGuard<Extensions>> {
        self.extensions.write()
    }
}
