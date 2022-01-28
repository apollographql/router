use crate::configuration::Configuration;
use crate::http_subgraph::HttpSubgraphFetcher;
use apollo_router_core::prelude::*;
use std::collections::HashMap;
use std::fmt;

/// Service registry that uses http to connect to subgraphs.
pub struct HttpServiceRegistry {
    services: HashMap<String, Box<dyn graphql::Fetcher>>,
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
                    let fetcher: Box<dyn graphql::Fetcher> = Box::new(HttpSubgraphFetcher::new(
                        name.to_owned(),
                        subgraph.routing_url.to_owned(),
                    ));
                    (name.to_string(), fetcher)
                })
                .collect(),
        }
    }
}

impl graphql::ServiceRegistry for HttpServiceRegistry {
    fn get(&self, service: &str) -> Option<&(dyn graphql::Fetcher)> {
        self.services.get(service).map(|a| &**a)
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
