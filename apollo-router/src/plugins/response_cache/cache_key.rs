//! Describe primary cache key for both root fields and entities
use std::fmt::Write;

use itertools::Itertools;
use serde::Serialize;
use serde_json_bytes::ByteString;
use serde_json_bytes::Map;
use serde_json_bytes::Value;

use super::plugin::RESPONSE_CACHE_VERSION;
use crate::Context;
use crate::graphql;
use crate::json_ext::Object;
use crate::plugins::authorization::CacheKeyMetadata;
use crate::plugins::response_cache::plugin::CONTEXT_CACHE_KEY;
use crate::plugins::response_cache::plugin::REPRESENTATIONS;
use crate::plugins::response_cache::serde_blake3::Blake3Serializer;
use crate::spec::QueryHash;

/// Cache key for root field
pub(super) struct PrimaryCacheKeyRoot<'a> {
    pub(super) subgraph_name: &'a str,
    pub(super) graphql_type: &'a str,
    pub(super) subgraph_query_hash: &'a QueryHash,
    pub(super) body: &'a graphql::Request,
    pub(super) context: &'a Context,
    pub(super) auth_cache_key_metadata: &'a CacheKeyMetadata,
    pub(super) private_id: Option<&'a str>,
}

impl<'a> PrimaryCacheKeyRoot<'a> {
    pub(super) fn hash(&self) -> String {
        let Self {
            subgraph_name,
            graphql_type,
            subgraph_query_hash,
            body,
            context,
            auth_cache_key_metadata,
            private_id,
        } = self;

        let query_hash = hash_query(subgraph_query_hash);
        let additional_data_hash = hash_additional_data(body, context, auth_cache_key_metadata);

        // - response cache version: current version of the hash
        // - subgraph name: subgraph name
        // - entity type: entity type
        // - query hash: specific query and operation name
        // - additional data: separate cache entries depending on info like authorization status
        let mut key = format!(
            "version:{RESPONSE_CACHE_VERSION}:subgraph:{subgraph_name}:type:{graphql_type}:hash:{query_hash}:data:{additional_data_hash}"
        );
        if let Some(private_id) = private_id {
            let _ = write!(&mut key, ":{private_id}");
        }

        key
    }
}

/// Cache key for an entity
pub(super) struct PrimaryCacheKeyEntity<'a> {
    pub(super) subgraph_name: &'a str,
    pub(super) entity_type: &'a str,
    pub(super) representation: &'a Map<ByteString, Value>,
    pub(super) entity_key: &'a Map<ByteString, Value>,
    /// NB: hashed before insertion into this struct, so that the hashed representation can be reused for all entities in this query
    pub(super) subgraph_query_hash: &'a str,
    pub(super) additional_data_hash: &'a str,
    pub(super) private_id: Option<&'a str>,
}

impl<'a> PrimaryCacheKeyEntity<'a> {
    pub(super) fn hash(&mut self) -> String {
        let Self {
            subgraph_name,
            entity_type,
            subgraph_query_hash,
            additional_data_hash,
            private_id,
            representation,
            entity_key,
        } = self;

        let hashed_representation = if representation.is_empty() {
            String::new()
        } else {
            hash_representation(representation)
        };
        let hashed_entity_key = hash_entity_key(entity_key);

        // - response cache version: current version of the hash
        // - subgraph name: caching is done per subgraph
        // - type: can invalidate all instances of a type
        // - entity key: invalidate a specific entity
        // - representation: representation variable value
        // - query hash: invalidate the entry for a specific query and operation name
        // - additional data: separate cache entries depending on info like authorization status
        let mut key = format!(
            "version:{RESPONSE_CACHE_VERSION}:subgraph:{subgraph_name}:type:{entity_type}:entity:{hashed_entity_key}:representation:{hashed_representation}:hash:{subgraph_query_hash}:data:{additional_data_hash}"
        );

        if let Some(private_id) = private_id {
            let _ = write!(&mut key, ":{private_id}");
        }

        key
    }
}

/// Hash subgraph query
pub(super) fn hash_query(query_hash: &QueryHash) -> String {
    let mut digest = blake3::Hasher::new();
    digest.update(query_hash.as_bytes());
    digest.update(&[0u8; 1][..]);

    digest.finalize().to_hex().to_string()
}

