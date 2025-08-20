//! Cache key generation utilities for connectors.

use std::collections::BTreeMap;

use http::HeaderMap;

use super::http_json_transport::TransportRequest;
use super::key::ResponseKey;

/// Cache key for connector requests
#[derive(Debug, Clone)]
pub enum CacheKey {
    /// Individual cache keys for entity fetches - one per entity
    Entities(Vec<String>),
    /// Individual cache keys for root field requests - one per root field
    Roots(Vec<String>),
}

/// Cache policy for connector responses
#[derive(Debug, Clone)]
pub enum CachePolicy {
    /// Individual cache policies for entity fetches - one per entity
    Entities(Vec<HeaderMap>),
    /// Cache policies for root field requests - consumer can combine as needed
    Roots(Vec<HeaderMap>),
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
    /// Create a colon-delimited string for debugging and external hashing
    pub fn to_delimited_string(&self) -> String {
        // Use a format that's easier to debug: subgraph:method:uri:headers:body
        // where headers are formatted as name1=value1,name2=value2
        let headers_str = self
            .headers
            .iter()
            .map(|(k, v)| format!("{}={}", k, v))
            .collect::<Vec<_>>()
            .join(",");

        format!(
            "{}:{}:{}:{}:{}",
            self.subgraph_name, self.method, self.uri, headers_str, self.body
        )
    }
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
        // For root fields, create individual cache keys for each request
        let individual_keys: Vec<String> = requests
            .iter()
            .map(|(_, transport_req)| {
                let components = extract_cache_components(subgraph_name, transport_req);
                let delimited_string = components.to_delimited_string();
                format!("connector:v1:{}", delimited_string)
            })
            .collect();

        CacheKey::Roots(individual_keys)
    } else {
        // For entities, create individual cache keys
        // For batch entities, we need to duplicate keys based on batch size
        let mut individual_keys = Vec::new();

        for (key, transport_req) in requests.iter() {
            let components = extract_cache_components(subgraph_name, transport_req);
            let delimited_string = components.to_delimited_string();
            let cache_key = format!("connector:v1:{}", delimited_string);

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
    cache_policies: Vec<HeaderMap>,
) -> CachePolicy {
    if keys.is_empty() {
        return CachePolicy::Entities(Vec::new());
    }

    // Check if all requests are root fields
    let root_field = keys
        .iter()
        .any(|key| matches!(key, ResponseKey::RootField { .. }));

    if root_field {
        CachePolicy::Roots(cache_policies)
    } else {
        // For batch entities, we need to duplicate policies based on batch size
        let mut expanded_policies = Vec::new();

        for (key, policy) in keys.iter().zip(cache_policies.into_iter()) {
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
    response_policies: Vec<HeaderMap>,
) -> CachePolicy {
    let keys: Vec<&ResponseKey> = requests.iter().map(|(key, _)| *key).collect();
    create_cache_policy_from_keys(
        &keys.iter().map(|k| (*k).clone()).collect::<Vec<_>>(),
        response_policies,
    )
}

#[cfg(test)]
mod tests {
    use serde_json_bytes::ByteString;
    use serde_json_bytes::Map;
    use serde_json_bytes::Value;

    use super::*;
    use crate::connectors::runtime::http_json_transport::HttpRequest;
    use crate::connectors::runtime::inputs::RequestInputs;

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
    fn test_cache_components_delimited_string() {
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

        let result = components.to_delimited_string();

        // Should be deterministic
        assert_eq!(result, components.to_delimited_string());

        // Should be in the format: subgraph:method:uri:headers:body
        assert_eq!(
            result,
            "test:GET:https://api.example.com/test:content-type=application/json,x-api-key=secret:{}"
        );
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

        // Should be Roots variant with individual keys
        if let CacheKey::Roots(keys) = result {
            assert_eq!(keys.len(), 2);
            assert!(keys[0].starts_with("connector:v1:"));
            assert!(keys[1].starts_with("connector:v1:"));
            // Keys should be different
            assert_ne!(keys[0], keys[1]);
        } else {
            panic!("Expected CacheKey::Roots variant");
        }
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
        let policies = vec![HeaderMap::new()];
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
        let policies = vec![HeaderMap::new()];
        let result = create_cache_policy(&request_refs, policies);

        matches!(result, CachePolicy::Entities(_));
    }

    #[test]
    fn test_batch_entity_key_duplication() {
        use std::sync::Arc;

        use apollo_compiler::Name;
        use apollo_compiler::Schema;
        use apollo_compiler::executable::FieldSet;

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
            map.insert(
                ByteString::from("id"),
                Value::String(ByteString::from("123")),
            );
            batch1.push(map);
        }

        // Second batch entity with 2 items
        let mut batch2 = Vec::new();
        for _ in 0..2 {
            let mut map = Map::new();
            map.insert(
                ByteString::from("id"),
                Value::String(ByteString::from("456")),
            );
            batch2.push(map);
        }

        let keys =
            FieldSet::parse_and_validate(&schema, Name::new("Entity").unwrap(), "id", "test")
                .unwrap();

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
            assert_eq!(
                keys.len(),
                5,
                "Expected 5 cache keys (3 + 2 from batch entities)"
            );
        } else {
            panic!("Expected CacheKey::Entities variant");
        }

        // Test cache policy duplication
        let response_keys = vec![requests[0].0.clone(), requests[1].0.clone()];

        let mut policy1 = HeaderMap::new();
        policy1.insert(http::header::CACHE_CONTROL, "max-age=60".parse().unwrap());

        let mut policy2 = HeaderMap::new();
        policy2.insert(http::header::CACHE_CONTROL, "max-age=120".parse().unwrap());

        let policies = vec![policy1, policy2];
        let cache_policy = create_cache_policy_from_keys(&response_keys, policies);

        // Should have 5 policies total (3 + 2)
        if let CachePolicy::Entities(policies) = cache_policy {
            assert_eq!(
                policies.len(),
                5,
                "Expected 5 cache policies (3 + 2 from batch entities)"
            );
            // First 3 should have max-age=60
            for policy in policies.iter().take(3) {
                assert_eq!(
                    policy.get(http::header::CACHE_CONTROL).unwrap(),
                    "max-age=60",
                    "First 3 policies should have max-age=60"
                );
            }
            // Last 2 should have max-age=120
            for policy in policies.iter().take(5).skip(3) {
                assert_eq!(
                    policy.get(http::header::CACHE_CONTROL).unwrap(),
                    "max-age=120",
                    "Last 2 policies should have max-age=120"
                );
            }
        } else {
            panic!("Expected CachePolicy::Entities variant");
        }
    }
}
