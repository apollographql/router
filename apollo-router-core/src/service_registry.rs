use crate::prelude::graphql::*;
use futures::future::BoxFuture;
use std::collections::HashMap;
use std::fmt;

pub struct ServiceRegistry {
    services: HashMap<
        String,
        Box<dyn DynService<SubgraphRequest, Response = RouterResponse, Error = FetchError>>,
    >,
}

impl fmt::Debug for ServiceRegistry {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let mut debug = f.debug_tuple("ServiceRegistry");
        for name in self.services.keys() {
            debug.field(name);
        }
        debug.finish()
    }
}

impl ServiceRegistry {
    pub fn new() -> Self {
        Self {
            services: Default::default(),
        }
    }

    pub fn with_capacity(size: usize) -> Self {
        Self {
            services: HashMap::with_capacity(size),
        }
    }

    pub fn insert<S>(&mut self, name: impl Into<String>, service: S)
    where
        S: tower::Service<SubgraphRequest, Response = RouterResponse, Error = FetchError>
            + Clone
            + Send
            + Sync
            + 'static,
        S::Future: Send + 'static,
    {
        self.services
            .insert(name.into(), Box::new(service) as Box<_>);
    }

    pub fn len(&self) -> usize {
        self.services.len()
    }

    pub fn is_empty(&self) -> bool {
        self.services.is_empty()
    }

    pub fn contains(&self, name: impl AsRef<str>) -> bool {
        self.services.contains_key(name.as_ref())
    }

    pub(crate) fn get(
        &self,
        name: impl AsRef<str>,
    ) -> Option<Box<dyn DynService<SubgraphRequest, Response = RouterResponse, Error = FetchError>>>
    {
        self.services.get(name.as_ref()).map(|x| x.clone_box())
    }
}

pub(crate) trait DynService<Request>: Send + Sync {
    type Response;
    type Error;

    fn ready<'a>(&'a mut self) -> BoxFuture<'a, Result<(), Self::Error>>;
    fn call(&mut self, req: Request) -> BoxFuture<'static, Result<Self::Response, Self::Error>>;
    fn clone_box(
        &self,
    ) -> Box<dyn DynService<Request, Response = Self::Response, Error = Self::Error>>;
}

impl<T, R> DynService<R> for T
where
    T: tower::Service<R> + Clone + Send + Sync + 'static,
    T::Future: Send + 'static,
{
    type Response = <T as tower::Service<R>>::Response;
    type Error = <T as tower::Service<R>>::Error;

    fn ready<'a>(&'a mut self) -> BoxFuture<'a, Result<(), Self::Error>> {
        Box::pin(futures::future::poll_fn(move |cx| self.poll_ready(cx)))
    }
    fn call(&mut self, req: R) -> BoxFuture<'static, Result<Self::Response, Self::Error>> {
        let fut = tower::Service::call(self, req);
        Box::pin(fut)
    }
    fn clone_box(&self) -> Box<dyn DynService<R, Response = Self::Response, Error = Self::Error>> {
        Box::new(self.clone())
    }
}
