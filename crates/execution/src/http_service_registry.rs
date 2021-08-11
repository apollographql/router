use std::collections::HashMap;

use configuration::Configuration;

use crate::http_subgraph::HttpSubgraphFetcher;
use crate::{GraphQLFetcher, ServiceRegistry};

/// Service registry that uses http to connect to subgraphs.
#[derive(Debug)]
pub struct HttpServiceRegistry {
    services: HashMap<String, Box<dyn GraphQLFetcher>>,
}

impl HttpServiceRegistry {
    /// Create a new http service registry from a configuration.
    pub fn new(configuration: &Configuration) -> Self {
        Self {
            services: configuration
                .subgraphs
                .iter()
                .map(|(name, subgraph)| {
                    let fetcher: Box<dyn GraphQLFetcher> = Box::new(HttpSubgraphFetcher::new(
                        name.to_owned(),
                        subgraph.routing_url.to_owned(),
                    ));
                    (name.to_string(), fetcher)
                })
                .collect(),
        }
    }
}

impl ServiceRegistry for HttpServiceRegistry {
    fn get(&self, service: &str) -> Option<&(dyn GraphQLFetcher)> {
        self.services.get(service).map(|a| &**a)
    }

    fn has(&self, service: &str) -> bool {
        self.services.get(service).is_some()
    }
}

#[cfg(test)]
mod tests {
    use configuration::Configuration;

    use crate::http_service_registry::HttpServiceRegistry;
    use crate::ServiceRegistry;

    #[test]
    fn test_from_string() {
        let config =
            serde_yaml::from_str::<Configuration>(include_str!("testdata/supergraph_config.yaml"))
                .unwrap();
        let registry = HttpServiceRegistry::new(&config);
        assert!(registry.get("products").is_some())
    }
}
