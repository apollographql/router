use std::collections::HashMap;
use std::ops::Deref;
use std::ops::DerefMut;

use serde::Deserialize;
use serde::Serialize;
use tower::BoxError;

/// The full identifier for an operation in a PQ list consists of an operation
/// ID and an optional client name.
#[derive(Debug, Clone, Eq, Hash, PartialEq)]
pub struct FullPersistedQueryOperationId {
    /// The operation ID (usually a hash).
    pub operation_id: String,
    /// The client name associated with the operation; if None, can be any client.
    pub client_name: Option<String>,
}

/// A single operation containing an ID and a body,
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ManifestOperation {
    /// The operation ID (usually a hash).
    pub id: String,
    /// The operation body.
    pub body: String,
    /// The client name associated with the operation. If None, can be any client.
    pub client_name: Option<String>,
}

/// The format of each persisted query chunk returned from uplink.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub(crate) struct SignedUrlChunk {
    pub(crate) format: String,
    pub(crate) version: u64,
    pub(crate) operations: Vec<ManifestOperation>,
}

impl SignedUrlChunk {
    pub(crate) fn validate(self) -> Result<Self, BoxError> {
        if self.format != "apollo-persisted-query-manifest" {
            return Err("chunk format is not 'apollo-persisted-query-manifest'".into());
        }

        if self.version != 1 {
            return Err("persisted query manifest chunk version is not 1".into());
        }

        Ok(self)
    }

    pub(crate) fn parse_and_validate(raw_chunk: &str) -> Result<Self, BoxError> {
        let parsed_chunk =
            serde_json::from_str::<SignedUrlChunk>(raw_chunk).map_err(|e| -> BoxError {
                format!("Could not parse persisted query manifest chunk: {e}").into()
            })?;

        parsed_chunk.validate()
    }
}

/// An in memory cache of persisted queries.
// pub type PersistedQueryManifest = HashMap<FullPersistedQueryOperationId, String>;
#[derive(Debug, Clone, Default)]
pub struct PersistedQueryManifest {
    inner: HashMap<FullPersistedQueryOperationId, String>,
}

impl PersistedQueryManifest {
    /// Add a chunk to the manifest.
    pub(crate) fn add_chunk(&mut self, chunk: &SignedUrlChunk) {
        for operation in &chunk.operations {
            self.inner.insert(
                FullPersistedQueryOperationId {
                    operation_id: operation.id.clone(),
                    client_name: operation.client_name.clone(),
                },
                operation.body.clone(),
            );
        }
    }
}

impl From<Vec<ManifestOperation>> for PersistedQueryManifest {
    fn from(operations: Vec<ManifestOperation>) -> Self {
        let mut manifest = PersistedQueryManifest::default();
        for operation in operations {
            manifest.insert(
                FullPersistedQueryOperationId {
                    operation_id: operation.id,
                    client_name: operation.client_name,
                },
                operation.body,
            );
        }
        manifest
    }
}

impl Deref for PersistedQueryManifest {
    type Target = HashMap<FullPersistedQueryOperationId, String>;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl DerefMut for PersistedQueryManifest {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.inner
    }
}
