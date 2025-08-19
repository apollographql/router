//! Cache key generation utilities for connectors.

use std::collections::BTreeMap;

use super::http_json_transport::TransportRequest;

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connectors::runtime::http_json_transport::HttpRequest;

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
}
