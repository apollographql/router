//! Layer that folds the configured Rust plugins over a service to build a plugin pipeline.

use std::sync::Arc;

use tower::Layer;

use crate::plugin::DynPlugin;
use crate::services::Plugins;

/// A [`Layer`] that folds the configured plugins over the inner service to build a plugin
/// pipeline.
///
/// See [`InternalServiceBuilderExt::rust_plugins`].
///
/// [`InternalServiceBuilderExt::rust_plugins`]: crate::layers::InternalServiceBuilderExt::rust_plugins
pub(crate) struct RustPluginsLayer<F> {
    plugins: Arc<Plugins>,
    apply: F,
}

impl<F> RustPluginsLayer<F> {
    pub(crate) fn new(plugins: Arc<Plugins>, apply: F) -> Self {
        Self { plugins, apply }
    }
}

impl<F> Clone for RustPluginsLayer<F>
where
    F: Clone,
{
    fn clone(&self) -> Self {
        Self {
            plugins: self.plugins.clone(),
            apply: self.apply.clone(),
        }
    }
}

impl<F, S> Layer<S> for RustPluginsLayer<F>
where
    F: Fn(&dyn DynPlugin, S) -> S,
{
    type Service = S;

    fn layer(&self, inner: S) -> S {
        self.plugins
            .values()
            .rev()
            .fold(inner, |acc, plugin| (self.apply)(plugin.as_ref(), acc))
    }
}
