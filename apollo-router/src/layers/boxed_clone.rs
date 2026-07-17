use std::marker::PhantomData;

use tower::util::BoxCloneService;

/// Layer that produces a [`BoxCloneService`].
///
/// See [`InternalServiceBuilderExt::concrete_boxed_clone`].
///
/// [`InternalServiceBuilderExt::concrete_boxed_clone`]: crate::layers::InternalServiceBuilderExt::concrete_boxed_clone
#[derive(Clone)]
pub(crate) struct BoxCloneLayer<R> {
    _private: PhantomData<R>,
}

impl<R> BoxCloneLayer<R> {
    pub(crate) fn new() -> Self {
        Self {
            _private: PhantomData,
        }
    }
}

impl<R, S> tower::Layer<S> for BoxCloneLayer<R>
where
    S: tower::Service<R> + Clone + Send + 'static,
    S::Future: Send + 'static,
{
    type Service = BoxCloneService<R, S::Response, S::Error>;

    fn layer(&self, inner: S) -> Self::Service {
        BoxCloneService::new(inner)
    }
}
