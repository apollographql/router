use serde::{Deserialize, Serialize};

use crate::{graphql, json_ext::Object, plugins::response_cache::cache_control::CacheControl};

pub(super) type CacheKeysContext = Vec<CacheKeyContext>;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct CacheKeyContext {
    pub(super) key: String,
    pub(super) invalidation_keys: Vec<String>,
    pub(super) kind: CacheEntryKind,
    pub(super) subgraph_name: String,
    pub(super) subgraph_request: graphql::Request,
    pub(super) source: CacheKeySource,
    pub(super) cache_control: CacheControl,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) hashed_private_id: Option<String>,
    pub(super) data: serde_json_bytes::Value,
    pub(super) warnings: Vec<Warning>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Warning {
    pub(super) code: String,
    pub(super) links: Vec<Link>,
    pub(super) message: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Link {
    pub(super) url: String,
    pub(super) title: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[cfg_attr(test, derive(PartialEq, Eq, Hash))]
#[serde(rename_all = "camelCase", untagged)]
pub(crate) enum CacheEntryKind {
    Entity {
        typename: String,
        #[serde(rename = "entityKey")]
        entity_key: Object,
    },
    RootFields {
        #[serde(rename = "rootFields")]
        root_fields: Vec<String>,
    },
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[cfg_attr(test, derive(PartialEq, Eq, Hash))]
#[serde(rename_all = "camelCase")]
pub(crate) enum CacheKeySource {
    /// Data fetched from subgraph
    Subgraph,
    /// Data fetched from cache
    Cache,
}

#[cfg(test)]
impl PartialOrd for CacheKeySource {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

#[cfg(test)]
impl Ord for CacheKeySource {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        match (self, other) {
            (CacheKeySource::Subgraph, CacheKeySource::Subgraph) => std::cmp::Ordering::Equal,
            (CacheKeySource::Subgraph, CacheKeySource::Cache) => std::cmp::Ordering::Greater,
            (CacheKeySource::Cache, CacheKeySource::Subgraph) => std::cmp::Ordering::Less,
            (CacheKeySource::Cache, CacheKeySource::Cache) => std::cmp::Ordering::Equal,
        }
    }
}

impl CacheKeyContext {
    pub(super) fn compute_warnings(mut self) -> Self {
        self
    }
}
