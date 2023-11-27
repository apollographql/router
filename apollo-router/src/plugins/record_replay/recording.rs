use std::collections::HashMap;
use std::path::PathBuf;

use serde::Deserialize;
use serde::Serialize;
use serde_json_bytes::ByteString;
use serde_json_bytes::Map;
use serde_json_bytes::Value;
use sha2::Digest;
use sha2::Sha256;

use crate::graphql::Response;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct Recording {
    pub(crate) supergraph_sdl: String,
    pub(crate) client_request: RequestDetails,
    pub(crate) client_response: ResponseDetails,
    pub(crate) formatted_query_plan: Option<String>,
    pub(crate) subgraph_fetches: Option<Subgraphs>,
}

impl Recording {
    pub(super) fn filename(&self) -> PathBuf {
        let operation_name = self
            .client_request
            .operation_name
            .clone()
            .unwrap_or("UnknownOperation".to_string());

        let mut digest = Sha256::new();
        let req = serde_json::to_string(&self.client_request).expect("can serialize");
        digest.update(req);
        let hash = hex::encode(digest.finalize().as_slice());

        PathBuf::from(format!("{}-{}.json", operation_name, hash))
    }
}

#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub(crate) struct RequestDetails {
    pub(crate) query: Option<String>,
    pub(crate) operation_name: Option<String>,
    pub(crate) variables: Map<ByteString, Value>,
    pub(crate) headers: HashMap<String, Vec<String>>,
    pub(crate) method: String,
    pub(crate) uri: String,
}

#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub(crate) struct ResponseDetails {
    pub(crate) chunks: Vec<Response>,
    pub(crate) headers: HashMap<String, Vec<String>>,
}

#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub(crate) struct Subgraph {
    pub(crate) subgraph_name: String,
    pub(crate) request: RequestDetails,
    pub(crate) response: ResponseDetails,
}

pub(crate) type Subgraphs = HashMap<String, Subgraph>;
