use std::sync::Arc;

use apollo_compiler::ExecutableDocument;
use apollo_compiler::Schema;
use apollo_compiler::validation::Valid;
use apollo_compiler::validation::WithErrors;
use serde::Deserialize;
use serde::Serialize;

/// Like `Valid<ExecutableDocument>>` but can be (de)serialized as a string in GraphQL syntax.
///
/// The relevant schema is required to parse from string but not available during deserialization,
/// so this contains a dual (either or both) “string” or “parsed” representation.
/// Accessing the latter is fallible, and requires an explicit initialization step to provide the schema.
#[derive(Clone)]
pub struct SerializableDocument {
    serialized: String,
    /// Ideally this would be always present,
    /// but we don’t have access to the relevant schema during `Deserialize`.
    parsed: Option<Arc<Valid<ExecutableDocument>>>,
}

impl SerializableDocument {
    pub fn from_string(serialized: impl Into<String>) -> Self {
        Self {
            serialized: serialized.into(),
            parsed: None,
        }
    }

    pub fn from_parsed(parsed: impl Into<Arc<Valid<ExecutableDocument>>>) -> Self {
        let parsed = parsed.into();
        Self {
            serialized: parsed.serialize().no_indent().to_string(),
            parsed: Some(parsed),
        }
    }

    pub fn as_serialized(&self) -> &str {
        &self.serialized
    }

    #[allow(clippy::result_large_err)]
    pub fn init_parsed(
        &mut self,
        subgraph_schema: &Valid<Schema>,
    ) -> Result<&Arc<Valid<ExecutableDocument>>, WithErrors<ExecutableDocument>> {
        match &mut self.parsed {
            Some(parsed) => Ok(parsed),
            option => {
                let parsed = Arc::new(ExecutableDocument::parse_and_validate(
                    subgraph_schema,
                    &self.serialized,
                    "operation.graphql",
                )?);
                Ok(option.insert(parsed))
            }
        }
    }

    pub fn as_parsed(
        &self,
    ) -> Result<&Arc<Valid<ExecutableDocument>>, SerializableDocumentNotInitialized> {
        self.parsed
            .as_ref()
            .ok_or(SerializableDocumentNotInitialized)
    }
}

#[derive(Debug, thiserror::Error)]
#[error("Failed to call `SerializableDocument::init_parsed` after creating a query plan")]
pub struct SerializableDocumentNotInitialized;

impl Serialize for SerializableDocument {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        self.as_serialized().serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for SerializableDocument {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        Ok(Self::from_string(String::deserialize(deserializer)?))
    }
}

impl PartialEq for SerializableDocument {
    fn eq(&self, other: &Self) -> bool {
        self.as_serialized() == other.as_serialized()
    }
}

impl std::fmt::Debug for SerializableDocument {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(self.as_serialized(), f)
    }
}

impl std::fmt::Display for SerializableDocument {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(self.as_serialized(), f)
    }
}
