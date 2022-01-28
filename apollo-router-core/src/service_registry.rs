use crate::{RouterResponse, SubgraphRequest};
use futures::lock::Mutex;
use std::collections::{HashMap, HashSet};
use tower::util::BoxCloneService;
use tower::BoxError;

pub struct ServiceRegistry {
    service_names: HashSet<String>,
    services: Mutex<HashMap<String, BoxCloneService<SubgraphRequest, RouterResponse, BoxError>>>,
}

impl ServiceRegistry {
    pub(crate) fn new(
        services: HashMap<String, BoxCloneService<SubgraphRequest, RouterResponse, BoxError>>,
    ) -> Self {
        Self {
            service_names: services
                .keys()
                .map(|k| k.to_string())
                .collect::<HashSet<String>>(),
            services: Mutex::new(services),
        }
    }

    pub async fn get(
        &self,
        name: &str,
    ) -> Option<BoxCloneService<SubgraphRequest, RouterResponse, BoxError>> {
        self.services.lock().await.get(name).cloned()
    }

    pub fn contains(&self, name: &str) -> bool {
        self.service_names.contains(name)
    }
}
