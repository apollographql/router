use crate::{RouterResponse, SubgraphRequest};
use std::collections::HashMap;
use tower::buffer::Buffer;
use tower::util::{BoxCloneService, BoxService};
use tower::ServiceExt;
use tower::{BoxError, ServiceBuilder};

pub struct ServiceRegistry {
    services: HashMap<
        String,
        Buffer<BoxService<SubgraphRequest, RouterResponse, BoxError>, SubgraphRequest>,
    >,
}

impl ServiceRegistry {
    pub(crate) fn new(
        concurrency: usize,
        services: HashMap<String, BoxService<SubgraphRequest, RouterResponse, BoxError>>,
    ) -> Self {
        Self {
            services: services
                .into_iter()
                .map(|(name, s)| (name, ServiceBuilder::new().buffer(concurrency).service(s)))
                .collect(),
        }
    }

    pub fn get(
        &self,
        name: &str,
    ) -> Option<BoxCloneService<SubgraphRequest, RouterResponse, BoxError>> {
        self.services.get(name).map(|s| s.clone().boxed_clone())
    }

    pub fn contains(&self, name: &str) -> bool {
        self.services.contains_key(name)
    }
}
