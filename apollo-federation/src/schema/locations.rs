use apollo_compiler::Node;
use apollo_compiler::ast::DirectiveDefinition;
use apollo_compiler::schema::ExtendedType;

use crate::error::HasLocations;
use crate::error::Locations;
use crate::error::SubgraphLocation;
use crate::merger::compose_directive_manager::MergeDirectiveItem;
use crate::schema::position::AbstractTypeDefinitionPosition;
use crate::schema::position::CompositeTypeDefinitionPosition;
use crate::schema::position::DirectiveArgumentDefinitionPosition;
use crate::schema::position::DirectiveDefinitionPosition;
use crate::schema::position::DirectiveTargetPosition;
use crate::schema::position::EnumTypeDefinitionPosition;
use crate::schema::position::EnumValueDefinitionPosition;
use crate::schema::position::FieldArgumentDefinitionPosition;
use crate::schema::position::FieldDefinitionPosition;
use crate::schema::position::InputObjectFieldDefinitionPosition;
use crate::schema::position::InputObjectTypeDefinitionPosition;
use crate::schema::position::InterfaceFieldArgumentDefinitionPosition;
use crate::schema::position::InterfaceFieldDefinitionPosition;
use crate::schema::position::InterfaceTypeDefinitionPosition;
use crate::schema::position::ObjectFieldArgumentDefinitionPosition;
use crate::schema::position::ObjectFieldDefinitionPosition;
use crate::schema::position::ObjectOrInterfaceFieldDefinitionPosition;
use crate::schema::position::ObjectOrInterfaceTypeDefinitionPosition;
use crate::schema::position::ObjectTypeDefinitionPosition;
use crate::schema::position::OutputTypeDefinitionPosition;
use crate::schema::position::ScalarTypeDefinitionPosition;
use crate::schema::position::SchemaDefinitionPosition;
use crate::schema::position::SchemaRootDefinitionPosition;
use crate::schema::position::TypeDefinitionPosition;
use crate::schema::position::UnionTypeDefinitionPosition;
use crate::schema::position::UnionTypenameFieldDefinitionPosition;
use crate::subgraph::typestate::HasMetadata;
use crate::subgraph::typestate::Subgraph;

impl HasLocations for DirectiveDefinition {
    fn locations<T: HasMetadata>(&self, subgraph: &Subgraph<T>) -> Locations {
        subgraph
            .schema()
            .schema()
            .directive_definitions
            .get(&self.name)
            .map(|dir| dir.locations(subgraph))
            .unwrap_or_default()
    }
}

impl HasLocations for MergeDirectiveItem {
    fn locations<T: HasMetadata>(&self, subgraph: &Subgraph<T>) -> Locations {
        self.definition.locations(subgraph)
    }
}

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

impl HasLocations for TypeDefinitionPosition {
    fn locations<T: HasMetadata>(&self, subgraph: &Subgraph<T>) -> Locations {
        let schema = subgraph.schema();
        self.get(schema.schema())
            .map(|ty| ty.locations(subgraph))
            .unwrap_or_default()
    }
}

impl HasLocations for CompositeTypeDefinitionPosition {
    fn locations<T: HasMetadata>(&self, subgraph: &Subgraph<T>) -> Locations {
        let schema = subgraph.schema();
        self.get(schema.schema())
            .map(|ty| ty.locations(subgraph))
            .unwrap_or_default()
    }
}

impl HasLocations for OutputTypeDefinitionPosition {
    fn locations<T: HasMetadata>(&self, subgraph: &Subgraph<T>) -> Locations {
        match self {
            Self::Scalar(p) => p.locations(subgraph),
            Self::Object(p) => p.locations(subgraph),
            Self::Interface(p) => p.locations(subgraph),
            Self::Union(p) => p.locations(subgraph),
            Self::Enum(p) => p.locations(subgraph),
        }
    }
}

impl HasLocations for AbstractTypeDefinitionPosition {
    fn locations<T: HasMetadata>(&self, subgraph: &Subgraph<T>) -> Locations {
        match self {
            Self::Interface(p) => p.locations(subgraph),
            Self::Union(p) => p.locations(subgraph),
        }
    }
}

