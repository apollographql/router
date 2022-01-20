use crate::prelude::graphql::*;
use futures::future::BoxFuture;
use std::collections::HashMap;
use std::fmt;
use std::task::{Context, Poll};
use tower::util::BoxCloneService;
use tower::ServiceExt;

pub struct ServiceRegistry2 {
    services: HashMap<String, BoxCloneService<SubgraphRequest, Response, FetchError>>,
}

impl fmt::Debug for ServiceRegistry2 {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let mut debug = f.debug_tuple("ServiceRegistry2");
        for name in self.services.keys() {
            debug.field(name);
        }
        debug.finish()
    }
}

impl ServiceRegistry2 {
    pub fn new(
        services: impl IntoIterator<
            Item = (
                String,
                BoxCloneService<SubgraphRequest, Response, FetchError>,
            ),
        >,
    ) -> Self {
        Self {
            services: services.into_iter().collect(),
        }
    }

    pub fn insert(
        &mut self,
        name: impl Into<String>,
        service: BoxCloneService<SubgraphRequest, Response, FetchError>,
    ) {
        self.services.insert(name.into(), service);
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

    pub fn get(
        &self,
        name: impl AsRef<str>,
    ) -> Option<&BoxCloneService<SubgraphRequest, Response, FetchError>> {
        self.services.get(name.as_ref())
    }
}
