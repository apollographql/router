//! Cache key generation utilities for connectors.

use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::collections::hash_map::DefaultHasher;
use std::hash::Hash;
use std::hash::Hasher;

use super::http_json_transport::TransportRequest;
use super::key::ResponseKey;

/// Cache key for connector requests
#[derive(Debug, Clone)]
pub enum CacheKey {
    /// Individual cache keys for entity fetches - one per entity
    Entities(Vec<String>),
    /// Combined cache key for root field requests (pre-combined hash)
    Root(String),
}

/// Cache policy for connector responses
#[derive(Debug, Clone)]
pub enum CachePolicy {
    /// Individual cache policies for entity fetches - one per entity
    Entities(Vec<http::HeaderMap>),
    /// Cache policies for root field requests - consumer can combine as needed
    Roots(Vec<http::HeaderMap>),
}

/// Components of a request that should be included in a cache key
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CacheKeyComponents {
    /// The subgraph name for uniqueness across subgraphs
    pub subgraph_name: String,
    /// HTTP method (GET, POST, etc.)
    pub method: String,
    /// The request URI with interpolated values
    pub uri: String,
    /// Relevant headers that affect the response (sorted for determinism)
    pub headers: BTreeMap<String, String>,
    /// The request body
    pub body: String,
}

/// Extract cache key components from transport request
pub fn extract_cache_components(
    subgraph_name: &str,
    transport_request: &TransportRequest,
) -> CacheKeyComponents {
    match transport_request {
        TransportRequest::Http(http_req) => {
            extract_http_cache_components(subgraph_name, &http_req.inner)
        }
    }
}

/// Extract cache key components from HTTP request
fn extract_http_cache_components(
    subgraph_name: &str,
    req: &http::Request<String>,
) -> CacheKeyComponents {
    // Include relevant headers (sorted for determinism)
    // Only include non-sensitive headers that affect the response
    let headers: BTreeMap<String, String> = req
        .headers()
        .iter()
        .filter_map(|(name, value)| {
            let name_str = name.as_str().to_lowercase();
            // Include content-type and custom headers, exclude auth headers
            let include = name_str.starts_with("x-")
                || name_str == "content-type"
                || name_str == "accept"
                || name_str == "user-agent";

            if include {
                value.to_str().ok().map(|v| (name_str, v.to_string()))
            } else {
                None
            }
        })
        .collect();

    CacheKeyComponents {
        subgraph_name: subgraph_name.to_string(),
        method: req.method().as_str().to_string(),
        uri: req.uri().to_string(),
        headers,
        body: req.body().clone(),
    }
}

impl CacheKeyComponents {
    /// Serialize components for hashing in a deterministic way
    pub fn to_hash_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::new();

        // Subgraph name
        bytes.extend_from_slice(self.subgraph_name.as_bytes());
        bytes.push(0); // separator

        // HTTP method
        bytes.extend_from_slice(self.method.as_bytes());
        bytes.push(0); // separator

        // URI
        bytes.extend_from_slice(self.uri.as_bytes());
        bytes.push(0); // separator

        // Headers (already sorted by BTreeMap)
        for (name, value) in &self.headers {
            bytes.extend_from_slice(name.as_bytes());
            bytes.push(0); // separator
            bytes.extend_from_slice(value.as_bytes());
            bytes.push(0); // separator
        }

        // Body
        bytes.extend_from_slice(self.body.as_bytes());

        bytes
    }
}

