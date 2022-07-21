//! Reusable layers
//! Layers that are specific to one plugin should not be placed in this module.
use std::future::Future;
use std::ops::ControlFlow;
use std::sync::Arc;

use moka::sync::Cache;
use tokio::sync::RwLock;
use tower::buffer::BufferLayer;
use tower::layer::util::Stack;
use tower::BoxError;
use tower::ServiceBuilder;
use tower_service::Service;
use tracing::Span;

use crate::layers::async_checkpoint::AsyncCheckpointLayer;
use crate::layers::cache::CachingLayer;
use crate::layers::instrument::InstrumentLayer;
use crate::layers::map_future_with_context::MapFutureWithContextLayer;
use crate::layers::map_future_with_context::MapFutureWithContextService;
use crate::layers::sync_checkpoint::CheckpointLayer;

pub mod map_future_with_context;

pub mod async_checkpoint;
pub mod cache;
pub mod instrument;
pub mod sync_checkpoint;

pub(crate) const DEFAULT_BUFFER_SIZE: usize = 20_000;

/// Extension to the [`ServiceBuilder`] trait to make it easy to add router specific capabilities
/// (e.g.: checkpoints) to a [`Service`].
#[allow(clippy::type_complexity)]
pub trait ServiceBuilderExt<L>: Sized {
    /// Add a caching layer to the service stack.
    /// Given a request and response extract a cacheable key and value that may be used later to return a cached result.
    ///
    /// # Arguments
    ///
    /// * `cache`: The Moka cache that backs this layer.
    /// * `key_fn`: The callback to extract a key from the request.
    /// * `value_fn`: The callback to extract a value from the response.
    /// * `response_fn`: The callback to construct a response given a request and a cached value.
    ///
    /// returns: ServiceBuilder<Stack<CachingLayer<Request, Response, Key, Value>, L>>
    ///
    /// # Examples
    ///
    /// ```rust
    /// # use std::time::Duration;
    /// # use moka::sync::Cache;
    /// # use tower::ServiceBuilder;
    /// # use tower_service::Service;
    /// # use tracing::info_span;
    /// # use apollo_router::graphql::Response;
    /// # use apollo_router::services::{RouterRequest, RouterResponse};
    /// # use apollo_router::layers::ServiceBuilderExt;
    /// # fn test<S: Service<RouterRequest> + Send>(service: S) {
    /// //TODO This doc has highlighted a couple of issues that need to be resolved
    /// //let _ = ServiceBuilder::new()
    /// //            .cache(Cache::builder().time_to_live(Duration::from_secs(1)).max_capacity(100).build(),
    /// //                |req: &RouterRequest| req.originating_request.headers().get("cache_key"),
    /// //                |resp: &RouterResponse| &resp.response.body(),
    /// //                |req: RouterRequest, cached: &ResponseBody| RouterResponse::builder()
    /// //                    .context(req.context)
    /// //                    .data(cached.clone()) //TODO builder should take ResponseBody
    /// //                    .build().unwrap()) //TODO make response function fallible
    /// //            .service(service);
    /// # }
    /// ```
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

    /// Decide if processing should continue or not, and if not allow returning of a response.
    ///
    /// This is useful for validation functionality where you want to abort processing but return a
    /// valid response.
    ///
    /// # Arguments
    ///
    /// * `checkpoint_fn`: Ths callback to decides if processing should continue or not.
    ///
    /// returns: ServiceBuilder<Stack<CheckpointLayer<S, Request>, L>>
    ///
    /// # Examples
    ///
    /// ```rust
    /// # use std::ops::ControlFlow;
    /// # use http::Method;
    /// # use tower::ServiceBuilder;
    /// # use tower_service::Service;
    /// # use tracing::info_span;
    /// # use apollo_router::services::{RouterRequest, RouterResponse};
    /// # use apollo_router::layers::ServiceBuilderExt;
    /// # fn test<S: Service<RouterRequest, Response = Result<RouterResponse, Box<dyn std::error::Error + Send + Sync>>> + 'static + Send>(service: S) where <S as Service<RouterRequest>>::Future: Send, <S as Service<RouterRequest>>::Error: Send + Sync + std::error::Error, <S as Service<RouterRequest>>::Response: Send {
    /// let _ = ServiceBuilder::new()
    ///             .checkpoint(|req:RouterRequest|{
    ///                if req.originating_request.method() == Method::GET {
    ///                  Ok(ControlFlow::Break(RouterResponse::builder()
    ///                      .data("Only get requests allowed")
    ///                      .context(req.context).build())
    ///                    )
    ///                }
    ///                else {
    ///                  Ok(ControlFlow::Continue(req))
    ///                }
    ///             })
    ///             .service(service);
    /// # }
    /// ```
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

