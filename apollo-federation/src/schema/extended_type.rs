use apollo_compiler::Node;
use apollo_compiler::schema::ExtendedType;

use crate::error::HasLocations;
use crate::error::Locations;
use crate::error::SubgraphLocation;
use crate::subgraph::typestate::HasMetadata;
use crate::subgraph::typestate::Subgraph;

impl HasLocations for ExtendedType {
    fn locations<T: HasMetadata>(&self, subgraph: &Subgraph<T>) -> Locations {
        match self {
            Self::Scalar(node) => node.locations(subgraph),
            Self::Object(node) => node.locations(subgraph),
            Self::Interface(node) => node.locations(subgraph),
            Self::Union(node) => node.locations(subgraph),
            Self::Enum(node) => node.locations(subgraph),
            Self::InputObject(node) => node.locations(subgraph),
        }
    }
}

impl<T> HasLocations for Node<T> {
    fn locations<U: HasMetadata>(&self, subgraph: &Subgraph<U>) -> Locations {
        subgraph
            .schema()
            .node_locations(self)
            .map(|range| SubgraphLocation {
                subgraph: subgraph.name.to_string(),
                range,
            })
            .collect()
    }
}
