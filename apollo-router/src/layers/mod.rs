//! Reusable layers
//! Layers that are specific to one plugin should not be placed in this module.
use std::future::Future;
use std::ops::ControlFlow;

use tower::buffer::BufferLayer;
use tower::layer::util::Stack;
use tower::BoxError;
use tower::ServiceBuilder;
use tower_service::Service;
use tracing::Span;

use self::map_first_graphql_response::MapFirstGraphqlResponseLayer;
use self::map_first_graphql_response::MapFirstGraphqlResponseService;
use crate::graphql;
use crate::layers::async_checkpoint::AsyncCheckpointLayer;
use crate::layers::async_checkpoint::OneShotAsyncCheckpointLayer;
use crate::layers::instrument::InstrumentLayer;
use crate::layers::map_future_with_request_data::MapFutureWithRequestDataLayer;
use crate::layers::map_future_with_request_data::MapFutureWithRequestDataService;
use crate::layers::sync_checkpoint::CheckpointLayer;
use crate::services::supergraph;
use crate::Context;

pub mod async_checkpoint;
pub mod instrument;
pub mod map_first_graphql_response;
pub mod map_future_with_request_data;
pub mod sync_checkpoint;

pub(crate) const DEFAULT_BUFFER_SIZE: usize = 20_000;

/// Extension to the [`ServiceBuilder`] trait to make it easy to add router specific capabilities
/// (e.g.: checkpoints) to a [`Service`].
#[allow(clippy::type_complexity)]
pub trait ServiceBuilderExt<L>: Sized {
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
    /// # use apollo_router::services::supergraph;
    /// # use apollo_router::layers::ServiceBuilderExt;
    /// # fn test(service: supergraph::BoxService) {
    /// let _ = ServiceBuilder::new()
    ///     .checkpoint(|req: supergraph::Request|{
    ///         if req.supergraph_request.method() == Method::GET {
    ///             Ok(ControlFlow::Break(supergraph::Response::builder()
    ///                 .data("Only get requests allowed")
    ///                 .context(req.context)
    ///                 .build()?))
    ///         } else {
    ///             Ok(ControlFlow::Continue(req))
    ///         }
    ///     })
    ///     .service(service);
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
    /// # use apollo_router::services::supergraph;
    /// # use apollo_router::layers::ServiceBuilderExt;
    /// # fn test(service: supergraph::BoxService) {
    /// let _ = ServiceBuilder::new()
    ///     .checkpoint_async(|req: supergraph::Request|
    ///         async {
    ///             if req.supergraph_request.method() == Method::GET {
    ///                 Ok(ControlFlow::Break(supergraph::Response::builder()
    ///                     .data("Only get requests allowed")
    ///                     .context(req.context)
    ///                     .build()?))
    ///             } else {
    ///                 Ok(ControlFlow::Continue(req))
    ///             }
    ///         }
    ///         .boxed()
    ///     )
    ///     .buffered()
    ///     .service(service);
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

