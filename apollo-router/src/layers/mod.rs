//! Reusable layers
//! Layers that are specific to one plugin should not be placed in this module.
use std::future::Future;
use std::ops::ControlFlow;
use std::sync::Arc;

use tower::BoxError;
use tower::ServiceBuilder;
use tower::layer::util::Stack;
use tower_service::Service;
use tracing::Span;

use self::map_first_graphql_response::MapFirstGraphqlResponseLayer;
use self::map_first_graphql_response::MapFirstGraphqlResponseService;
use crate::Context;
use crate::graphql;
use crate::layers::async_checkpoint::AsyncCheckpointLayer;
use crate::layers::instrument::InstrumentLayer;
use crate::layers::map_future_with_request_data::MapFutureWithRequestDataLayer;
use crate::layers::map_future_with_request_data::MapFutureWithRequestDataService;
use crate::layers::rust_plugins::RustPluginsLayer;
use crate::layers::unconstrained_buffer::UnconstrainedBufferLayer;
use crate::plugin::DynPlugin;
use crate::plugin::PluginPrivate;
use crate::services::Plugins;
use crate::services::supergraph;

pub mod async_checkpoint;
pub mod instrument;
pub mod map_first_graphql_response;
pub mod map_future_with_request_data;
pub(crate) mod rust_plugins;
pub mod unconstrained_buffer;

// Note: We use Buffer in many places throughout the router. 50_000 represents
// the "maximal number of requests that can be queued for the buffered
// service before backpressure is applied to callers". We set this to be
// so high, 50_000, because we anticipate that many users will want to
//
// Think of this as a backstop for when there are no other backpressure
// enforcing limits configured in a router. In future we may tweak this
// value higher or lower or expose it as a configurable.
pub(crate) const DEFAULT_BUFFER_SIZE: usize = 50_000;

/// Extension to the [`ServiceBuilder`] trait to make it easy to add router specific capabilities
/// (e.g.: checkpoints) to a [`Service`].
#[allow(clippy::type_complexity)]
pub trait ServiceBuilderExt<L>: Sized {
    /// Decide if processing should continue or not, and if not allow returning of a response.
    /// Unlike checkpoint it is possible to perform async operations in the callback. However
    /// the resulting service requires `S: Clone`. Since `BoxCloneService` is already `Clone`,
    /// a `.buffered()` call is no longer needed when wrapping a `BoxCloneService`.
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
    /// # fn test(service: supergraph::BoxCloneService) {
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

    /// Adds a buffer to the service stack with a default size.
    ///
    /// The buffer spawns a dedicated worker task and queues requests in an in-memory channel.
    /// The primary reasons to include a buffer are:
    ///
    /// - **Backpressure**: callers block (rather than failing immediately) when the inner
    ///   service is busy processing previous requests.
    /// - **`LoadShed` / `ConcurrencyLimit` / `RateLimit` interaction**: these layers
    ///   signal overload by returning `Poll::Pending` from `poll_ready`. A buffer placed
    ///   *before* them absorbs that pending state and prevents Tokio's cooperative-scheduling
    ///   budget from causing spurious `Overloaded` responses.
    ///
    /// Now that pipeline services are `BoxCloneService`, a buffer is **no longer needed
    /// merely to make a service `Clone` or `Send`**.
    ///
    /// # Examples
    ///
    /// ```rust
    /// # use tower::ServiceBuilder;
    /// # use tower_service::Service;
    /// # use tracing::info_span;
    /// # use apollo_router::services::supergraph;
    /// # use apollo_router::layers::ServiceBuilderExt;
    /// # fn test(service: supergraph::BoxCloneService) {
    /// let _ = ServiceBuilder::new()
    ///             .buffered()
    ///             .service(service);
    /// # }
    /// ```
    fn buffered<Request>(self) -> ServiceBuilder<Stack<UnconstrainedBufferLayer<Request>, L>>;

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
    /// # fn test(service: supergraph::BoxCloneService) {
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
    ///     fn supergraph_service(&self, inner: supergraph::BoxCloneService) -> supergraph::BoxCloneService {
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
    /// # use tower_service::Service;
    /// # use tracing::info_span;
    /// # use apollo_router::Context;
    /// # use apollo_router::services::supergraph;
    /// # use apollo_router::layers::ServiceBuilderExt;
    /// # fn test(service: supergraph::BoxCloneService) {
    /// let _ : supergraph::BoxCloneService = ServiceBuilder::new()
    ///     .map_future_with_request_data(
    ///         |req: &supergraph::Request| req.context.clone(),
    ///         |ctx : Context, fut| async { fut.await })
    ///     .service(service)
    ///     .boxed_clone();
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

