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
        let additional_data_hash =
            hash_additional_data(subgraph_name, body, context, auth_cache_key_metadata);

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
        } = self;

        let hashed_representation = if representation.is_empty() {
            String::new()
        } else {
            sort_and_hash_object(representation)
        };

        // - response cache version: current version of the hash
        // - subgraph name: caching is done per subgraph
        // - type: can invalidate all instances of a type
        // - representation: representation variable value
        // - query hash: invalidate the entry for a specific query and operation name
        // - additional data: separate cache entries depending on info like authorization status
        let mut key = format!(
            "version:{RESPONSE_CACHE_VERSION}:subgraph:{subgraph_name}:type:{entity_type}:representation:{hashed_representation}:hash:{subgraph_query_hash}:data:{additional_data_hash}"
        );

        if let Some(private_id) = private_id {
            let _ = write!(&mut key, ":{private_id}");
        }

        key
    }
}

/// Cache key for connector root field
pub(super) struct ConnectorCacheKeyRoot<'a> {
    pub(super) source_name: &'a str,
    pub(super) graphql_type: &'a str,
    pub(super) operation_hash: &'a str,
    pub(super) additional_data_hash: &'a str,
    pub(super) private_id: Option<&'a str>,
}

impl<'a> ConnectorCacheKeyRoot<'a> {
    pub(super) fn hash(&self) -> String {
        let Self {
            source_name,
            graphql_type,
            operation_hash,
            additional_data_hash,
            private_id,
        } = self;

        let mut key = format!(
            "version:{RESPONSE_CACHE_VERSION}:connector:{source_name}:type:{graphql_type}:hash:{operation_hash}:data:{additional_data_hash}"
        );
        if let Some(private_id) = private_id {
            let _ = write!(&mut key, ":{private_id}");
        }

        key
    }
}

/// Cache key for a connector entity
pub(super) struct ConnectorCacheKeyEntity<'a> {
    pub(super) source_name: &'a str,
    pub(super) entity_type: &'a str,
    pub(super) representation: &'a Map<ByteString, Value>,
    pub(super) operation_hash: &'a str,
    pub(super) additional_data_hash: &'a str,
    pub(super) private_id: Option<&'a str>,
}

impl<'a> ConnectorCacheKeyEntity<'a> {
    pub(super) fn hash(&mut self) -> String {
        let Self {
            source_name,
            entity_type,
            operation_hash,
            additional_data_hash,
            private_id,
            representation,
        } = self;

        let hashed_representation = if representation.is_empty() {
            String::new()
        } else {
            sort_and_hash_object(representation)
        };

        let mut key = format!(
            "version:{RESPONSE_CACHE_VERSION}:connector:{source_name}:type:{entity_type}:representation:{hashed_representation}:hash:{operation_hash}:data:{additional_data_hash}"
        );

        if let Some(private_id) = private_id {
            let _ = write!(&mut key, ":{private_id}");
        }

        key
    }
}

/// Hash an operation document for use as a connector query hash
pub(super) fn hash_operation(operation: &str) -> String {
    let mut digest = blake3::Hasher::new();
    digest.update(operation.as_bytes());
    digest.update(&[0u8; 1][..]);
    digest.finalize().to_hex().to_string()
}

/// Hash additional data for connector cache keys.
/// Similar to `hash_additional_data` but works with connector `Variables` instead of `graphql::Request`.
pub(super) fn hash_connector_additional_data(
    source_name: &str,
    variables: &Object,
    context: &Context,
    cache_key: &CacheKeyMetadata,
) -> String {
    let mut hasher = blake3::Hasher::new();

    let repr_key = ByteString::from(REPRESENTATIONS);
    hash(
        &mut hasher,
        variables.iter().filter(|(key, _value)| key != &&repr_key),
    );

    cache_key
        .serialize(Blake3Serializer::new(&mut hasher))
        .expect("this serializer doesn't throw any errors; qed");

    // Takes value specific for a connector source, if it doesn't exist take value for all
    if let Ok(Some(cache_data)) = context.get::<&str, Object>(CONTEXT_CACHE_KEY) {
        if let Some(v) = cache_data
            .get("connectors")
            .and_then(|s| s.as_object())
            .and_then(|connector_data| connector_data.get(source_name))
        {
            v.serialize(Blake3Serializer::new(&mut hasher))
                .expect("this serializer doesn't throw any errors; qed");
        } else if let Some(v) = cache_data.get("all") {
            v.serialize(Blake3Serializer::new(&mut hasher))
                .expect("this serializer doesn't throw any errors; qed");
        }
    }

    hasher.finalize().to_hex().to_string()
}

