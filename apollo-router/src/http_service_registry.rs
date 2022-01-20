use crate::configuration::Configuration;
use crate::http_subgraph::HttpSubgraphFetcher;
use apollo_router_core::prelude::*;
use std::collections::HashMap;
use std::fmt;

/// Service registry that uses http to connect to subgraphs.
pub struct HttpServiceRegistry {
    services: HashMap<String, HttpSubgraphFetcher>,
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
                    (name.to_string(), fetcher)
                })
                .collect(),
        }
    }

    // test dynamically adding a Service
    pub fn add_service<S>(&mut self, name: String, service: HttpSubgraphFetcher) {
        self.services.insert(name, service);
    }
}

impl graphql::ServiceRegistry for HttpServiceRegistry {
    fn get(&self, service: &str) -> Option<&dyn Fetcher> {
        self.services.get(service).map(|x| x as &dyn Fetcher)
    }

    fn has(&self, service: &str) -> bool {
        self.services.get(service).is_some()
    }
}

// TODO POC: same service registry but using boxed fetcher
pub struct HttpBoxedServiceRegistry {
    services: HashMap<String, Box<dyn graphql::Fetcher>>,
}

impl fmt::Debug for HttpBoxedServiceRegistry {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let mut debug = f.debug_tuple("HttpServiceRegistry");
        for name in self.services.keys() {
            debug.field(name);
        }
        debug.finish()
    }
}

impl HttpBoxedServiceRegistry {
    /// Create a new http service registry from a configuration.
    pub fn new(configuration: &Configuration) -> Self {
        Self {
            services: configuration
                .subgraphs
                .iter()
                .map(|(name, subgraph)| {
                    let fetcher = Box::new(HttpSubgraphFetcher::new(
                        name.to_owned(),
                        subgraph.routing_url.to_owned(),
                    )) as Box<_>;
                    (name.to_string(), fetcher)
                })
                .collect(),
        }
    }

    // test dynamically adding a Service
    pub fn add_service<S>(&mut self, name: String, service: Box<dyn graphql::Fetcher>) {
        self.services.insert(name, service);
    }
}

impl graphql::ServiceRegistry for HttpBoxedServiceRegistry {
    fn get(&self, service: &str) -> Option<&dyn graphql::Fetcher> {
        self.services.get(service).map(|x| &**x)
    }

    fn has(&self, service: &str) -> bool {
        self.services.get(service).is_some()
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
