use crate::Object;
use serde_json_bytes::{ByteString, Value};
use std::sync::Arc;

pub mod execution_request;
pub mod execution_response;
pub mod router_request;
pub mod router_response;
pub mod subgraph_response;

type CompatRequest = Arc<crate::http_compat::Request<crate::Request>>;

fn from_names_and_values(extensions: Vec<(&str, Value)>) -> Object {
    extensions
        .into_iter()
        .map(|(name, value)| (ByteString::from(name.to_string()), value))
        .collect()
}
