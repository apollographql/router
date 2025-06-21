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
                .field("key_selection", &keys.serialize().no_indent().to_string())
                .field("inputs", inputs)
                .finish(),
        }
    }
}
