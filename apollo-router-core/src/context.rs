use crate::prelude::graphql::*;
use crate::services::http_compat;
use futures::Future;
use serde_json_bytes::ByteString;
use std::hash::Hash;
use std::{borrow::Borrow, sync::Arc};
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
    /// Take a read lock of extensions
    /// Be careful when using this method, you could create deadlock if you use `lock_extensions_mut` in the same scope
    pub fn lock_extensions(&self) -> impl Future<Output = RwLockReadGuard<Object>> {
        self.extensions.read()
    }

    /// Take a write lock of extensions
    /// Be careful when using this method, you could create deadlock if you use `lock_extensions` in the same scope
    pub fn lock_extensions_mut(&self) -> impl Future<Output = RwLockWriteGuard<Object>> {
        self.extensions.write()
    }

    pub async fn cloned_extensions(&self) -> Object {
        self.extensions.read().await.clone()
    }

    pub async fn extensions_len(&self) -> usize {
        self.extensions.read().await.len()
    }

    pub async fn is_extensions_empty(&self) -> bool {
        self.extensions.read().await.is_empty()
    }

    pub async fn clear(&self) {
        self.extensions.write().await.clear()
    }

    /// Inserts a key-value pair into extensions.
    /// If extensions did not have this key present, None is returned.
    /// If extensions did have this key present, the value is updated, and the old value is returned.
    pub async fn insert_extension(&self, k: String, v: Value) -> Option<Value> {
        self.extensions.write().await.insert(k, v)
    }

    /// Removes a key-value pair from extensions.
    /// If extensions did not have this key present, None is returned.
    /// If extensions did have this key present, the value is updated, and the old value is returned.
    pub async fn remove_extension<Q>(&self, k: &Q) -> Option<Value>
    where
        ByteString: Borrow<Q>,
        Q: ?Sized + Ord + Eq + Hash,
    {
        self.extensions.write().await.remove(k)
    }

    pub async fn get_extension<Q>(&self, k: &Q) -> Option<Value>
    where
        ByteString: Borrow<Q>,
        Q: ?Sized + Ord + Eq + Hash,
    {
        self.extensions.read().await.get(k).cloned()
    }
}

impl Default for Context<()> {
    fn default() -> Self {
        Self::new()
    }
}
