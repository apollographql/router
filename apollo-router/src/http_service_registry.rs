use crate::configuration::Configuration;
use crate::http_subgraph::HttpSubgraphFetcher;
use apollo_router_core::prelude::*;
use futures::Future;
use std::collections::HashMap;
use std::fmt;
use std::pin::Pin;
use std::sync::Arc;
use tower::Service;

/// Service registry that uses http to connect to subgraphs.
pub struct HttpServiceRegistry {
    services: HashMap<String, Arc<dyn NewSubgraphService>>,
}

impl fmt::Debug for HttpServiceRegistry {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let mut debug = f.debug_tuple("HttpServiceRegistry");
        for name in self.services.keys() {
            debug.field(name);
        }
        debug.finish()
    }
}

impl HttpServiceRegistry {
    /// Create a new http service registry from a configuration.
    pub fn new(configuration: &Configuration) -> Self {
        Self {
            services: configuration
                .subgraphs
                .iter()
                .map(|(name, subgraph)| {
                    let fetcher =
                        HttpSubgraphFetcher::new(name.to_owned(), subgraph.routing_url.to_owned());
                    let cloner: Arc<dyn NewSubgraphService> = Arc::new(move || fetcher.clone());
                    (name.to_string(), cloner)
                })
                .collect(),
        }
    }

    // test dynamically adding a Service
    pub fn add_service<S>(&mut self, name: String, service: S)
    where
        S: Clone + SubgraphService,
    {
        let cloner: Arc<dyn NewSubgraphService> = Arc::new(move || service.clone());
        self.services.insert(name, cloner);
    }
}

impl graphql::ServiceRegistry for HttpServiceRegistry {
    fn get(&self, service: &str) -> Option<Box<dyn SubgraphService>> {
        self.services.get(service).map(|a| a.new_service())
    }

    fn has(&self, service: &str) -> bool {
        self.services.get(service).is_some()
    }
}

impl Service<(String, graphql::Request)> for HttpServiceRegistry {
    type Response = graphql::Response;

    type Error = graphql::FetchError;

    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>>>>;

    fn poll_ready(
        &mut self,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        std::task::Poll::Ready(Ok(()))
    }

    fn call(&mut self, (service_name, request): (String, graphql::Request)) -> Self::Future {
        let mut service = self
            .services
            .get(&service_name)
            .map(|a| &**a)
            .unwrap()
            .new_service();

        service.call(request)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::configuration::Configuration;

    #[test]
    fn test_from_string() {
        let config =
            serde_yaml::from_str::<Configuration>(include_str!("testdata/supergraph_config.yaml"))
                .unwrap();
        let registry = HttpServiceRegistry::new(&config);
        assert!(registry.get("products").is_some())
    }
}
