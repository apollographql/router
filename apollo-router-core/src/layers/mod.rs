//! Reusable layers
//! Layers that are specific to one plugin should not be placed in this module.
use crate::async_checkpoint::AsyncCheckpointLayer;
use crate::instrument::InstrumentLayer;
use crate::layers::cache::CachingLayer;
use crate::sync_checkpoint::CheckpointLayer;
use futures::future::BoxFuture;
use moka::sync::Cache;
use std::ops::ControlFlow;
use std::sync::Arc;
use tokio::sync::RwLock;
use tower::buffer::BufferLayer;
use tower::layer::util::Stack;
use tower::{BoxError, ServiceBuilder};
use tower_service::Service;
use tracing::Span;

pub mod async_checkpoint;
pub mod cache;
pub mod instrument;
pub mod sync_checkpoint;

pub const DEFAULT_BUFFER_SIZE: usize = 20_000;

/// Extension to the [`ServiceBuilder`] trait to make it easy to add router specific capabilities
/// (e.g.: checkpoints) to a [`Service`].
#[allow(clippy::type_complexity)]
pub trait ServiceBuilderExt<L>: Sized {
    fn cache<Request, Response, Key, Value>(
        self,
        cache: Cache<Key, Arc<RwLock<Option<Result<Value, String>>>>>,
        key_fn: fn(&Request) -> Option<&Key>,
        value_fn: fn(&Response) -> &Value,
        response_fn: fn(Request, Value) -> Response,
    ) -> ServiceBuilder<Stack<CachingLayer<Request, Response, Key, Value>, L>>
    where
        Request: Send,
    {
        self.layer(CachingLayer::new(cache, key_fn, value_fn, response_fn))
    }

    fn checkpoint<S, Request>(
        self,
        checkpoint_fn: impl Fn(
                Request,
            ) -> Result<
                ControlFlow<<S as Service<Request>>::Response, Request>,
                <S as Service<Request>>::Error,
            > + Send
            + Sync
            + 'static,
    ) -> ServiceBuilder<Stack<CheckpointLayer<S, Request>, L>>
    where
        S: Service<Request> + Send + 'static,
        Request: Send + 'static,
        S::Future: Send,
        S::Response: Send + 'static,
        S::Error: Into<BoxError> + Send + 'static,
    {
        self.layer(CheckpointLayer::new(checkpoint_fn))
    }

    fn async_checkpoint<S, Request>(
        self,
        async_checkpoint_fn: impl Fn(
                Request,
            ) -> BoxFuture<
                'static,
                Result<ControlFlow<<S as Service<Request>>::Response, Request>, BoxError>,
            > + Send
            + Sync
            + 'static,
    ) -> ServiceBuilder<Stack<AsyncCheckpointLayer<S, Request>, L>>
    where
        S: Service<Request, Error = BoxError> + Clone + Send + 'static,
        Request: Send + 'static,
        S::Future: Send,
        S::Response: Send + 'static,
    {
        self.layer(AsyncCheckpointLayer::new(async_checkpoint_fn))
    }
    fn buffered<Request>(self) -> ServiceBuilder<Stack<BufferLayer<Request>, L>>;
    fn instrument<F, Request>(
        self,
        span_fn: F,
    ) -> ServiceBuilder<Stack<InstrumentLayer<F, Request>, L>>
    where
        F: Fn(&Request) -> Span,
    {
        self.layer(InstrumentLayer::new(span_fn))
    }
    fn layer<T>(self, layer: T) -> ServiceBuilder<Stack<T, L>>;
}

#[allow(clippy::type_complexity)]
impl<L> ServiceBuilderExt<L> for ServiceBuilder<L> {
    fn layer<T>(self, layer: T) -> ServiceBuilder<Stack<T, L>> {
        ServiceBuilder::layer(self, layer)
    }

    fn buffered<Request>(self) -> ServiceBuilder<Stack<BufferLayer<Request>, L>> {
        self.buffer(DEFAULT_BUFFER_SIZE)
    }
}