    /// Decide if processing should continue or not, and if not allow returning of a response.
    /// Unlike checkpoint it is possible to perform async operations in the callback. However
    /// this requires that the service is `Clone`. This can be achieved using `.buffered()`.
    ///
    /// This is useful for things like authentication where you need to make an external call to
    /// check if a request should proceed or not.
    ///
    /// # Arguments
    ///
    /// * `async_checkpoint_fn`: The asynchronous callback to decide if processing should continue or not.
    ///
    /// returns: ServiceBuilder<Stack<AsyncCheckpointLayer<S, Request>, L>>
    ///
    /// # Examples
    ///
    /// ```rust
    /// # use std::ops::ControlFlow;
    /// use futures::FutureExt;
    /// # use http::Method;
    /// # use tower::ServiceBuilder;
    /// # use tower_service::Service;
    /// # use tracing::info_span;
    /// # use apollo_router::services::{RouterRequest, RouterResponse};
    /// # use apollo_router::layers::ServiceBuilderExt;
    /// # fn test<S: Service<RouterRequest, Response = Result<RouterResponse, Box<dyn std::error::Error + Send + Sync>>> + 'static + Send>(service: S) where <S as Service<RouterRequest>>::Future: Send, <S as Service<RouterRequest>>::Error: Send + Sync + std::error::Error, <S as Service<RouterRequest>>::Response: Send {
    /// let _ = ServiceBuilder::new()
    ///             .checkpoint_async(|req:RouterRequest| async {
    ///                if req.originating_request.method() == Method::GET {
    ///                  Ok(ControlFlow::Break(RouterResponse::builder()
    ///                      .data("Only get requests allowed")
    ///                      .context(req.context).build())
    ///                    )
    ///                }
    ///                else {
    ///                  Ok(ControlFlow::Continue(req))
    ///                }
    ///             }.boxed())
    ///             .buffered()
    ///             .service(service);
    /// # }
    /// ```
    fn checkpoint_async<F, S, Fut, Request>(
        self,
        async_checkpoint_fn: F,
    ) -> ServiceBuilder<Stack<AsyncCheckpointLayer<S, Fut, Request>, L>>
    where
        S: Service<Request, Error = BoxError> + Clone + Send + 'static,
        Fut: Future<
            Output = Result<ControlFlow<<S as Service<Request>>::Response, Request>, BoxError>,
        >,
        F: Fn(Request) -> Fut + Send + Sync + 'static,
    {
        self.layer(AsyncCheckpointLayer::new(async_checkpoint_fn))
    }

    /// Adds a buffer to the service stack with a default size.
    ///
    /// This is useful for making services `Clone` and `Send`
    ///
    /// # Examples
    ///
    /// ```rust
    /// # use tower::ServiceBuilder;
    /// # use tower_service::Service;
    /// # use tracing::info_span;
    /// # use apollo_router::services::RouterRequest;
    /// # use apollo_router::layers::ServiceBuilderExt;
    /// # fn test<S: Service<RouterRequest> + 'static + Send>(service: S) where <S as Service<RouterRequest>>::Future: Send, <S as Service<RouterRequest>>::Error: Send + Sync + std::error::Error, <S as Service<RouterRequest>>::Response: Send {
    /// let _ = ServiceBuilder::new()
    ///             .buffered()
    ///             .service(service);
    /// # }
    /// ```
    fn buffered<Request>(self) -> ServiceBuilder<Stack<BufferLayer<Request>, L>>;

    /// Place a span around the request.
    ///
    /// This is useful for adding a new span with custom attributes to tracing.
    ///
    /// Note that it is not possible to add extra attributes to existing spans. However, you can add
    /// empty placeholder attributes to your span if you want to supply those attributes later.
    ///
    /// # Arguments
    ///
    /// * `span_fn`: The callback to create the span given the request.
    ///
    /// returns: ServiceBuilder<Stack<InstrumentLayer<F, Request>, L>>
    ///
    /// # Examples
    ///
    /// ```rust
    /// # use tower::ServiceBuilder;
    /// # use tower_service::Service;
    /// # use tracing::info_span;
    /// # use apollo_router::services::RouterRequest;
    /// # use apollo_router::layers::ServiceBuilderExt;
    /// # fn test<S: Service<RouterRequest> + Send>(service: S) {
    /// let instrumented = ServiceBuilder::new()
    ///             .instrument(|_request| info_span!("query_planning"))
    ///             .service(service);
    /// # }
    /// ```
    fn instrument<F, Request>(
        self,
        span_fn: F,
    ) -> ServiceBuilder<Stack<InstrumentLayer<F, Request>, L>>
    where
        F: Fn(&Request) -> Span,
    {
        self.layer(InstrumentLayer::new(span_fn))
    }