impl HasLocations for ObjectOrInterfaceTypeDefinitionPosition {
    fn locations<T: HasMetadata>(&self, subgraph: &Subgraph<T>) -> Locations {
        match self {
            Self::Object(p) => p.locations(subgraph),
            Self::Interface(p) => p.locations(subgraph),
        }
    }
}

impl HasLocations for FieldDefinitionPosition {
    fn locations<T: HasMetadata>(&self, subgraph: &Subgraph<T>) -> Vec<SubgraphLocation> {
        let schema = subgraph.schema();
        self.get(schema.schema())
            .map(|node| subgraph.node_locations(node))
            .unwrap_or_default()
    }
}

impl HasLocations for ObjectOrInterfaceFieldDefinitionPosition {
    fn locations<T: HasMetadata>(&self, subgraph: &Subgraph<T>) -> Locations {
        let schema = subgraph.schema();
        self.get(schema.schema())
            .map(|node| subgraph.node_locations(node))
            .unwrap_or_default()
    }
}

impl HasLocations for FieldArgumentDefinitionPosition {
    fn locations<T: HasMetadata>(&self, subgraph: &Subgraph<T>) -> Locations {
        let schema = subgraph.schema();
        self.get(schema.schema())
            .map(|node| subgraph.node_locations(node))
            .unwrap_or_default()
    }
}

impl HasLocations for SchemaDefinitionPosition {
    fn locations<T: HasMetadata>(&self, subgraph: &Subgraph<T>) -> Locations {
        subgraph.node_locations(self.get(subgraph.schema().schema()))
    }
}

impl HasLocations for SchemaRootDefinitionPosition {
    fn locations<T: HasMetadata>(&self, _subgraph: &Subgraph<T>) -> Locations {
        Locations::new()
    }
}

impl HasLocations for ScalarTypeDefinitionPosition {
    fn locations<T: HasMetadata>(&self, subgraph: &Subgraph<T>) -> Locations {
        let schema = subgraph.schema();
        self.get(schema.schema())
            .map(|node| subgraph.node_locations(node))
            .unwrap_or_default()
    }
}

impl HasLocations for ObjectTypeDefinitionPosition {
    fn locations<T: HasMetadata>(&self, subgraph: &Subgraph<T>) -> Locations {
        let schema = subgraph.schema();
        self.get(schema.schema())
            .map(|node| subgraph.node_locations(node))
            .unwrap_or_default()
    }
}

impl HasLocations for InterfaceTypeDefinitionPosition {
    fn locations<T: HasMetadata>(&self, subgraph: &Subgraph<T>) -> Locations {
        let schema = subgraph.schema();
        self.get(schema.schema())
            .map(|node| subgraph.node_locations(node))
            .unwrap_or_default()
    }
}

impl HasLocations for UnionTypeDefinitionPosition {
    fn locations<T: HasMetadata>(&self, subgraph: &Subgraph<T>) -> Locations {
        let schema = subgraph.schema();
        self.get(schema.schema())
            .map(|node| subgraph.node_locations(node))
            .unwrap_or_default()
    }
}

impl HasLocations for EnumTypeDefinitionPosition {
    fn locations<T: HasMetadata>(&self, subgraph: &Subgraph<T>) -> Locations {
        let schema = subgraph.schema();
        self.get(schema.schema())
            .map(|node| subgraph.node_locations(node))
            .unwrap_or_default()
    }
}

impl HasLocations for InputObjectTypeDefinitionPosition {
    fn locations<T: HasMetadata>(&self, subgraph: &Subgraph<T>) -> Locations {
        let schema = subgraph.schema();
        self.get(schema.schema())
            .map(|node| subgraph.node_locations(node))
            .unwrap_or_default()
    }
}

impl HasLocations for DirectiveDefinitionPosition {
    fn locations<T: HasMetadata>(&self, subgraph: &Subgraph<T>) -> Locations {
        let schema = subgraph.schema();
        self.get(schema.schema())
            .map(|node| subgraph.node_locations(node))
            .unwrap_or_default()
    }
}