/// Hash subgraph query
pub(super) fn hash_query(query_hash: &QueryHash) -> String {
    let mut digest = blake3::Hasher::new();
    digest.update(query_hash.as_bytes());
    digest.update(&[0u8; 1][..]);

    digest.finalize().to_hex().to_string()
}

pub(super) fn hash_additional_data(
    subgraph_name: &str,
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

    // Takes value specific for a subgraph, if it doesn't exist take value for all subgraphs, and if you have data specific for an operation name add it in the hash
    if let Ok(Some(cache_data)) = context.get::<&str, Object>(CONTEXT_CACHE_KEY) {
        if let Some(v) = cache_data
            .get("subgraphs")
            .and_then(|s| s.as_object())
            .and_then(|subgraph_data| subgraph_data.get(subgraph_name))
        {
            v.serialize(Blake3Serializer::new(&mut hasher))
                .expect("this serializer doesn't throw any errors; qed");
        } else if let Some(v) = cache_data.get("all") {
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

// Order-insensitive structural hash of a map, ie a representation or entity key
fn sort_and_hash_object(object: &Map<ByteString, Value>) -> String {
    let mut digest = blake3::Hasher::new();
    hash(&mut digest, object.iter());
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

#[cfg(test)]
mod tests {
    use insta::assert_snapshot;

    use super::*;

    #[test]
    fn test_hash_additional_data() {
        let context = Context::new();
        context.insert_json_value(
            CONTEXT_CACHE_KEY,
            serde_json_bytes::json!({
                "all": {
                  "locale": "be"
                },
                "subgraphs": {
                    "test": {
                        "foo": "bar"
                    },
                    "test_2": {
                        "bar": "foo"
                    }
                }
            }),
        );
        let hashed_data = hash_additional_data(
            "test",
            &graphql::Request::builder()
                .query("{ me { name } }")
                .variable("key", "value")
                .build(),
            &context,
            &Default::default(),
        );
        let hashed_data_2 = hash_additional_data(
            "test_2",
            &graphql::Request::builder()
                .query("{ me { name } }")
                .variable("key", "value")
                .build(),
            &context,
            &Default::default(),
        );
        // Because it takes different data from context
        assert!(hashed_data != hashed_data_2);

        let hashed_data_3 = hash_additional_data(
            "test_3",
            &graphql::Request::builder()
                .query("{ me { name } }")
                .variable("key", "value")
                .build(),
            &context,
            &Default::default(),
        );
        let hashed_data_4 = hash_additional_data(
            "test_4",
            &graphql::Request::builder()
                .query("{ me { name } }")
                .variable("key", "value")
                .build(),
            &context,
            &Default::default(),
        );
        // Because it takes the same data from context `all`
        assert_eq!(hashed_data_3, hashed_data_4);
    }

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
        let value1 = super::sort_and_hash_object(data.as_object().unwrap());

        let data = serde_json_bytes::json!({"nested": ["order", "does", "matter"]});
        let value2 = super::sort_and_hash_object(data.as_object().unwrap());

        assert_ne!(value1, value2);
        assert_snapshot!(value1);
        assert_snapshot!(value2);
    }

    #[test]
    fn connector_root_cache_key_format() {
        let key = ConnectorCacheKeyRoot {
            source_name: "mysubgraph.my_api",
            graphql_type: "Query",
            operation_hash: "abc123",
            additional_data_hash: "def456",
            private_id: None,
        };
        let hash = key.hash();
        assert!(hash.starts_with("version:"));
        assert!(hash.contains(":connector:mysubgraph.my_api:"));
        assert!(hash.contains(":type:Query:"));
        assert!(hash.contains(":hash:abc123:"));
        assert!(hash.contains(":data:def456"));
        assert!(!hash.contains(":subgraph:"));
        assert_snapshot!(hash);
    }

    #[test]
    fn connector_root_cache_key_with_private_id() {
        let without_private = ConnectorCacheKeyRoot {
            source_name: "mysubgraph.my_api",
            graphql_type: "Query",
            operation_hash: "abc123",
            additional_data_hash: "def456",
            private_id: None,
        };
        let with_private = ConnectorCacheKeyRoot {
            source_name: "mysubgraph.my_api",
            graphql_type: "Query",
            operation_hash: "abc123",
            additional_data_hash: "def456",
            private_id: Some("user_hash_xyz"),
        };
        let hash_without = without_private.hash();
        let hash_with = with_private.hash();
        assert!(hash_with.ends_with(":user_hash_xyz"));
        assert!(!hash_without.contains("user_hash_xyz"));
    }

    #[test]
    fn connector_entity_cache_key_format() {
        let repr = serde_json_bytes::json!({"id": "1"});
        let mut key = ConnectorCacheKeyEntity {
            source_name: "mysubgraph.my_api",
            entity_type: "User",
            representation: repr.as_object().unwrap(),
            operation_hash: "abc123",
            additional_data_hash: "def456",
            private_id: None,
        };
        let hash = key.hash();
        assert!(hash.contains(":connector:mysubgraph.my_api:"));
        assert!(hash.contains(":type:User:"));
        assert!(hash.contains(":representation:"));
        assert!(!hash.contains(":subgraph:"));
        assert_snapshot!(hash);
    }

    #[test]
    fn connector_entity_cache_key_empty_representation() {
        let repr = serde_json_bytes::Map::new();
        let mut key = ConnectorCacheKeyEntity {
            source_name: "mysubgraph.my_api",
            entity_type: "User",
            representation: &repr,
            operation_hash: "abc123",
            additional_data_hash: "def456",
            private_id: None,
        };
        let hash = key.hash();
        assert!(hash.contains(":representation::hash:"));
    }

    #[test]
    fn connector_entity_cache_key_representation_changes_hash() {
        let repr1 = serde_json_bytes::json!({"id": "1"});
        let repr2 = serde_json_bytes::json!({"id": "2"});
        let mut key1 = ConnectorCacheKeyEntity {
            source_name: "mysubgraph.my_api",
            entity_type: "User",
            representation: repr1.as_object().unwrap(),
            operation_hash: "abc123",
            additional_data_hash: "def456",
            private_id: None,
        };
        let mut key2 = ConnectorCacheKeyEntity {
            source_name: "mysubgraph.my_api",
            entity_type: "User",
            representation: repr2.as_object().unwrap(),
            operation_hash: "abc123",
            additional_data_hash: "def456",
            private_id: None,
        };
        assert_ne!(key1.hash(), key2.hash());
    }

    #[test]
    fn hash_operation_deterministic() {
        let hash1 = hash_operation("query { users { id name } }");
        let hash2 = hash_operation("query { users { id name } }");
        assert_eq!(hash1, hash2);
        assert_snapshot!(hash1);
    }

    #[test]
    fn hash_operation_different_input() {
        let hash1 = hash_operation("query { users { id name } }");
        let hash2 = hash_operation("query { posts { id title } }");
        assert_ne!(hash1, hash2);
    }

    #[test]
    fn hash_connector_additional_data_source_specific() {
        let context = Context::new();
        context.insert_json_value(
            CONTEXT_CACHE_KEY,
            serde_json_bytes::json!({
                "all": { "locale": "en" },
                "connectors": {
                    "source_a": { "foo": "bar" },
                    "source_b": { "baz": "qux" }
                }
            }),
        );
        let vars = serde_json_bytes::json!({"key": "value"});
        let hash_a = hash_connector_additional_data(
            "source_a",
            vars.as_object().unwrap(),
            &context,
            &Default::default(),
        );
        let hash_b = hash_connector_additional_data(
            "source_b",
            vars.as_object().unwrap(),
            &context,
            &Default::default(),
        );
        assert_ne!(hash_a, hash_b);
    }

    #[test]
    fn hash_connector_additional_data_fallback_to_all() {
        let context = Context::new();
        context.insert_json_value(
            CONTEXT_CACHE_KEY,
            serde_json_bytes::json!({
                "all": { "locale": "en" },
                "connectors": {
                    "source_a": { "foo": "bar" }
                }
            }),
        );
        let vars = serde_json_bytes::json!({"key": "value"});
        let hash_unknown_1 = hash_connector_additional_data(
            "unknown_source",
            vars.as_object().unwrap(),
            &context,
            &Default::default(),
        );
        let hash_unknown_2 = hash_connector_additional_data(
            "another_unknown",
            vars.as_object().unwrap(),
            &context,
            &Default::default(),
        );
        assert_eq!(hash_unknown_1, hash_unknown_2);
    }

    #[test]
    fn connector_key_uses_connector_prefix() {
        let connector_key = ConnectorCacheKeyRoot {
            source_name: "test_source",
            graphql_type: "Query",
            operation_hash: "hash",
            additional_data_hash: "data",
            private_id: None,
        };
        assert!(connector_key.hash().contains(":connector:"));
        assert!(!connector_key.hash().contains(":subgraph:"));
    }
}