    /// Similar to map_future but also providing an opportunity to extract information out of the
    /// request for use when constructing the response.
    ///
    /// # Arguments
    ///
    /// * `ctx_fn`: The callback to extract a context from the request.
    /// * `map_fn`: The callback to map the future.
    ///
    /// returns: ServiceBuilder<Stack<MapFutureWithContextLayer<C, F>, L>>
    ///
    /// # Examples
    ///
    /// ```rust
    /// # use std::future::Future;
    /// # use tower::{BoxError, ServiceBuilder, ServiceExt};
    /// # use tower::util::BoxService;
    /// # use tower_service::Service;
    /// # use tracing::info_span;
    /// # use apollo_router::Context;
    /// # use apollo_router::services::{RouterRequest, RouterResponse};
    /// # use apollo_router::layers::ServiceBuilderExt;
    /// # fn test<S: Service<RouterRequest, Response = Result<RouterResponse, BoxError>> + 'static + Send>(service: S) where <S as Service<RouterRequest>>::Future: Send, <S as Service<RouterRequest>>::Error: Send + Sync + std::error::Error, <S as Service<RouterRequest>>::Response: Send {
    /// let _ : BoxService<RouterRequest, S::Response, S::Error> = ServiceBuilder::new()
    ///             .map_future_with_context(|req: &RouterRequest| req.context.clone(), |ctx : Context, fut| async {
    ///                 fut.await
    ///              })
    ///             .service(service)
    ///             .boxed();
    /// # }
    /// ```
    fn map_future_with_context<C, F>(
        self,
        ctx_fn: C,
        map_fn: F,
    ) -> ServiceBuilder<Stack<MapFutureWithContextLayer<C, F>, L>> {
        self.layer(MapFutureWithContextLayer::new(ctx_fn, map_fn))
    }

    /// Utility function to allow us to specify default methods on this trait rather than duplicating in the impl.
    ///
    /// # Arguments
    ///
    /// * `layer`: The layer to add to the service stack.
    ///
    /// returns: ServiceBuilder<Stack<T, L>>
    ///
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

pub trait ServiceExt<Request>: Service<Request> {
    /// Similar to map_future but also providing an opportunity to extract information out of the
    /// request for use when constructing the response.
    ///
    /// # Arguments
    ///
    /// * `ctx_fn`: The callback to extract a context from the request.
    /// * `map_fn`: The callback to map the future.
    ///
    /// returns: ServiceBuilder<Stack<MapFutureWithContextLayer<C, F>, L>>
    ///
    /// # Examples
    ///
    /// ```rust
    /// # use std::future::Future;
    /// # use tower::{BoxError, ServiceBuilder, ServiceExt};
    /// # use tower::util::BoxService;
    /// # use tower_service::Service;
    /// # use tracing::info_span;
    /// # use apollo_router::Context;
    /// # use apollo_router::services::{RouterRequest, RouterResponse};
    /// # use apollo_router::layers::ServiceBuilderExt;
    /// # use apollo_router::layers::ServiceExt as ApolloServiceExt;
    /// # fn test<S: Service<RouterRequest, Response = Result<RouterResponse, BoxError>> + 'static + Send>(service: S) where <S as Service<RouterRequest>>::Future: Send, <S as Service<RouterRequest>>::Error: Send + Sync + std::error::Error, <S as Service<RouterRequest>>::Response: Send {
    /// let _ : BoxService<RouterRequest, S::Response, S::Error> = service
    ///             .map_future_with_context(|req: &RouterRequest| req.context.clone(), |ctx : Context, fut| async {
    ///                 fut.await
    ///              })
    ///             .boxed();
    /// # }
    /// ```
    fn map_future_with_context<C, F>(
        self,
        cxt_fn: C,
        map_fn: F,
    ) -> MapFutureWithContextService<Self, C, F>
    where
        Self: Sized,
        C: Clone,
        F: Clone,
    {
        MapFutureWithContextService::new(self, cxt_fn, map_fn)
    }
}
impl<T: ?Sized, Request> ServiceExt<Request> for T where T: Service<Request> {}
