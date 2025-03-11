use serde::{Deserialize, Serialize};
use std::collections::HashMap;

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
pub(crate) struct ManifestOperation {
    pub(crate) id: String,
    pub(crate) body: String,
    pub(crate) client_name: Option<String>,
}

/// The format of each persisted query chunk returned from uplink.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub(crate) struct SignedUrlChunk {
    pub(crate) format: String,
    pub(crate) version: u64,
    pub(crate) operations: Vec<ManifestOperation>,
}

/// An in memory cache of persisted queries.
pub type PersistedQueryManifest = HashMap<FullPersistedQueryOperationId, String>;
