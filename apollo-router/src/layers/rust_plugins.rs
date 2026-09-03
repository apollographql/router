//! Layer that folds the configured Rust plugins over a service to build a plugin pipeline.

use std::marker::PhantomData;
use std::sync::Arc;

use tower::Layer;
use tower::util::BoxCloneService;
use tower_service::Service;

use crate::plugin::DynPlugin;
use crate::services::Plugins;

/// A [`Layer`] that folds the configured plugins over the inner service to build a plugin
/// pipeline.
///
/// See [`InternalServiceBuilderExt::rust_plugins`].
///
/// [`InternalServiceBuilderExt::rust_plugins`]: crate::layers::InternalServiceBuilderExt::rust_plugins
pub(crate) struct RustPluginsLayer<F, R> {
    plugins: Arc<Plugins>,
    apply: F,
    // `R` (the request type of the boxed service `apply` operates on) doesn't otherwise
    // appear in this struct, but is needed to pin down the `Layer<S>` impl below.
    _request: PhantomData<fn(R)>,
}

impl<F, R> RustPluginsLayer<F, R> {
    pub(crate) fn new(plugins: Arc<Plugins>, apply: F) -> Self {
        Self {
            plugins,
            apply,
            _request: PhantomData,
        }
    }
}

impl<F, R> Clone for RustPluginsLayer<F, R>
where
    F: Clone,
{
    fn clone(&self) -> Self {
        Self {
            plugins: self.plugins.clone(),
            apply: self.apply.clone(),
            _request: PhantomData,
        }
    }
}

impl<R, F, S> Layer<S> for RustPluginsLayer<F, R>
where
    F: Fn(
        &dyn DynPlugin,
        BoxCloneService<R, S::Response, S::Error>,
    ) -> BoxCloneService<R, S::Response, S::Error>,
    S: Service<R> + Clone + Send + 'static,
    S::Error: Send + 'static,
    S::Future: Send + 'static,
{
    type Service = BoxCloneService<R, S::Response, S::Error>;

    fn layer(&self, inner: S) -> Self::Service {
        let boxed = BoxCloneService::new(inner);
        self.plugins
            .values()
            .rev()
            .fold(boxed, |acc, plugin| (self.apply)(plugin.as_ref(), acc))
    }
}
