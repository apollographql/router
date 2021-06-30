//! Logic for loading configuration in to an object model
use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// The configuration for the router.
/// Currently maintains a mapping of subgraphs.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Configuration {
    /// Mapping of name to subgraph that the router may contact.
    pub subgraphs: HashMap<String, Subgraph>,
}

/// A subgraph.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Subgraph {
    /// The url for the subgraph.
    pub routing_url: String,
}

#[cfg(test)]
mod tests {
    use maplit::hashmap;

    use super::*;

    #[test]
    fn test_supergraph_config_serde() {
        let result =
            serde_yaml::from_str::<Configuration>(include_str!("testdata/supergraph_config.yaml"));
        assert_eq!(
            result.unwrap(),
            Configuration {
                subgraphs: hashmap! {
                    "accounts".to_string() => Subgraph {
                        routing_url: "http://localhost:4001/graphql".to_string()
                    },
                    "reviews".to_string() => Subgraph {
                        routing_url: "http://localhost:4002/graphql".to_string()
                    },
                    "products".to_string() => Subgraph {
                        routing_url: "http://localhost:4003/graphql".to_string()
                    },
                    "inventory".to_string() => Subgraph {
                        routing_url: "http://localhost:4004/graphql".to_string()
                    },
                }
            }
        )
    }
}