/// Combine multiple cache key components for root field requests with aliases
/// This creates a single deterministic hash from all the request variations
pub fn combine_cache_components(components: &[CacheKeyComponents]) -> Vec<u8> {
    if components.is_empty() {
        return Vec::new();
    }

    let mut combined = Vec::new();

    // Start with the subgraph name (should be the same for all)
    if let Some(first) = components.first() {
        combined.extend_from_slice(first.subgraph_name.as_bytes());
        combined.push(0); // separator
    }

    // Collect all unique methods, URIs, headers, and bodies in sorted order
    let methods: BTreeSet<_> = components.iter().map(|c| &c.method).collect();
    let uris: BTreeSet<_> = components.iter().map(|c| &c.uri).collect();
    let mut all_headers: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let bodies: BTreeSet<_> = components.iter().map(|c| &c.body).collect();

    // Aggregate headers
    for component in components {
        for (name, value) in &component.headers {
            all_headers
                .entry(name.clone())
                .or_default()
                .insert(value.clone());
        }
    }

    // Add methods
    combined.extend_from_slice(b"methods");
    combined.push(0);
    for method in methods {
        combined.extend_from_slice(method.as_bytes());
        combined.push(0);
    }

    // Add URIs
    combined.extend_from_slice(b"uris");
    combined.push(0);
    for uri in uris {
        combined.extend_from_slice(uri.as_bytes());
        combined.push(0);
    }

    // Add aggregated headers
    combined.extend_from_slice(b"headers");
    combined.push(0);
    for (name, values) in all_headers {
        combined.extend_from_slice(name.as_bytes());
        combined.push(0);
        for value in values {
            combined.extend_from_slice(value.as_bytes());
            combined.push(0);
        }
    }

    // Add bodies
    combined.extend_from_slice(b"bodies");
    combined.push(0);
    for body in bodies {
        combined.extend_from_slice(body.as_bytes());
        combined.push(0);
    }

    combined
}

/// Create appropriate CacheKey variant based on request types
pub fn create_cache_key(
    requests: &[(&ResponseKey, &TransportRequest)],
    subgraph_name: &str,
) -> CacheKey {
    if requests.is_empty() {
        return CacheKey::Entities(Vec::new());
    }

    // Check if all requests are root fields
    let all_root_fields = requests
        .iter()
        .any(|(key, _)| matches!(key, ResponseKey::RootField { .. }));

    if all_root_fields {
        // For root fields, combine all cache components into a single hash
        let components: Vec<CacheKeyComponents> = requests
            .iter()
            .map(|(_, transport_req)| extract_cache_components(subgraph_name, transport_req))
            .collect();

        let combined_bytes = combine_cache_components(&components);
        let mut hasher = DefaultHasher::new();
        combined_bytes.hash(&mut hasher);

        let combined_key = format!("connector:v1:{:x}", hasher.finish());
        CacheKey::Root(combined_key)
    } else {
        // For entities, create individual cache keys
        // For batch entities, we need to duplicate keys based on batch size
        let mut individual_keys = Vec::new();
        
        for (key, transport_req) in requests.iter() {
            let components = extract_cache_components(subgraph_name, transport_req);
            let mut hasher = DefaultHasher::new();
            components.to_hash_bytes().hash(&mut hasher);
            let cache_key = format!("connector:v1:{:x}", hasher.finish());
            
            match key {
                ResponseKey::BatchEntity { inputs, .. } => {
                    // Duplicate the key for each entity in the batch
                    let batch_size = inputs.batch.len();
                    for _ in 0..batch_size {
                        individual_keys.push(cache_key.clone());
                    }
                }
                _ => {
                    // For non-batch entities, just add the key once
                    individual_keys.push(cache_key);
                }
            }
        }

        CacheKey::Entities(individual_keys)
    }
}

/// Create appropriate CachePolicy variant based on request types
pub fn create_cache_policy_from_keys(
    keys: &[ResponseKey],
    response_policies: Vec<http::HeaderMap>,
) -> CachePolicy {
    if keys.is_empty() {
        return CachePolicy::Entities(Vec::new());
    }

    // Check if all requests are root fields
    let all_root_fields = keys
        .iter()
        .any(|key| matches!(key, ResponseKey::RootField { .. }));

    if all_root_fields {
        CachePolicy::Roots(response_policies)
    } else {
        // For batch entities, we need to duplicate policies based on batch size
        let mut expanded_policies = Vec::new();
        
        for (key, policy) in keys.iter().zip(response_policies.into_iter()) {
            match key {
                ResponseKey::BatchEntity { inputs, .. } => {
                    // Duplicate the policy for each entity in the batch
                    let batch_size = inputs.batch.len();
                    for _ in 0..batch_size {
                        expanded_policies.push(policy.clone());
                    }
                }
                _ => {
                    // For non-batch entities, just add the policy once
                    expanded_policies.push(policy);
                }
            }
        }
        
        CachePolicy::Entities(expanded_policies)
    }
}

