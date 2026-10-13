//! Describe primary cache key for both root fields and entities
use std::borrow::Cow;
use std::fmt::Write;

use apollo_json::JsonKind;
use itertools::Itertools;
use serde::Serialize;

use super::plugin::RESPONSE_CACHE_VERSION;
use crate::Context;
use crate::graphql;
use crate::json_ext::Value;
use crate::plugins::authorization::CacheKeyMetadata;
use crate::plugins::response_cache::cache_tag::CacheScope;
use crate::plugins::response_cache::plugin::CONTEXT_CACHE_KEY;
use crate::plugins::response_cache::plugin::REPRESENTATIONS;
use crate::plugins::response_cache::serde_blake3::Blake3Serializer;
use crate::spec::QueryHash;
use crate::spec::TYPENAME;

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
            "version:{RESPONSE_CACHE_VERSION}:{}:{subgraph_name}:type:{graphql_type}:hash:{query_hash}:data:{additional_data_hash}",
            CacheScope::Subgraph.word()
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
    /// The entity's representation. Its `__typename` is left out of the hash,
    /// since `entity_type` already names the type.
    pub(super) representation: &'a Value,
    /// NB: hashed before insertion into this struct, so that the hashed representation can be
    /// reused for all entities in this query
    pub(super) subgraph_query_hash: &'a str,
    pub(super) additional_data_hash: &'a str,
    pub(super) private_id: Option<&'a str>,
}

impl<'a> PrimaryCacheKeyEntity<'a> {
    #[cfg(test)]
    pub(super) fn hash(&mut self) -> String {
        let mut key = String::new();
        self.hash_into(&mut key);
        key
    }

    /// Writes the key into `key`, replacing its contents, so a caller making
    /// many keys can reuse one buffer.
    pub(super) fn hash_into(&mut self, key: &mut String) {
        let Self {
            subgraph_name,
            entity_type,
            subgraph_query_hash,
            additional_data_hash,
            private_id,
            representation,
        } = self;

        let has_fields = representation.object_iter().any(|(key, _)| key != TYPENAME);
        let hashed_representation = has_fields.then(|| {
            let mut digest = blake3::Hasher::new();
            hash_object(&mut digest, representation, Some(TYPENAME));
            digest.finalize().to_hex()
        });
        let hashed_representation = hashed_representation
            .as_ref()
            .map(|hex| hex.as_str())
            .unwrap_or_default();

        // - response cache version: current version of the hash
        // - subgraph name: caching is done per subgraph
        // - type: can invalidate all instances of a type
        // - representation: representation variable value
        // - query hash: invalidate the entry for a specific query and operation name
        // - additional data: separate cache entries depending on info like authorization status
        //
        // Assembled in a buffer of exactly the key's size: `format!` would grow
        // its buffer several times for a key this long, and this runs once per
        // entity.
        let parts = [
            "version:",
            RESPONSE_CACHE_VERSION,
            ":",
            CacheScope::Subgraph.word(),
            ":",
            subgraph_name,
            ":type:",
            entity_type,
            ":representation:",
            hashed_representation,
            ":hash:",
            subgraph_query_hash,
            ":data:",
            additional_data_hash,
        ];
        let private_id = private_id.as_deref();
        let len = parts.iter().map(|part| part.len()).sum::<usize>()
            + private_id.map_or(0, |id| id.len() + 1);
        key.clear();
        key.reserve_exact(len);
        for part in parts {
            key.push_str(part);
        }
        if let Some(private_id) = private_id {
            key.push(':');
            key.push_str(private_id);
        }
    }
}

/// Cache key for connector root field
pub(super) struct ConnectorCacheKeyRoot<'a> {
    pub(super) source_name: &'a str,
    pub(super) graphql_type: &'a str,
    pub(super) operation_hash: &'a str,
    /// Hash of the response-mapping selection this request maps its upstream response through.
    ///
    /// A connector subgraph has a single root field, but one fetch node can request it several
    /// times with aliases (`root_fields` in `plugins/connectors/make_requests.rs` emits one
    /// `ResponseKey::RootField` per field selection). `operation_hash` covers the whole fetch
    /// node's document and `additional_data_hash` covers the field arguments, so two aliases with
    /// equal arguments and different sub-selections would otherwise share one entry. The stored
    /// value is the *mapped* data, already narrowed by the storing request's selection, so the
    /// second alias would read back the wrong shape.
    pub(super) selection_hash: &'a str,
    pub(super) additional_data_hash: &'a str,
    pub(super) private_id: Option<&'a str>,
}