    fn buffered<Request>(self) -> ServiceBuilder<Stack<UnconstrainedBufferLayer<Request>, L>> {
        self.layer(UnconstrainedBufferLayer::new(DEFAULT_BUFFER_SIZE))
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
    ///     fn supergraph_service(&self, inner: supergraph::BoxCloneService) -> supergraph::BoxCloneService {
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
    /// # use tower_service::Service;
    /// # use tracing::info_span;
    /// # use apollo_router::Context;
    /// # use apollo_router::services::supergraph;
    /// # use apollo_router::layers::ServiceBuilderExt;
    /// # use apollo_router::layers::ServiceExt as ApolloServiceExt;
    /// # fn test(service: supergraph::BoxCloneService) {
    /// let _ : supergraph::BoxCloneService = service
    ///     .map_future_with_request_data(
    ///         |req: &supergraph::Request| req.context.clone(),
    ///         |ctx : Context, fut| async { fut.await }
    ///     )
    ///     .boxed_clone();
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

/// Helper type to name layers produced by [`ServiceBuilder::option_layer()`].
type OptionLayer<L> = tower::util::Either<L, tower::layer::util::Identity>;

/// Extension to [`ServiceBuilder`] for pipeline utilities that are not exposed to crate consumers.
pub(crate) trait InternalServiceBuilderExt<L>: Sized {
    /// Apply plugins to a service stack.
    ///
    /// Provide the way of applying the plugin as a closure. The inner service must be a
    /// [`BoxCloneService`][tower::util::BoxCloneService] to work with plugin hooks.
    ///
    /// # Example
    /// ```rust,ignore
    /// ServiceBuilder::new()
    ///     .rust_plugins(plugins, |plugin, service| plugin.router_service(service))
    ///     .service(router_service.boxed_clone());
    /// ```
    fn rust_plugins<F, R, Resp, Err>(
        self,
        plugins: Arc<Plugins>,
        apply: F,
    ) -> ServiceBuilder<Stack<RustPluginsLayer<F, R>, L>>
    where
        F: Fn(
            &dyn DynPlugin,
            tower::util::BoxCloneService<R, Resp, Err>,
        ) -> tower::util::BoxCloneService<R, Resp, Err>;

    /// Apply a plugin layer to a service stack.
    ///
    /// Provide the plugin layer to apply as a method reference.
    ///
    /// If the plugin isn't available in the `plugins` registry, this function is a no-op.
    /// That covers both a plugin the configuration leaves out and one the license
    /// disables. Gating removes the plugin during construction, so placement sees one kind
    /// of absence.
    ///
    /// For a plugin that `create_plugins` always registers, use
    /// [`Self::apply_required_plugin_layer`] instead. It panics on a miss rather than
    /// silently dropping the layer.
    fn apply_plugin_layer<P, OutLayer>(
        self,
        plugins: &Plugins,
        get_layer: impl FnOnce(&P) -> OutLayer,
    ) -> ServiceBuilder<Stack<OptionLayer<OutLayer>, L>>
    where
        P: PluginPrivate;

    /// Apply the layer of a *mandatory* plugin to a service stack.
    ///
    /// Same as [`Self::apply_plugin_layer`], except that a missing plugin is a bug rather
    /// than a supported configuration. The miss panics instead of quietly reducing the
    /// stack to a no-op. Several of these layers are security-relevant: losing
    /// `IncludeSubgraphErrors::redact_subgraph_errors_layer` would send every subgraph
    /// error to clients unredacted, so a silent drop is the worst failure mode available.
    ///
    /// A wholly empty registry is left alone. Test fixtures legitimately build pipelines
    /// with no plugins at all.
    ///
    /// ## Example
    ///
    /// ```rust,ignore
    /// ServiceBuilder::new()
    ///     .apply_required_plugin_layer(&plugins, Headers::masking_rules_context_layer)
    ///     .service(router_service)
    /// ```
    fn apply_required_plugin_layer<P, OutLayer>(
        self,
        plugins: &Plugins,
        get_layer: impl FnOnce(&P) -> OutLayer,
    ) -> ServiceBuilder<Stack<OptionLayer<OutLayer>, L>>
    where
        P: PluginPrivate;
}

/// Find the single instance of plugin type `P` in the registry, if it was built.
fn find_plugin<P: PluginPrivate>(plugins: &Plugins) -> Option<&P> {
    plugins
        .values()
        .find_map(|plugin| plugin.as_any().downcast_ref::<P>())
}

impl<L> InternalServiceBuilderExt<L> for ServiceBuilder<L> {
    fn rust_plugins<F, R, Resp, Err>(
        self,
        plugins: Arc<Plugins>,
        apply: F,
    ) -> ServiceBuilder<Stack<RustPluginsLayer<F, R>, L>>
    where
        F: Fn(
            &dyn DynPlugin,
            tower::util::BoxCloneService<R, Resp, Err>,
        ) -> tower::util::BoxCloneService<R, Resp, Err>,
    {
        self.layer(RustPluginsLayer::new(plugins, apply))
    }

    fn apply_plugin_layer<P, OutLayer>(
        self,
        plugins: &Plugins,
        get_layer: impl FnOnce(&P) -> OutLayer,
    ) -> ServiceBuilder<Stack<OptionLayer<OutLayer>, L>>
    where
        P: PluginPrivate,
    {
        self.option_layer(find_plugin::<P>(plugins).map(get_layer))
    }

    fn apply_required_plugin_layer<P, OutLayer>(
        self,
        plugins: &Plugins,
        get_layer: impl FnOnce(&P) -> OutLayer,
    ) -> ServiceBuilder<Stack<OptionLayer<OutLayer>, L>>
    where
        P: PluginPrivate,
    {
        if find_plugin::<P>(plugins).is_none() && !plugins.is_empty() {
            panic!(
                "mandatory plugin {} is missing from the plugin registry, so its layer will not \
                 be applied to the pipeline; this is a router bug",
                std::any::type_name::<P>()
            );
        }
        self.apply_plugin_layer(plugins, get_layer)
    }
}

#[cfg(test)]
mod tests {
    use tower::ServiceBuilder;

    use super::InternalServiceBuilderExt;
    use crate::plugins::headers::Headers;
    use crate::services::Plugins;
    use crate::test_harness::MockedSubgraphs;

    #[test]
    fn apply_required_plugin_layer_skips_empty_registry() {
        let plugins = Plugins::default();
        let _ = ServiceBuilder::new()
            .apply_required_plugin_layer(&plugins, Headers::masking_rules_context_layer);
    }

    #[test]
    #[should_panic(expected = "mandatory plugin")]
    fn apply_required_plugin_layer_panics_when_mandatory_plugin_is_missing() {
        let mut plugins = Plugins::default();
        plugins.insert(
            "unrelated".to_string(),
            Box::new(MockedSubgraphs::default()),
        );
        let _ = ServiceBuilder::new()
            .apply_required_plugin_layer(&plugins, Headers::masking_rules_context_layer);
    }
}
