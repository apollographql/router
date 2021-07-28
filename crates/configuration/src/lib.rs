//! Logic for loading configuration in to an object model
use std::collections::HashMap;
use std::net::SocketAddr;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use typed_builder::TypedBuilder;

/// The configuration for the router.
/// Currently maintains a mapping of subgraphs.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TypedBuilder)]
pub struct Configuration {
    /// The socket address and port to listen on
    /// Defaults to 127.0.0.1:4000
    #[serde(default = "default_listen")]
    #[builder(default_code = "default_listen()")]
    pub listen: SocketAddr,

    /// Mapping of name to subgraph that the router may contact.
    pub subgraphs: HashMap<String, Subgraph>,
}

fn default_listen() -> SocketAddr {
    SocketAddr::from_str("127.0.0.1:4000").unwrap()
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
                listen: SocketAddr::from_str("127.0.0.1:4001").unwrap(),
                subgraphs: hashmap! {
                    "accounts".to_string() => Subgraph {
                        routing_url: "http://localhost:4001/graphql".into()
                    },
                    "reviews".to_string() => Subgraph {
                        routing_url: "http://localhost:4002/graphql".into()
                    },
                    "products".to_string() => Subgraph {
                        routing_url: "http://localhost:4003/graphql".into()
                    },
                    "inventory".to_string() => Subgraph {
                        routing_url: "http://localhost:4004/graphql".into()
                    },
                }
            }
        )
    }
}
