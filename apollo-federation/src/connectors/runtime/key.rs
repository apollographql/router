use std::sync::Arc;

use apollo_compiler::Name;
use apollo_compiler::executable::FieldSet;
use apollo_compiler::validation::Valid;

use crate::connectors::JSONSelection;
use crate::connectors::runtime::inputs::RequestInputs;

#[derive(Clone)]
pub enum ResponseKey {
    RootField {
        name: String,
        selection: Arc<JSONSelection>,
        inputs: RequestInputs,
    },
    Entity {
        index: usize,
        selection: Arc<JSONSelection>,
        inputs: RequestInputs,
    },
    EntityField {
        index: usize,
        field_name: String,
        /// Is Some only if the output type is a concrete object type. If it's
        /// an interface, it's treated as an interface object and we can't emit
        /// a __typename in the response.
        typename: Option<Name>,
        selection: Arc<JSONSelection>,
        inputs: RequestInputs,
    },
    BatchEntity {
        selection: Arc<JSONSelection>,
        keys: Valid<FieldSet>,
        inputs: RequestInputs,
    },
}

impl ResponseKey {
    pub fn selection(&self) -> &JSONSelection {
        match self {
            ResponseKey::RootField { selection, .. } => selection,
            ResponseKey::Entity { selection, .. } => selection,
            ResponseKey::EntityField { selection, .. } => selection,
            ResponseKey::BatchEntity { selection, .. } => selection,
        }
    }

    pub fn inputs(&self) -> &RequestInputs {
        match self {
            ResponseKey::RootField { inputs, .. } => inputs,
            ResponseKey::Entity { inputs, .. } => inputs,
            ResponseKey::EntityField { inputs, .. } => inputs,
            ResponseKey::BatchEntity { inputs, .. } => inputs,
        }
    }

    /// Returns a serialized representation of the Path from apollo-router.
    /// Intended to be parsed into a Path when converting a connectors
    /// `RuntimeError` in the router's graphql::Error.
    ///
    /// This mimics the behavior of a GraphQL subgraph, including the `_entities`
    /// field. When the path gets to `FetchNode::response_at_path`, it will be
    /// amended and appended to a parent path to create the full path to the
    /// field. For example:
    ///
    /// - parent path: `["posts", @, "user"]`
    /// - path from key: `["_entities", 0, "user", "profile"]`
    /// - result: `["posts", 1, "user", "profile"]`
    pub fn path_string(&self) -> String {
        match self {
            ResponseKey::RootField { name, .. } => name.to_string(),
            ResponseKey::Entity { index, .. } => format!("_entities/{index}"),
            ResponseKey::EntityField {
                index, field_name, ..
            } => format!("_entities/{index}/{field_name}"),
            ResponseKey::BatchEntity { .. } => "_entities".to_string(),
        }
    }
}

impl std::fmt::Debug for ResponseKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::RootField {
                name,
                selection,
                inputs,
            } => f
                .debug_struct("RootField")
                .field("name", name)
                .field("selection", &selection.to_string())
                .field("inputs", inputs)
                .finish(),
            Self::Entity {
                index,
                selection,
                inputs,
            } => f
                .debug_struct("Entity")
                .field("index", index)
                .field("selection", &selection.to_string())
                .field("inputs", inputs)
                .finish(),
            Self::EntityField {
                index,
                field_name,
                typename,
                selection,
                inputs,
            } => f
                .debug_struct("EntityField")
                .field("index", index)
                .field("field_name", field_name)
                .field("typename", typename)
                .field("selection", &selection.to_string())
                .field("inputs", inputs)
                .finish(),
            Self::BatchEntity {
                selection,
                keys,
                inputs,
            } => f
                .debug_struct("BatchEntity")
                .field("selection", &selection.to_string())
                .field("key", &keys.serialize().no_indent().to_string())
                .field("inputs", inputs)
                .finish(),
        }
    }
}