/// Create appropriate CachePolicy variant based on request types
pub fn create_cache_policy(
    requests: &[(&ResponseKey, &TransportRequest)],
    response_policies: Vec<http::HeaderMap>,
) -> CachePolicy {
    let keys: Vec<&ResponseKey> = requests.iter().map(|(key, _)| *key).collect();
    create_cache_policy_from_keys(
        &keys.iter().map(|k| (*k).clone()).collect::<Vec<_>>(),
        response_policies,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connectors::runtime::http_json_transport::HttpRequest;
    use crate::connectors::runtime::inputs::RequestInputs;
    use serde_json_bytes::ByteString;
    use serde_json_bytes::Map;
    use serde_json_bytes::Value;

    #[test]
    fn test_extract_cache_components() {
        let http_req = http::Request::builder()
            .method("GET")
            .uri("https://api.example.com/users/123")
            .header("content-type", "application/json")
            .header("x-api-key", "secret")
            .header("authorization", "Bearer token") // Should be excluded
            .body("{}".to_string())
            .unwrap();

        let transport_req = TransportRequest::Http(HttpRequest {
            inner: http_req,
            debug: (None, vec![]),
        });

        let components = extract_cache_components("test-subgraph", &transport_req);

        assert_eq!(components.subgraph_name, "test-subgraph");
        assert_eq!(components.method, "GET");
        assert_eq!(components.uri, "https://api.example.com/users/123");
        assert_eq!(components.body, "{}");

        // Should include content-type and x-api-key but not authorization
        assert_eq!(components.headers.len(), 2);
        assert!(components.headers.contains_key("content-type"));
        assert!(components.headers.contains_key("x-api-key"));
        assert!(!components.headers.contains_key("authorization"));
    }

    #[test]
    fn test_cache_components_deterministic_bytes() {
        let components = CacheKeyComponents {
            subgraph_name: "test".to_string(),
            method: "GET".to_string(),
            uri: "https://api.example.com/test".to_string(),
            headers: [
                ("content-type".to_string(), "application/json".to_string()),
                ("x-api-key".to_string(), "secret".to_string()),
            ]
            .into_iter()
            .collect(),
            body: "{}".to_string(),
        };

        let bytes1 = components.to_hash_bytes();
        let bytes2 = components.to_hash_bytes();

        assert_eq!(bytes1, bytes2);
    }

    #[test]
    fn test_different_subgraphs_different_components() {
        let http_req = http::Request::builder()
            .method("GET")
            .uri("https://api.example.com/users/123")
            .body("{}".to_string())
            .unwrap();

        let transport_req = TransportRequest::Http(HttpRequest {
            inner: http_req,
            debug: (None, vec![]),
        });

        let components1 = extract_cache_components("subgraph1", &transport_req);
        let components2 = extract_cache_components("subgraph2", &transport_req);

        assert_ne!(components1, components2);
        assert_ne!(components1.to_hash_bytes(), components2.to_hash_bytes());
    }

    #[test]
    fn test_combine_cache_components_empty() {
        let result = combine_cache_components(&[]);
        assert_eq!(result, Vec::<u8>::new());
    }

    #[test]
    fn test_combine_cache_components_single() {
        let component = CacheKeyComponents {
            subgraph_name: "test".to_string(),
            method: "GET".to_string(),
            uri: "https://api.example.com/foo".to_string(),
            headers: BTreeMap::new(),
            body: "{}".to_string(),
        };

        let result = combine_cache_components(&[component]);

        // Should contain the subgraph name
        assert!(result.starts_with(b"test"));
        // Should contain all the markers
        assert!(result.windows(7).any(|w| w == b"methods"));
        assert!(result.windows(4).any(|w| w == b"uris"));
        assert!(result.windows(7).any(|w| w == b"headers"));
        assert!(result.windows(6).any(|w| w == b"bodies"));
    }

    #[test]
    fn test_combine_cache_components_multiple_aliases() {
        // Simulating: { foo(bar: "a") alias: foo(bar: "b") }
        let component1 = CacheKeyComponents {
            subgraph_name: "test".to_string(),
            method: "GET".to_string(),
            uri: "https://api.example.com/foo?bar=a".to_string(),
            headers: BTreeMap::new(),
            body: "{\"bar\":\"a\"}".to_string(),
        };

        let component2 = CacheKeyComponents {
            subgraph_name: "test".to_string(),
            method: "GET".to_string(),
            uri: "https://api.example.com/foo?bar=b".to_string(),
            headers: BTreeMap::new(),
            body: "{\"bar\":\"b\"}".to_string(),
        };

        let result = combine_cache_components(&[component1.clone(), component2.clone()]);

        // Should contain both URIs
        let result_str = String::from_utf8_lossy(&result);
        assert!(result_str.contains("bar=a"));
        assert!(result_str.contains("bar=b"));

        // Should be deterministic
        let result2 = combine_cache_components(&[component2, component1]);
        assert_eq!(result, result2);
    }

    #[test]
    fn test_combine_cache_components_with_headers() {
        let component1 = CacheKeyComponents {
            subgraph_name: "test".to_string(),
            method: "POST".to_string(),
            uri: "https://api.example.com/foo".to_string(),
            headers: [("x-api-key".to_string(), "key1".to_string())]
                .into_iter()
                .collect(),
            body: "{}".to_string(),
        };

        let component2 = CacheKeyComponents {
            subgraph_name: "test".to_string(),
            method: "POST".to_string(),
            uri: "https://api.example.com/foo".to_string(),
            headers: [("x-api-key".to_string(), "key2".to_string())]
                .into_iter()
                .collect(),
            body: "{}".to_string(),
        };

        let result = combine_cache_components(&[component1, component2]);

        // Should combine headers
        let result_str = String::from_utf8_lossy(&result);
        assert!(result_str.contains("x-api-key"));
        assert!(result_str.contains("key1"));
        assert!(result_str.contains("key2"));
    }

    #[test]
    fn test_create_cache_key_empty() {
        let result = create_cache_key(&[], "test-subgraph");
        matches!(result, CacheKey::Entities(ref keys) if keys.is_empty());
    }

    #[test]
    fn test_create_cache_key_root_fields() {
        use std::sync::Arc;

        use crate::connectors::JSONSelection;
        use crate::connectors::runtime::inputs::RequestInputs;

        let selection = Arc::new(JSONSelection::parse("field").unwrap());
        let root_key1 = ResponseKey::RootField {
            name: "foo".to_string(),
            selection: selection.clone(),
            inputs: RequestInputs::default(),
        };
        let root_key2 = ResponseKey::RootField {
            name: "bar".to_string(),
            selection,
            inputs: RequestInputs::default(),
        };

        let http_req1 = http::Request::builder()
            .method("GET")
            .uri("https://api.example.com/foo")
            .body("{}".to_string())
            .unwrap();
        let http_req2 = http::Request::builder()
            .method("GET")
            .uri("https://api.example.com/bar")
            .body("{}".to_string())
            .unwrap();

        let transport1 = TransportRequest::Http(HttpRequest {
            inner: http_req1,
            debug: (None, vec![]),
        });
        let transport2 = TransportRequest::Http(HttpRequest {
            inner: http_req2,
            debug: (None, vec![]),
        });

        let requests = vec![(root_key1, transport1), (root_key2, transport2)];
        let request_refs: Vec<_> = requests.iter().map(|(k, t)| (k, t)).collect();
        let result = create_cache_key(&request_refs, "test-subgraph");

        // Should be Root variant with combined key
        matches!(result, CacheKey::Root(ref key) if key.starts_with("connector:v1:combined:"));
    }

    #[test]
    fn test_create_cache_key_entities() {
        use std::sync::Arc;

        use crate::connectors::JSONSelection;
        use crate::connectors::runtime::inputs::RequestInputs;

        let selection = Arc::new(JSONSelection::parse("field").unwrap());
        let entity_key1 = ResponseKey::Entity {
            index: 0,
            selection: selection.clone(),
            inputs: RequestInputs::default(),
        };
        let entity_key2 = ResponseKey::Entity {
            index: 1,
            selection,
            inputs: RequestInputs::default(),
        };

        let http_req1 = http::Request::builder()
            .method("GET")
            .uri("https://api.example.com/entity/1")
            .body("{}".to_string())
            .unwrap();
        let http_req2 = http::Request::builder()
            .method("GET")
            .uri("https://api.example.com/entity/2")
            .body("{}".to_string())
            .unwrap();

        let transport1 = TransportRequest::Http(HttpRequest {
            inner: http_req1,
            debug: (None, vec![]),
        });
        let transport2 = TransportRequest::Http(HttpRequest {
            inner: http_req2,
            debug: (None, vec![]),
        });

        let requests = vec![(entity_key1, transport1), (entity_key2, transport2)];
        let request_refs: Vec<_> = requests.iter().map(|(k, t)| (k, t)).collect();
        let result = create_cache_key(&request_refs, "test-subgraph");

        // Should be Entities variant with individual keys
        matches!(result, CacheKey::Entities(ref keys) if keys.len() == 2);
    }

    #[test]
    fn test_create_cache_policy_root_fields() {
        use std::sync::Arc;

        use crate::connectors::JSONSelection;
        use crate::connectors::runtime::inputs::RequestInputs;

        let selection = Arc::new(JSONSelection::parse("field").unwrap());
        let root_key = ResponseKey::RootField {
            name: "foo".to_string(),
            selection,
            inputs: RequestInputs::default(),
        };

        let http_req = http::Request::builder()
            .method("GET")
            .uri("https://api.example.com/foo")
            .body("{}".to_string())
            .unwrap();

        let transport = TransportRequest::Http(HttpRequest {
            inner: http_req,
            debug: (None, vec![]),
        });

        let requests = vec![(root_key, transport)];
        let request_refs: Vec<_> = requests.iter().map(|(k, t)| (k, t)).collect();
        let policies = vec![http::HeaderMap::new()];
        let result = create_cache_policy(&request_refs, policies);

        matches!(result, CachePolicy::Roots(_));
    }

    #[test]
    fn test_create_cache_policy_entities() {
        use std::sync::Arc;

        use crate::connectors::JSONSelection;
        use crate::connectors::runtime::inputs::RequestInputs;

        let selection = Arc::new(JSONSelection::parse("field").unwrap());
        let entity_key = ResponseKey::Entity {
            index: 0,
            selection,
            inputs: RequestInputs::default(),
        };

        let http_req = http::Request::builder()
            .method("GET")
            .uri("https://api.example.com/entity/1")
            .body("{}".to_string())
            .unwrap();

        let transport = TransportRequest::Http(HttpRequest {
            inner: http_req,
            debug: (None, vec![]),
        });

        let requests = vec![(entity_key, transport)];
        let request_refs: Vec<_> = requests.iter().map(|(k, t)| (k, t)).collect();
        let policies = vec![http::HeaderMap::new()];
        let result = create_cache_policy(&request_refs, policies);

        matches!(result, CachePolicy::Entities(_));
    }

    #[test]
    fn test_batch_entity_key_duplication() {
        use std::sync::Arc;
        use apollo_compiler::{Schema, Name, executable::FieldSet};

        use crate::connectors::JSONSelection;

        // Create a simple schema for testing
        let schema_str = r#"
            type Query {
                entity: Entity
            }
            
            type Entity {
                id: ID!
                name: String
            }
        "#;
        let schema = Schema::parse_and_validate(schema_str, "test.graphql").unwrap();

        // Create batch entities with different batch sizes
        let selection = Arc::new(JSONSelection::parse("field").unwrap());
        
        // First batch entity with 3 items
        let mut batch1 = Vec::new();
        for _ in 0..3 {
            let mut map = Map::new();
            map.insert(ByteString::from("id"), Value::String(ByteString::from("123")));
            batch1.push(map);
        }
        
        // Second batch entity with 2 items
        let mut batch2 = Vec::new();
        for _ in 0..2 {
            let mut map = Map::new();
            map.insert(ByteString::from("id"), Value::String(ByteString::from("456")));
            batch2.push(map);
        }

        let keys = FieldSet::parse_and_validate(&schema, Name::new("Entity").unwrap(), "id", "test").unwrap();

        let batch_key1 = ResponseKey::BatchEntity {
            selection: selection.clone(),
            keys: keys.clone(),
            inputs: RequestInputs {
                batch: batch1,
                ..Default::default()
            },
        };

        let batch_key2 = ResponseKey::BatchEntity {
            selection,
            keys,
            inputs: RequestInputs {
                batch: batch2,
                ..Default::default()
            },
        };

        // Create transport requests
        let http_req1 = http::Request::builder()
            .method("POST")
            .uri("https://api.example.com/batch1")
            .body("{}".to_string())
            .unwrap();

        let http_req2 = http::Request::builder()
            .method("POST")
            .uri("https://api.example.com/batch2")
            .body("{}".to_string())
            .unwrap();

        let transport1 = TransportRequest::Http(HttpRequest {
            inner: http_req1,
            debug: (None, vec![]),
        });

        let transport2 = TransportRequest::Http(HttpRequest {
            inner: http_req2,
            debug: (None, vec![]),
        });

        // Test cache key duplication
        let requests = vec![(batch_key1, transport1), (batch_key2, transport2)];
        let request_refs: Vec<_> = requests.iter().map(|(k, t)| (k, t)).collect();
        let cache_keys = create_cache_key(&request_refs, "test-subgraph");
        
        // Should have 5 keys total (3 + 2)
        if let CacheKey::Entities(keys) = cache_keys {
            assert_eq!(keys.len(), 5, "Expected 5 cache keys (3 + 2 from batch entities)");
        } else {
            panic!("Expected CacheKey::Entities variant");
        }

        // Test cache policy duplication
        let response_keys = vec![
            requests[0].0.clone(),
            requests[1].0.clone(),
        ];
        
        let mut policy1 = http::HeaderMap::new();
        policy1.insert(http::header::CACHE_CONTROL, "max-age=60".parse().unwrap());
        
        let mut policy2 = http::HeaderMap::new();
        policy2.insert(http::header::CACHE_CONTROL, "max-age=120".parse().unwrap());
        
        let policies = vec![policy1, policy2];
        let cache_policy = create_cache_policy_from_keys(&response_keys, policies);
        
        // Should have 5 policies total (3 + 2)
        if let CachePolicy::Entities(policies) = cache_policy {
            assert_eq!(policies.len(), 5, "Expected 5 cache policies (3 + 2 from batch entities)");
            // First 3 should have max-age=60
            for i in 0..3 {
                assert_eq!(
                    policies[i].get(http::header::CACHE_CONTROL).unwrap(),
                    "max-age=60",
                    "First 3 policies should have max-age=60"
                );
            }
            // Last 2 should have max-age=120
            for i in 3..5 {
                assert_eq!(
                    policies[i].get(http::header::CACHE_CONTROL).unwrap(),
                    "max-age=120",
                    "Last 2 policies should have max-age=120"
                );
            }
        } else {
            panic!("Expected CachePolicy::Entities variant");
        }
    }
}