    /// Decide if processing should continue or not, and if not allow returning of a response.
    /// Unlike checkpoint it is possible to perform async operations in the callback. Unlike
    /// checkpoint_async, this does not require that the service is `Clone` and avoids the
    /// requiremnent to buffer services.
    ///
    /// This is useful for things like authentication where you need to make an external call to
    /// check if a request should proceed or not.
    ///
    /// # Arguments
    ///
    /// * `async_checkpoint_fn`: The asynchronous callback to decide if processing should continue or not.
    ///
    /// returns: ServiceBuilder<Stack<OneShotAsyncCheckpointLayer<S, Request>, L>>
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
    /// # use apollo_router::services::supergraph;
    /// # use apollo_router::layers::ServiceBuilderExt;
    /// # fn test(service: supergraph::BoxService) {
    /// let _ = ServiceBuilder::new()
    ///     .oneshot_checkpoint_async(|req: supergraph::Request|
    ///         async {
    ///             if req.supergraph_request.method() == Method::GET {
    ///                 Ok(ControlFlow::Break(supergraph::Response::builder()
    ///                     .data("Only get requests allowed")
    ///                     .context(req.context)
    ///                     .build()?))
    ///             } else {
    ///                 Ok(ControlFlow::Continue(req))
    ///             }
    ///         }
    ///         .boxed()
    ///     )
    ///     .service(service);
    /// # }
    /// ```
    fn oneshot_checkpoint_async<F, S, Fut, Request>(
        self,
        async_checkpoint_fn: F,
    ) -> ServiceBuilder<Stack<OneShotAsyncCheckpointLayer<S, Fut, Request>, L>>
    where
        S: Service<Request, Error = BoxError> + Send + 'static,
        Fut: Future<
            Output = Result<ControlFlow<<S as Service<Request>>::Response, Request>, BoxError>,
        >,
        F: Fn(Request) -> Fut + Send + Sync + 'static,
    {
        self.layer(OneShotAsyncCheckpointLayer::new(async_checkpoint_fn))
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
    /// # use apollo_router::services::supergraph;
    /// # use apollo_router::layers::ServiceBuilderExt;
    /// # fn test(service: supergraph::BoxService) {
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
    /// # use apollo_router::services::supergraph;
    /// # use apollo_router::layers::ServiceBuilderExt;
    /// # fn test(service: supergraph::BoxService) {
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

    /// Maps HTTP parts, as well as the first GraphQL response, to different values.
    ///
    /// In supergraph and execution services, the service response contains
    /// not just one GraphQL response but a stream of them,
    /// in order to support features such as `@defer`.
    ///
    /// This method wraps a service and calls a `callback` when the first GraphQL response
    /// in the stream returned by the inner service becomes available.
    /// The callback can then access the HTTP parts (headers, status code, etc)
    /// or the first GraphQL response before returning them.
    ///
    /// Note that any subsequent GraphQL responses after the first will be forwarded unmodified.
    /// In order to inspect or modify all GraphQL responses,
    /// consider using [`map_response`][tower::ServiceExt::map_response]
    /// together with [`supergraph::Response::map_stream`] instead.
    /// (See the example in `map_stream`’s documentation.)
    /// In that case however HTTP parts cannot be modified because they may have already been sent.
    ///
    /// # Example
    ///
    /// ```
    /// use apollo_router::services::supergraph;
    /// use apollo_router::layers::ServiceBuilderExt as _;
    /// use tower::ServiceExt as _;
    ///
    /// struct ExamplePlugin;
    ///
    /// #[async_trait::async_trait]
    /// impl apollo_router::plugin::Plugin for ExamplePlugin {
    ///     # type Config = ();
    ///     # async fn new(
    ///     #     _init: apollo_router::plugin::PluginInit<Self::Config>,
    ///     # ) -> Result<Self, tower::BoxError> {
    ///     #     Ok(Self)
    ///     # }
    ///     // …
    ///     fn supergraph_service(&self, inner: supergraph::BoxService) -> supergraph::BoxService {
    ///         tower::ServiceBuilder::new()
    ///             .map_first_graphql_response(|context, mut http_parts, mut graphql_response| {
    ///                 // Something interesting here
    ///                 (http_parts, graphql_response)
    ///             })
    ///             .service(inner)
    ///             .boxed()
    ///     }
    /// }
    /// ```
    fn map_first_graphql_response<Callback>(
        self,
        callback: Callback,
    ) -> ServiceBuilder<Stack<MapFirstGraphqlResponseLayer<Callback>, L>>
    where
        Callback: FnOnce(
                Context,
                http::response::Parts,
                graphql::Response,
            ) -> (http::response::Parts, graphql::Response)
            + Clone
            + Send
            + 'static,
    {
        self.layer(MapFirstGraphqlResponseLayer { callback })
    }

    /// Similar to map_future but also providing an opportunity to extract information out of the
    /// request for use when constructing the response.
    ///
    /// # Arguments
    ///
    /// * `req_fn`: The callback to extract data from the request.
    /// * `map_fn`: The callback to map the future.
    ///
    /// returns: ServiceBuilder<Stack<MapFutureWithRequestDataLayer<RF, MF>, L>>
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
    /// # use apollo_router::services::supergraph;
    /// # use apollo_router::layers::ServiceBuilderExt;
    /// # fn test(service: supergraph::BoxService) {
    /// let _ : supergraph::BoxService = ServiceBuilder::new()
    ///     .map_future_with_request_data(
    ///         |req: &supergraph::Request| req.context.clone(),
    ///         |ctx : Context, fut| async { fut.await })
    ///     .service(service)
    ///     .boxed();
    /// # }
    /// ```
    fn map_future_with_request_data<RF, MF>(
        self,
        req_fn: RF,
        map_fn: MF,
    ) -> ServiceBuilder<Stack<MapFutureWithRequestDataLayer<RF, MF>, L>> {
        self.layer(MapFutureWithRequestDataLayer::new(req_fn, map_fn))
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

/// Extension trait for [`Service`].
///
/// Importing both this trait and [`tower::ServiceExt`] could lead a name collision error.
/// To work around that, use `as _` syntax to make a trait’s methods available in a module
/// without assigning it a name in that module’s namespace.
///
/// ```
/// use apollo_router::layers::ServiceExt as _;
/// use tower::ServiceExt as _;
/// ```
pub trait ServiceExt<Request>: Service<Request> {
    /// Maps HTTP parts, as well as the first GraphQL response, to different values.
    ///
    /// In supergraph and execution services, the service response contains
    /// not just one GraphQL response but a stream of them,
    /// in order to support features such as `@defer`.
    ///
    /// This method wraps a service and call `callback` when the first GraphQL response
    /// in the stream returned by the inner service becomes available.
    /// The callback can then modify the HTTP parts (headers, status code, etc)
    /// or the first GraphQL response before returning them.
    ///
    /// Note that any subsequent GraphQL responses after the first will be forwarded unmodified.
    /// In order to inspect or modify all GraphQL responses,
    /// consider using [`map_response`][tower::ServiceExt::map_response]
    /// together with [`supergraph::Response::map_stream`] instead.
    /// (See the example in `map_stream`’s documentation.)
    /// In that case however HTTP parts cannot be modified because they may have already been sent.
    ///
    /// # Example
    ///
    /// ```
    /// use apollo_router::services::supergraph;
    /// use apollo_router::layers::ServiceExt as _;
    /// use tower::ServiceExt as _;
    ///
    /// struct ExamplePlugin;
    ///
    /// #[async_trait::async_trait]
    /// impl apollo_router::plugin::Plugin for ExamplePlugin {
    ///     # type Config = ();
    ///     # async fn new(
    ///     #     _init: apollo_router::plugin::PluginInit<Self::Config>,
    ///     # ) -> Result<Self, tower::BoxError> {
    ///     #     Ok(Self)
    ///     # }
    ///     // …
    ///     fn supergraph_service(&self, inner: supergraph::BoxService) -> supergraph::BoxService {
    ///         inner
    ///             .map_first_graphql_response(|context, mut http_parts, mut graphql_response| {
    ///                 // Something interesting here
    ///                 (http_parts, graphql_response)
    ///             })
    ///             .boxed()
    ///     }
    /// }
    /// ```
    fn map_first_graphql_response<Callback>(
        self,
        callback: Callback,
    ) -> MapFirstGraphqlResponseService<Self, Callback>
    where
        Self: Sized + Service<Request, Response = supergraph::Response>,
        <Self as Service<Request>>::Future: Send + 'static,
        Callback: FnOnce(
                Context,
                http::response::Parts,
                graphql::Response,
            ) -> (http::response::Parts, graphql::Response)
            + Clone
            + Send
            + 'static,
    {
        ServiceBuilder::new()
            .map_first_graphql_response(callback)
            .service(self)
    }

    /// Similar to map_future but also providing an opportunity to extract information out of the
    /// request for use when constructing the response.
    ///
    /// # Arguments
    ///
    /// * `req_fn`: The callback to extract data from the request.
    /// * `map_fn`: The callback to map the future.
    ///
    /// returns: ServiceBuilder<Stack<MapFutureWithRequestDataLayer<RF, MF>, L>>
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
    /// # use apollo_router::services::supergraph;
    /// # use apollo_router::layers::ServiceBuilderExt;
    /// # use apollo_router::layers::ServiceExt as ApolloServiceExt;
    /// # fn test(service: supergraph::BoxService) {
    /// let _ : supergraph::BoxService = service
    ///     .map_future_with_request_data(
    ///         |req: &supergraph::Request| req.context.clone(),
    ///         |ctx : Context, fut| async { fut.await }
    ///     )
    ///     .boxed();
    /// # }
    /// ```
    fn map_future_with_request_data<RF, MF>(
        self,
        req_fn: RF,
        map_fn: MF,
    ) -> MapFutureWithRequestDataService<Self, RF, MF>
    where
        Self: Sized,
        RF: Clone,
        MF: Clone,
    {
        MapFutureWithRequestDataService::new(self, req_fn, map_fn)
    }
}
impl<T: ?Sized, Request> ServiceExt<Request> for T where T: Service<Request> {}
