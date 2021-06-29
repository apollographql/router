use crate::{GraphQLFetcher, SubgraphRegistry};

use crate::http_subgraph::HttpSubgraphFetcher;
use configuration::Configuration;
use std::collections::HashMap;

/// Service registry that uses http to connect to subgraphs.
#[derive(Debug)]
pub struct HttpServiceRegistry {
    services: HashMap<String, Box<dyn GraphQLFetcher>>,
}

impl HttpServiceRegistry {
    /// Create a new http service registry from a configuration.
    pub fn new(configuration: Configuration) -> HttpServiceRegistry {
        HttpServiceRegistry {
            services: configuration
                .subgraphs
                .into_iter()
                .map(|(name, subgraph)| {
                    let fetcher: Box<dyn GraphQLFetcher> = Box::new(HttpSubgraphFetcher::new(
                        name.to_owned(),
                        subgraph.routing_url,
                    ));
                    (name, fetcher)
                })
                .collect(),
        }
    }
}

impl SubgraphRegistry for HttpServiceRegistry {
    fn get(&self, service: String) -> Option<&(dyn GraphQLFetcher)> {
        self.services.get(service.as_str()).map(|a| &**a)
    }
}

#[cfg(test)]
mod tests {
    use crate::http_service_registry::HttpServiceRegistry;
    use crate::SubgraphRegistry;
    use configuration::Configuration;

    #[test]
    fn test_from_string() {
        let config =
            serde_yaml::from_str::<Configuration>(include_str!("testdata/supergraph_config.yaml"))
                .unwrap();
        let registry = HttpServiceRegistry::new(config);
        assert!(registry.get("products".into()).is_some())
    }
}