impl HasLocations for ObjectFieldDefinitionPosition {
    fn locations<T: HasMetadata>(&self, subgraph: &Subgraph<T>) -> Locations {
        let schema = subgraph.schema();
        self.get(schema.schema())
            .map(|node| subgraph.node_locations(node))
            .unwrap_or_default()
    }
}

impl HasLocations for InterfaceFieldDefinitionPosition {
    fn locations<T: HasMetadata>(&self, subgraph: &Subgraph<T>) -> Locations {
        let schema = subgraph.schema();
        self.get(schema.schema())
            .map(|node| subgraph.node_locations(node))
            .unwrap_or_default()
    }
}

impl HasLocations for UnionTypenameFieldDefinitionPosition {
    fn locations<T: HasMetadata>(&self, subgraph: &Subgraph<T>) -> Locations {
        let schema = subgraph.schema();
        self.get(schema.schema())
            .map(|node| subgraph.node_locations(node))
            .unwrap_or_default()
    }
}

impl HasLocations for EnumValueDefinitionPosition {
    fn locations<T: HasMetadata>(&self, subgraph: &Subgraph<T>) -> Locations {
        let schema = subgraph.schema();
        self.get(schema.schema())
            .map(|node| subgraph.node_locations(node))
            .unwrap_or_default()
    }
}

impl HasLocations for InputObjectFieldDefinitionPosition {
    fn locations<T: HasMetadata>(&self, subgraph: &Subgraph<T>) -> Locations {
        let schema = subgraph.schema();
        self.get(schema.schema())
            .map(|node| subgraph.node_locations(node))
            .unwrap_or_default()
    }
}

impl HasLocations for ObjectFieldArgumentDefinitionPosition {
    fn locations<T: HasMetadata>(&self, subgraph: &Subgraph<T>) -> Locations {
        let schema = subgraph.schema();
        self.get(schema.schema())
            .map(|node| subgraph.node_locations(node))
            .unwrap_or_default()
    }
}

impl HasLocations for InterfaceFieldArgumentDefinitionPosition {
    fn locations<T: HasMetadata>(&self, subgraph: &Subgraph<T>) -> Locations {
        let schema = subgraph.schema();
        self.get(schema.schema())
            .map(|node| subgraph.node_locations(node))
            .unwrap_or_default()
    }
}

impl HasLocations for DirectiveArgumentDefinitionPosition {
    fn locations<T: HasMetadata>(&self, subgraph: &Subgraph<T>) -> Locations {
        let schema = subgraph.schema();
        self.get(schema.schema())
            .map(|node| subgraph.node_locations(node))
            .unwrap_or_default()
    }
}

impl HasLocations for DirectiveTargetPosition {
    fn locations<T: HasMetadata>(&self, subgraph: &Subgraph<T>) -> Locations {
        match self {
            DirectiveTargetPosition::Schema(pos) => pos.locations(subgraph),
            DirectiveTargetPosition::ScalarType(pos) => pos.locations(subgraph),
            DirectiveTargetPosition::ObjectType(pos) => pos.locations(subgraph),
            DirectiveTargetPosition::ObjectField(pos) => pos.locations(subgraph),
            DirectiveTargetPosition::ObjectFieldArgument(pos) => pos.locations(subgraph),
            DirectiveTargetPosition::InterfaceType(pos) => pos.locations(subgraph),
            DirectiveTargetPosition::InterfaceField(pos) => pos.locations(subgraph),
            DirectiveTargetPosition::InterfaceFieldArgument(pos) => pos.locations(subgraph),
            DirectiveTargetPosition::UnionType(pos) => pos.locations(subgraph),
            DirectiveTargetPosition::EnumType(pos) => pos.locations(subgraph),
            DirectiveTargetPosition::EnumValue(pos) => pos.locations(subgraph),
            DirectiveTargetPosition::InputObjectType(pos) => pos.locations(subgraph),
            DirectiveTargetPosition::InputObjectField(pos) => pos.locations(subgraph),
            DirectiveTargetPosition::DirectiveArgument(pos) => pos.locations(subgraph),
        }
    }
}