impl<'a> ConnectorCacheKeyRoot<'a> {
    pub(super) fn hash(&self) -> String {
        let Self {
            source_name,
            graphql_type,
            operation_hash,
            selection_hash,
            additional_data_hash,
            private_id,
        } = self;

        let mut key = format!(
            "version:{RESPONSE_CACHE_VERSION}:{}:{source_name}:type:{graphql_type}:hash:{operation_hash}:selection:{selection_hash}:data:{additional_data_hash}",
            CacheScope::Connector.word()
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
    pub(super) representation: &'a Value,
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

        let hashed_representation = if representation.is_empty().unwrap_or(true) {
            String::new()
        } else {
            sort_and_hash_object(representation)
        };

        let mut key = format!(
            "version:{RESPONSE_CACHE_VERSION}:{}:{source_name}:type:{entity_type}:representation:{hashed_representation}:hash:{operation_hash}:data:{additional_data_hash}",
            CacheScope::Connector.word()
        );

        if let Some(private_id) = private_id {
            let _ = write!(&mut key, ":{private_id}");
        }

        key
    }
}

/// Hash an operation document for use as a connector query hash
pub(super) fn hash_operation(operation: &str) -> String {
    hash_str(operation)
}

/// Hash a connector operation together with its source name, for the private-query LRU key.
///
/// The "known private" LRU (`PrivateQueryKey`) is shared across every subgraph and connector
/// source. Folding the source name into the hash partitions that flag per source, so two sources
/// whose entity-fetch operations have byte-identical text cannot poison each other's private flag.
/// Kept separate from the cache key's `operation_hash` (which is intentionally source-independent —
/// the source is already its own cache-key segment).
pub(super) fn hash_operation_scoped(source_name: &str, operation: &str) -> String {
    let mut digest = blake3::Hasher::new();
    digest.update(source_name.as_bytes());
    digest.update(&[0u8; 1][..]);
    digest.update(operation.as_bytes());
    digest.update(&[0u8; 1][..]);
    digest.finalize().to_hex().to_string()
}

/// Hash a connector response-mapping selection for use in a connector root-field cache key.
/// See [`ConnectorCacheKeyRoot::selection_hash`] for why the selection is part of the key.
pub(super) fn hash_selection(selection: &str) -> String {
    hash_str(selection)
}

fn hash_str(value: &str) -> String {
    let mut digest = blake3::Hasher::new();
    digest.update(value.as_bytes());
    digest.update(&[0u8; 1][..]);
    digest.finalize().to_hex().to_string()
}

/// Hash additional data for connector cache keys.
/// Similar to `hash_additional_data` but works with connector `Variables` instead of `graphql::Request`.
///
/// `key_inputs` carries the connector's *referenced* per-request inputs (`$context` values and
/// forwarded `$request.headers` — see `connector_key_inputs`). These shape the upstream HTTP
/// request/response mapping but are NOT part of `variables`, so without hashing them two client
/// requests that produce different upstream requests (e.g. a per-user forwarded header) would
/// collide on the same cache key and leak one user's data to another.
pub(super) fn hash_connector_additional_data(
    source_name: &str,
    variables: &Value,
    context: &Context,
    cache_key: &CacheKeyMetadata,
    key_inputs: &serde_json_bytes::Value,
) -> String {
    let mut hasher = blake3::Hasher::new();

    hash(
        &mut hasher,
        variables
            .object_iter()
            .filter(|(key, _value)| key != REPRESENTATIONS),
    );

    cache_key
        .serialize(Blake3Serializer::new(&mut hasher))
        .expect("this serializer doesn't throw any errors; qed");

    // Referenced $context / $request.headers inputs. Hashed after the auth metadata so equal
    // inputs produce equal keys and any per-user difference partitions them.
    key_inputs
        .serialize(Blake3Serializer::new(&mut hasher))
        .expect("this serializer doesn't throw any errors; qed");

    // Takes value specific for a connector source, if it doesn't exist take value for all
    if let Some(cache_data) = context.get_json_value(CONTEXT_CACHE_KEY) {
        if let Some(v) = cache_data
            .get("connectors")
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

    hash_object(&mut hasher, &body.variables, Some(REPRESENTATIONS));

    cache_key
        .serialize(Blake3Serializer::new(&mut hasher))
        .expect("this serializer doesn't throw any errors; qed");

    // Takes value specific for a subgraph, if it doesn't exist take value for all subgraphs, and if you have data specific for an operation name add it in the hash
    if let Some(cache_data) = context.get_json_value(CONTEXT_CACHE_KEY) {
        if let Some(v) = cache_data
            .get("subgraphs")
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
fn sort_and_hash_object(object: &Value) -> String {
    let mut digest = blake3::Hasher::new();
    hash_object(&mut digest, object, None);
    digest.finalize().to_hex().to_string()
}

/// Hashes the members of `object` other than `skip`, exactly as [`hash`] does.
/// Members already in key order, as they usually are, are hashed as they are
/// visited rather than collected and sorted first.
fn hash_object(state: &mut blake3::Hasher, object: &Value, skip: Option<&str>) {
    let fields = || {
        object
            .object_iter()
            .filter(move |(key, _)| Some(&**key) != skip)
    };
    let mut previous: Option<Cow<'_, str>> = None;
    let in_order = fields().all(|(key, _)| {
        let in_order = previous.as_ref().is_none_or(|previous| *previous <= key);
        previous = Some(key);
        in_order
    });
    if in_order {
        fields().for_each(|(key, value)| hash_member(state, &key, &value));
    } else {
        hash(state, fields());
    }
}

/// Hashes the members of a JSON object. Members are visited in key order, so two
/// objects differing only in key order hash the same; arrays keep their order.
fn hash<'a, I>(state: &mut blake3::Hasher, fields: I)
where
    I: Iterator<Item = (Cow<'a, str>, Value)>,
{
    fields
        .sorted_by(|a, b| a.0.cmp(&b.0))
        .for_each(|(k, v)| hash_member(state, &k, &v));
}

fn hash_member(state: &mut blake3::Hasher, key: &str, value: &Value) {
    state.update(key.as_bytes());
    state.update(":".as_bytes());
    match value.kind() {
        JsonKind::Object => {
            state.update("{".as_bytes());
            hash_object(state, value, None);
            state.update("}".as_bytes());
        }
        JsonKind::String => {
            state.update(value.as_str().unwrap_or_default().as_bytes());
        }
        // A number's text is what serializing it writes, so it is hashed
        // in place rather than through a `String`.
        JsonKind::Number => {
            state.update(value.raw_number().unwrap_or_default().as_bytes());
        }
        JsonKind::Bool => {
            state.update(if value.as_bool() == Some(true) {
                b"true"
            } else {
                b"false"
            });
        }
        JsonKind::Null => {
            state.update(b"null");
        }
        _ => {
            state.update(value.to_string().as_bytes());
        }
    }
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
            apollo_json::json!({
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

    /// The hashing as it was before members were hashed in place: always
    /// sorted through a buffer, scalars other than strings through their
    /// serialized text. Cache keys already in Redis were made this way.
    fn reference_hash(state: &mut blake3::Hasher, object: &Value, skip: Option<&str>) {
        object
            .object_iter()
            .filter(|(key, _)| Some(&**key) != skip)
            .sorted_by(|a, b| a.0.cmp(&b.0))
            .for_each(|(k, v)| {
                state.update(k.as_bytes());
                state.update(":".as_bytes());
                match v.kind() {
                    JsonKind::Object => {
                        state.update("{".as_bytes());
                        reference_hash(state, &v, None);
                        state.update("}".as_bytes());
                    }
                    JsonKind::String => {
                        state.update(v.as_str().unwrap_or_default().as_bytes());
                    }
                    _ => {
                        state.update(v.to_string().as_bytes());
                    }
                }
            });
    }

    #[test]
    fn members_hash_as_before_in_any_order_and_of_every_kind() {
        let documents = [
            r#"{"__typename":"T","a":1,"b":"s","c":true,"d":null,"e":[1,"x",{"z":2}],"f":{"g":1.50,"h":false}}"#,
            r#"{"f":{"h":false,"g":1.50},"e":[1,"x",{"z":2}],"d":null,"c":true,"b":"s","a":1,"__typename":"T"}"#,
            r#"{"id":"1"}"#,
            r#"{"__typename":"T","n":-12e3}"#,
        ];
        for document in documents {
            let value = Value::parse(document.as_bytes().to_vec()).unwrap();
            for skip in [None, Some(TYPENAME)] {
                let mut expected = blake3::Hasher::new();
                reference_hash(&mut expected, &value, skip);
                let mut actual = blake3::Hasher::new();
                hash_object(&mut actual, &value, skip);
                assert_eq!(
                    actual.finalize(),
                    expected.finalize(),
                    "{document} {skip:?}"
                );
            }
        }
    }

    #[test]
    fn entity_key_is_built_as_before() {
        let representation =
            Value::parse(br#"{"__typename":"Product","upc":"1","n":2}"#.to_vec()).unwrap();
        let mut digest = blake3::Hasher::new();
        reference_hash(&mut digest, &representation, Some(TYPENAME));
        let hashed = digest.finalize().to_hex();
        for private_id in [None, Some("private")] {
            let key = PrimaryCacheKeyEntity {
                subgraph_name: "products",
                entity_type: "Product",
                representation: &representation,
                subgraph_query_hash: "query",
                additional_data_hash: "data",
                private_id,
            }
            .hash();
            let mut expected = format!(
                "version:{RESPONSE_CACHE_VERSION}:subgraph:products:type:Product:representation:{hashed}:hash:query:data:data"
            );
            if let Some(private_id) = private_id {
                expected.push(':');
                expected.push_str(private_id);
            }
            assert_eq!(key, expected);
            assert_eq!(key.capacity(), key.len());
        }
    }

    #[test]
    fn top_level_hash_is_order_insensitive() {
        // hash should not vary based on the order that the keys are provided.
        // NB: this doesn't check any nested arrays, that's done in serde_blake3
        let data = apollo_json::json!({"hello": "world", "order": "doesn't matter"});
        let entries: Vec<(Cow<'_, str>, Value)> = data.object_iter().collect();

        let mut hasher = blake3::Hasher::new();
        super::hash(&mut hasher, entries.iter().cloned());
        let value1 = hasher.finalize();

        let mut hasher = blake3::Hasher::new();
        super::hash(&mut hasher, entries.iter().rev().cloned());
        let value2 = hasher.finalize();

        assert_eq!(value1, value2);
        assert_snapshot!(value1);
    }

    #[test]
    fn nested_hash_is_order_sensitive() {
        // hash does vary based on the order that the vec values are provided.
        // NB: I'm not sure if this is intentional, but adding a test for the existing behavior.
        let data = apollo_json::json!({"nested": ["does", "order", "matter"]});
        let value1 = super::sort_and_hash_object(&data);

        let data = apollo_json::json!({"nested": ["order", "does", "matter"]});
        let value2 = super::sort_and_hash_object(&data);

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
            selection_hash: "sel789",
            additional_data_hash: "def456",
            private_id: None,
        };
        let hash = key.hash();
        assert!(hash.starts_with("version:"));
        assert!(hash.contains(":connector:mysubgraph.my_api:"));
        assert!(hash.contains(":type:Query:"));
        assert!(hash.contains(":hash:abc123:"));
        assert!(hash.contains(":selection:sel789:"));
        assert!(hash.contains(":data:def456"));
        assert!(!hash.contains(":subgraph:"));
        assert_snapshot!(hash);
    }

    /// Two aliases of the same root field, same arguments and same fetch-node document, but
    /// different sub-selections: the mapped data stored for one must not be read back for the
    /// other.
    #[test]
    fn connector_root_cache_key_separates_aliased_root_fields() {
        let narrow = ConnectorCacheKeyRoot {
            source_name: "mysubgraph.my_api",
            graphql_type: "Query",
            operation_hash: "abc123",
            selection_hash: &hash_selection("id"),
            additional_data_hash: "def456",
            private_id: None,
        };
        let wide = ConnectorCacheKeyRoot {
            source_name: "mysubgraph.my_api",
            graphql_type: "Query",
            operation_hash: "abc123",
            selection_hash: &hash_selection("id title"),
            additional_data_hash: "def456",
            private_id: None,
        };
        assert_ne!(narrow.hash(), wide.hash());

        // ... while two aliases with the *same* selection still share an entry, which is the
        // whole point of caching them.
        let same_as_narrow = ConnectorCacheKeyRoot {
            source_name: "mysubgraph.my_api",
            graphql_type: "Query",
            operation_hash: "abc123",
            selection_hash: &hash_selection("id"),
            additional_data_hash: "def456",
            private_id: None,
        };
        assert_eq!(narrow.hash(), same_as_narrow.hash());
    }

    #[test]
    fn connector_root_cache_key_with_private_id() {
        let without_private = ConnectorCacheKeyRoot {
            source_name: "mysubgraph.my_api",
            graphql_type: "Query",
            operation_hash: "abc123",
            selection_hash: "sel789",
            additional_data_hash: "def456",
            private_id: None,
        };
        let with_private = ConnectorCacheKeyRoot {
            source_name: "mysubgraph.my_api",
            graphql_type: "Query",
            operation_hash: "abc123",
            selection_hash: "sel789",
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
        let repr = apollo_json::json!({"id": "1"});
        let mut key = ConnectorCacheKeyEntity {
            source_name: "mysubgraph.my_api",
            entity_type: "User",
            representation: &repr,
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
        let repr = crate::graphql::json_object::empty_object();
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
        let repr1 = apollo_json::json!({"id": "1"});
        let repr2 = apollo_json::json!({"id": "2"});
        let mut key1 = ConnectorCacheKeyEntity {
            source_name: "mysubgraph.my_api",
            entity_type: "User",
            representation: &repr1,
            operation_hash: "abc123",
            additional_data_hash: "def456",
            private_id: None,
        };
        let mut key2 = ConnectorCacheKeyEntity {
            source_name: "mysubgraph.my_api",
            entity_type: "User",
            representation: &repr2,
            operation_hash: "abc123",
            additional_data_hash: "def456",
            private_id: None,
        };
        assert_ne!(key1.hash(), key2.hash());
    }

    #[test]
    fn entity_cache_key_leaves_typename_out_of_the_representation_hash() {
        let key = |representation: &Value| {
            PrimaryCacheKeyEntity {
                subgraph_name: "products",
                entity_type: "Product",
                representation,
                subgraph_query_hash: "abc123",
                additional_data_hash: "def456",
                private_id: None,
            }
            .hash()
        };
        let with_typename = apollo_json::json!({"__typename": "Product", "upc": "1"});
        let without = apollo_json::json!({"upc": "1"});
        assert_eq!(key(&with_typename), key(&without));
        assert_eq!(
            key(&apollo_json::json!({"__typename": "Product"})),
            key(&apollo_json::json!({}))
        );
        assert!(key(&without).contains(&format!(
            ":representation:{}:",
            sort_and_hash_object(&without)
        )));
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
    fn hash_operation_scoped_partitions_by_source() {
        let op = "query { user(id: \"1\") { id name } }";
        // Same source + operation is stable; different sources with identical operation text
        // produce different hashes, so they cannot collide in the shared private-query LRU.
        assert_eq!(
            hash_operation_scoped("a.api", op),
            hash_operation_scoped("a.api", op)
        );
        assert_ne!(
            hash_operation_scoped("a.api", op),
            hash_operation_scoped("b.api", op)
        );
        // The source is genuinely part of the input, not merely appended text: a source/operation
        // split must not alias another (the NUL separators guarantee this).
        assert_ne!(
            hash_operation_scoped("a.api", op),
            hash_operation_scoped("a.ap", &format!("i{op}"))
        );
        // Scoped hashes are distinct from the unscoped cache-key hash of the same operation.
        assert_ne!(hash_operation_scoped("a.api", op), hash_operation(op));
    }

    #[test]
    fn hash_connector_additional_data_source_specific() {
        let context = Context::new();
        context.insert_json_value(
            CONTEXT_CACHE_KEY,
            apollo_json::json!({
                "all": { "locale": "en" },
                "connectors": {
                    "source_a": { "foo": "bar" },
                    "source_b": { "baz": "qux" }
                }
            }),
        );
        let vars = apollo_json::json!({"key": "value"});
        let hash_a = hash_connector_additional_data(
            "source_a",
            &vars,
            &context,
            &Default::default(),
            &serde_json_bytes::Value::Null,
        );
        let hash_b = hash_connector_additional_data(
            "source_b",
            &vars,
            &context,
            &Default::default(),
            &serde_json_bytes::Value::Null,
        );
        assert_ne!(hash_a, hash_b);
    }

    #[test]
    fn hash_connector_additional_data_fallback_to_all() {
        let context = Context::new();
        context.insert_json_value(
            CONTEXT_CACHE_KEY,
            apollo_json::json!({
                "all": { "locale": "en" },
                "connectors": {
                    "source_a": { "foo": "bar" }
                }
            }),
        );
        let vars = apollo_json::json!({"key": "value"});
        let hash_unknown_1 = hash_connector_additional_data(
            "unknown_source",
            &vars,
            &context,
            &Default::default(),
            &serde_json_bytes::Value::Null,
        );
        let hash_unknown_2 = hash_connector_additional_data(
            "another_unknown",
            &vars,
            &context,
            &Default::default(),
            &serde_json_bytes::Value::Null,
        );
        assert_eq!(hash_unknown_1, hash_unknown_2);
    }

    #[test]
    fn connector_key_uses_connector_prefix() {
        let connector_key = ConnectorCacheKeyRoot {
            source_name: "test_source",
            graphql_type: "Query",
            operation_hash: "hash",
            selection_hash: "selection",
            additional_data_hash: "data",
            private_id: None,
        };
        assert!(connector_key.hash().contains(":connector:"));
        assert!(!connector_key.hash().contains(":subgraph:"));
    }

    #[test]
    fn hash_connector_additional_data_partitions_on_key_inputs() {
        let context = Context::new();
        let vars = apollo_json::json!({"key": "value"});
        // Two requests identical except for a referenced forwarded-header value must NOT collide.
        let inputs_alice = serde_json_bytes::json!({
            "context": {},
            "headers": { "x-user": ["alice"] }
        });
        let inputs_bob = serde_json_bytes::json!({
            "context": {},
            "headers": { "x-user": ["bob"] }
        });
        let hash_alice = hash_connector_additional_data(
            "src",
            &vars,
            &context,
            &Default::default(),
            &inputs_alice,
        );
        let hash_bob = hash_connector_additional_data(
            "src",
            &vars,
            &context,
            &Default::default(),
            &inputs_bob,
        );
        assert_ne!(
            hash_alice, hash_bob,
            "different forwarded-header values must partition the cache key"
        );

        // Identical key inputs must produce identical hashes (cache still works within a user).
        let hash_alice_again = hash_connector_additional_data(
            "src",
            &vars,
            &context,
            &Default::default(),
            &inputs_alice,
        );
        assert_eq!(hash_alice, hash_alice_again);
    }

    #[test]
    fn hash_connector_additional_data_is_deterministic_for_empty_or_null_inputs() {
        // `key_inputs` is `Value::Null` or an empty object depending on the caller
        // (`connector_key_inputs` never produces `Null` in production, but callers reaching this
        // function directly might pass either). Neither shape needs to hash the same as the
        // other — only that each hashes stably across calls, so identical requests keep getting
        // identical cache keys.
        let context = Context::new();
        let vars = apollo_json::json!({"key": "value"});
        let empty = serde_json_bytes::json!({ "context": {}, "headers": {} });
        let with_empty =
            hash_connector_additional_data("src", &vars, &context, &Default::default(), &empty);
        let with_null = hash_connector_additional_data(
            "src",
            &vars,
            &context,
            &Default::default(),
            &serde_json_bytes::Value::Null,
        );
        // They need not be equal to each other, but each must be stable; assert determinism.
        assert_eq!(
            with_empty,
            hash_connector_additional_data("src", &vars, &context, &Default::default(), &empty,)
        );
        assert_eq!(
            with_null,
            hash_connector_additional_data(
                "src",
                &vars,
                &context,
                &Default::default(),
                &serde_json_bytes::Value::Null,
            )
        );
    }
}
