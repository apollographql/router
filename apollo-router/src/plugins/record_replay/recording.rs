use std::collections::HashMap;

use serde::Deserialize;
use serde::Serialize;
use serde_json_bytes::ByteString;
use serde_json_bytes::Map;
use serde_json_bytes::Value;

use crate::graphql::Response;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct Recording {
    pub(crate) supergraph_sdl: String,
    pub(crate) client_request: RequestDetails,
    pub(crate) client_response: ResponseDetails,
    pub(crate) formatted_query_plan: Option<String>,
    pub(crate) subgraph_fetches: Option<Subgraphs>,
}

#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub(crate) struct RequestDetails {
    pub(crate) query: Option<String>,
    pub(crate) operation_name: Option<String>,
    pub(crate) variables: Map<ByteString, Value>,
    pub(crate) headers: HashMap<String, Vec<String>>,
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
