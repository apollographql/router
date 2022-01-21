use crate::prelude::graphql::*;
use std::collections::HashMap;
use std::fmt;

pub struct ServiceRegistry {
    services: HashMap<
        String,
        Box<dyn DynCloneService<SubgraphRequest, Response = RouterResponse, Error = FetchError>>,
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
    ) -> Option<
        Box<dyn DynCloneService<SubgraphRequest, Response = RouterResponse, Error = FetchError>>,
    > {
        self.services.get(name.as_ref()).map(|x| x.clone_box())
    }
}