pub(super) fn hash_additional_data(
    body: &graphql::Request,
    context: &Context,
    cache_key: &CacheKeyMetadata,
) -> String {
    let mut hasher = blake3::Hasher::new();

    let repr_key = ByteString::from(REPRESENTATIONS);
    hash(
        &mut hasher,
        body.variables
            .iter()
            .filter(|(key, _value)| key != &&repr_key),
    );

    cache_key
        .serialize(Blake3Serializer::new(&mut hasher))
        .expect("this serializer doesn't throw any errors; qed");

    if let Ok(Some(cache_data)) = context.get::<&str, Object>(CONTEXT_CACHE_KEY) {
        if let Some(v) = cache_data.get("all") {
            v.serialize(Blake3Serializer::new(&mut hasher))
                .expect("this serializer doesn't throw any errors; qed");
        }
        if let Some(v) = body
            .operation_name
            .as_ref()
            .and_then(|op| cache_data.get(op.as_str()))
        {
            v.serialize(Blake3Serializer::new(&mut hasher))
                .expect("this serializer doesn't throw any errors; qed");
        }
    }

    hasher.finalize().to_hex().to_string()
}

// Order-insensitive structural hash of the representation value
pub(super) fn hash_representation(
    representation: &serde_json_bytes::Map<ByteString, Value>,
) -> String {
    let mut digest = blake3::Hasher::new();

    hash(&mut digest, representation.iter());

    digest.finalize().to_hex().to_string()
}

/// Hashes elements of a serde_json_bytes::Value::Object when yielded via `map.iter()`.
fn hash<'a, I>(state: &mut blake3::Hasher, fields: I)
where
    I: Iterator<Item = (&'a ByteString, &'a Value)>,
{
    fields.sorted_by(|a, b| a.0.cmp(b.0)).for_each(|(k, v)| {
        state.update(k.as_str().as_bytes());
        state.update(":".as_bytes());
        match v {
            Value::Object(obj) => {
                state.update("{".as_bytes());
                hash(state, obj.iter());
                state.update("}".as_bytes());
            }
            Value::String(s) => {
                state.update(s.as_str().as_bytes());
            }
            _ => {
                state.update(serde_json::to_string(v).unwrap().as_bytes());
            }
        }
    });
}

// Only hash the list of entity keys
pub(super) fn hash_entity_key(
    entity_keys: &serde_json_bytes::Map<ByteString, serde_json_bytes::Value>,
) -> String {
    tracing::trace!("entity keys: {entity_keys:?}");
    // We have to hash the representation because it can contains PII
    hash_representation(entity_keys)
}

#[cfg(test)]
mod tests {
    use insta::assert_snapshot;

    #[test]
    fn top_level_hash_is_order_insensitive() {
        // hash should not vary based on the order that the keys are provided.
        // NB: this doesn't check any nested arrays, that's done in serde_blake3
        let data = serde_json_bytes::json!({"hello": "world", "order": "doesn't matter"});
        let data_obj = data.as_object().unwrap();

        let mut hasher = blake3::Hasher::new();
        super::hash(&mut hasher, data_obj.iter());
        let value1 = hasher.finalize();

        let mut hasher = blake3::Hasher::new();
        super::hash(&mut hasher, data_obj.iter().rev());
        let value2 = hasher.finalize();

        assert_eq!(value1, value2);
        assert_snapshot!(value1);
    }

    #[test]
    fn nested_hash_is_order_sensitive() {
        // hash does vary based on the order that the vec values are provided.
        // NB: I'm not sure if this is intentional, but adding a test for the existing behavior.
        let data = serde_json_bytes::json!({"nested": ["does", "order", "matter"]});
        let data_obj = data.as_object().unwrap();

        let mut hasher = blake3::Hasher::new();
        super::hash(&mut hasher, data_obj.iter());
        let value1 = hasher.finalize();

        let data = serde_json_bytes::json!({"nested": ["order", "does", "matter"]});
        let data_obj = data.as_object().unwrap();

        let mut hasher = blake3::Hasher::new();
        super::hash(&mut hasher, data_obj.iter());
        let value2 = hasher.finalize();

        assert_ne!(value1, value2);
        assert_snapshot!(value1);
        assert_snapshot!(value2);
    }
}
