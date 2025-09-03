//! Cache key generation utilities for connectors.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;

use apollo_compiler::Name;
use apollo_compiler::ast::OperationType;
use http::HeaderMap;

use super::http_json_transport::TransportRequest;
use super::key::ResponseKey;

/// Cache key prefix for connector requests
const CACHE_KEY_PREFIX: &str = "connector:v1";

/// Cache policy for connector responses - just the headers
pub type CachePolicy = HeaderMap;

/// Lazily-evaluated details for a cacheable item
#[derive(Debug)]
pub struct CacheableDetails<'a> {
    /// Cache control headers for this cacheable unit
    pub policies: HeaderMap,
    /// Components for generating cache key
    pub cache_key_components: CacheKeyComponents,
    /// Internal data for lazy response extraction
    response_data: ResponseData<'a>,
}

/// Enum to hold response data based on item type for lazy extraction
#[derive(Debug)]
pub enum ResponseData<'a> {
    /// Full response for root fields - use data directly
    Full(&'a serde_json_bytes::Value),
    /// Entity at specific index - extract data._entities[index]
    Entity {
        data: &'a serde_json_bytes::Value,
        index: usize,
    },
    /// Batch item - extract data._entities[item_index]
    BatchItem {
        data: &'a serde_json_bytes::Value,
        item_index: usize, // Remove batch_index as it's not needed for extraction
    },
}

impl<'a> CacheableDetails<'a> {
    /// Create a new CacheableDetails instance
    pub fn new(
        policies: HeaderMap,
        cache_key_components: CacheKeyComponents,
        response_data: ResponseData<'a>,
    ) -> Self {
        Self {
            policies,
            cache_key_components,
            response_data,
        }
    }

    /// Extract the response data for this cacheable unit
    pub fn response(&self) -> serde_json_bytes::Value {
        match &self.response_data {
            ResponseData::Full(data) => {
                // For root fields, return the data directly (no JSON property extraction needed)
                (*data).clone()
            }
            ResponseData::Entity { data, index } => {
                // Extract data._entities[index]
                extract_entity_from_data(data, *index)
            }
            ResponseData::BatchItem { data, item_index } => {
                // Extract data._entities[item_index]  
                extract_entity_from_data(data, *item_index)
            }
        }
    }
}

/// Components of a request that should be included in a cache key
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CacheKeyComponents {
    /// The subgraph name for uniqueness across subgraphs
    pub subgraph_name: String,
    /// HTTP methods (GET, POST, etc.) - Vec to support consolidated requests
    pub methods: Vec<String>,
    /// The request URIs with interpolated values - Vec to support consolidated requests
    pub uris: Vec<String>,
    /// Relevant headers that affect the response (sorted for determinism) - Vec to support consolidated requests
    pub headers: Vec<BTreeMap<String, String>>,
    /// The request bodies - Vec to support consolidated requests
    pub bodies: Vec<String>,
}

/// Cacheable item representing an abstracted identifier for each independently cacheable unit
#[derive(Debug, Clone)]
pub enum CacheableItem {
    /// Consolidated root field requests - treated as a single cacheable unit
    RootFields {
        operation_type: OperationType,
        output_type: Name,
        output_names: Vec<String>,
    },
    /// Single entity request - independently cacheable
    Entity { index: usize, output_type: Name },
    /// Single item from a batch entity - independently cacheable
    BatchItem {
        batch_index: usize,
        item_index: usize,
        output_type: Name,
    },
}

/// Iterator over cacheable items with access to original keys
pub struct CacheableIterator {
    items: Vec<(CacheableItem, CacheKeyComponents)>,
    current: usize,
}

impl CacheableIterator {
    /// Create from pre-computed items (used for memoization)
    pub fn from_vec(items: Arc<Vec<(CacheableItem, CacheKeyComponents)>>) -> Self {
        Self {
            items: items.to_vec(),
            current: 0,
        }
    }
}

impl Iterator for CacheableIterator {
    type Item = (CacheableItem, CacheKeyComponents);

    fn next(&mut self) -> Option<Self::Item> {
        if self.current < self.items.len() {
            let item = self.items.get(self.current);
            self.current += 1;
            item.cloned()
        } else {
            None
        }
    }
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
    // Include all headers (sorted for determinism)
    let headers: BTreeMap<String, String> = req
        .headers()
        .iter()
        .filter_map(|(name, value)| {
            let name_str = name.as_str().to_lowercase();
            value.to_str().ok().map(|v| (name_str, v.to_string()))
        })
        .collect();

    CacheKeyComponents {
        subgraph_name: subgraph_name.to_string(),
        methods: vec![req.method().as_str().to_string()],
        uris: vec![req.uri().to_string()],
        headers: vec![headers],
        bodies: vec![req.body().clone()],
    }
}

impl fmt::Display for CacheKeyComponents {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Use a format that's easier to debug: connector:v1:subgraph:methods:uris:headers:bodies
        // where each Vec is formatted as item1|item2|... and headers as name1=value1,name2=value2

        let methods_str = self.methods.join("|");
        let uris_str = self.uris.join("|");
        let bodies_str = self.bodies.join("|");

        let headers_str = self
            .headers
            .iter()
            .map(|header_map| {
                header_map
                    .iter()
                    .map(|(k, v)| format!("{}={}", k, v))
                    .collect::<Vec<_>>()
                    .join(",")
            })
            .collect::<Vec<_>>()
            .join("|");

        write!(
            f,
            "{}:{}:{}:{}:{}:{}",
            CACHE_KEY_PREFIX, self.subgraph_name, methods_str, uris_str, headers_str, bodies_str
        )
    }
}

/// Create cache policies based on request types
/// Returns a Vec of HeaderMaps - one per response
pub fn create_cache_policies_from_keys(
    keys: &[ResponseKey],
    cache_policies: Vec<HeaderMap>,
) -> Vec<HeaderMap> {
    if keys.is_empty() {
        return Vec::new();
    }

    // For now, just return the policies as-is
    // The consolidation will happen in Response::cacheable_items()
    cache_policies
}

/// Combine multiple HeaderMaps into one, taking the most restrictive cache policy
pub fn combine_policies(policies: &[HeaderMap]) -> HeaderMap {
    let mut combined = HeaderMap::new();
    for policy in policies {
        for (key, value) in policy {
            // For cache-control headers, we'd want the most restrictive
            // For now, just append all headers
            combined.append(key.clone(), value.clone());
        }
    }
    combined
}

/// Extract entity data from GraphQL response data at the given index
fn extract_entity_from_data(
    data: &serde_json_bytes::Value,
    index: usize,
) -> serde_json_bytes::Value {
    data
        .get("_entities")
        .and_then(|entities| entities.get(index))
        .cloned()
        .unwrap_or(serde_json_bytes::Value::Null)
}

/// Create a cacheable iterator from ResponseKeys and TransportRequests with consolidation logic
pub fn create_cacheable_iterator(
    requests: Vec<(ResponseKey, TransportRequest)>,
    subgraph_name: &str,
) -> CacheableIterator {
    let mut items = Vec::new();
    let mut root_field_requests = Vec::new();
    let mut root_field_keys = Vec::new();

    for (index, (key, transport_request)) in requests.iter().enumerate() {
        match key {
            ResponseKey::RootField { .. } => {
                // Collect root field data for later consolidation
                root_field_requests.push(transport_request);
                root_field_keys.push(key);
            }
            ResponseKey::Entity { output_type, .. }
            | ResponseKey::EntityField { output_type, .. } => {
                // Emit one item per entity/entity field
                let cache_components = extract_cache_components(subgraph_name, transport_request);
                items.push((
                    CacheableItem::Entity {
                        index,
                        output_type: output_type.clone(),
                    },
                    cache_components,
                ));
            }
            ResponseKey::BatchEntity {
                range, type_name, ..
            } => {
                // Materialize batch entities - emit one item per range number
                let cache_components = extract_cache_components(subgraph_name, transport_request);
                for item_index in range.clone() {
                    items.push((
                        CacheableItem::BatchItem {
                            batch_index: index,
                            item_index,
                            output_type: type_name.clone(),
                        },
                        cache_components.clone(), // Clone for each batch item
                    ));
                }
            }
        }
    }

    // Consolidate root fields if any exist
    if !root_field_requests.is_empty() {
        let mut consolidated_components = CacheKeyComponents {
            subgraph_name: subgraph_name.to_string(),
            methods: Vec::new(),
            uris: Vec::new(),
            headers: Vec::new(),
            bodies: Vec::new(),
        };

        // Consolidate data from all root field requests
        for transport_request in &root_field_requests {
            let individual_components = extract_cache_components(subgraph_name, transport_request);
            consolidated_components
                .methods
                .extend(individual_components.methods);
            consolidated_components
                .uris
                .extend(individual_components.uris);
            consolidated_components
                .headers
                .extend(individual_components.headers);
            consolidated_components
                .bodies
                .extend(individual_components.bodies);
        }

        // Extract debugging information from the first root field key
        // (assuming all root fields have the same operation_type and output_type for consolidation)
        let first_root_key = &root_field_keys.first();
        let (operation_type, output_type, output_names) = match first_root_key {
            Some(ResponseKey::RootField {
                operation_type,
                output_type,
                ..
            }) => {
                let names = root_field_keys
                    .iter()
                    .filter_map(|key| {
                        if let ResponseKey::RootField { name, .. } = key {
                            Some(name.clone())
                        } else {
                            None
                        }
                    })
                    .collect();
                (*operation_type, output_type.clone(), names)
            }
            _ => unreachable!("root_field_keys should only contain RootField variants"),
        };

        items.insert(
            0,
            (
                CacheableItem::RootFields {
                    operation_type,
                    output_type,
                    output_names,
                },
                consolidated_components,
            ),
        );
    }

    CacheableIterator { items, current: 0 }
}

#[cfg(test)]
mod tests {

    use apollo_compiler::executable::OperationType;
    use apollo_compiler::name;

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
        assert_eq!(components.methods, vec!["GET"]);
        assert_eq!(components.uris, vec!["https://api.example.com/users/123"]);
        assert_eq!(components.bodies, vec!["{}"]);

        // Should include all headers now (no filtering)
        assert_eq!(components.headers.len(), 1);
        let headers_map = &components.headers[0];
        assert_eq!(headers_map.len(), 3);
        assert!(headers_map.contains_key("content-type"));
        assert!(headers_map.contains_key("x-api-key"));
        assert!(headers_map.contains_key("authorization"));
    }

    #[test]
    fn test_cache_components_delimited_string() {
        let components = CacheKeyComponents {
            subgraph_name: "test".to_string(),
            methods: vec!["GET".to_string()],
            uris: vec!["https://api.example.com/test".to_string()],
            headers: vec![
                [
                    ("content-type".to_string(), "application/json".to_string()),
                    ("x-api-key".to_string(), "secret".to_string()),
                ]
                .into_iter()
                .collect(),
            ],
            bodies: vec!["{}".to_string()],
        };

        let result = components.to_string();

        // Should be deterministic
        assert_eq!(result, components.to_string());

        // Should be in the format: connector:v1:subgraph:methods:uris:headers:bodies
        assert_eq!(
            result,
            "connector:v1:test:GET:https://api.example.com/test:content-type=application/json,x-api-key=secret:{}"
        );
    }

    #[test]
    fn test_create_cache_policy_root_fields() {
        use std::sync::Arc;

        use crate::connectors::JSONSelection;
        use crate::connectors::runtime::inputs::RequestInputs;

        let selection = Arc::new(JSONSelection::parse("field").unwrap());
        let root_key = ResponseKey::RootField {
            name: "foo".to_string(),
            operation_type: OperationType::Query,
            output_type: name!("TestType"),
            selection,
            inputs: RequestInputs::default(),
        };

        let http_req = http::Request::builder()
            .method("GET")
            .uri("https://api.example.com/foo")
            .body("{}".to_string())
            .unwrap();

        let _transport = TransportRequest::Http(HttpRequest {
            inner: http_req,
            debug: (None, vec![]),
        });

        let policies = vec![HeaderMap::new()];
        let result = create_cache_policies_from_keys(&[root_key], policies);

        // Should return a Vec<HeaderMap>
        assert!(!result.is_empty());
    }

    #[test]
    fn test_create_cache_policy_entities() {
        use std::sync::Arc;

        use crate::connectors::JSONSelection;
        use crate::connectors::runtime::inputs::RequestInputs;

        let selection = Arc::new(JSONSelection::parse("field").unwrap());
        let entity_key = ResponseKey::Entity {
            index: 0,
            output_type: name!(User),
            selection,
            inputs: RequestInputs::default(),
        };

        let http_req = http::Request::builder()
            .method("GET")
            .uri("https://api.example.com/entity/1")
            .body("{}".to_string())
            .unwrap();

        let _transport = TransportRequest::Http(HttpRequest {
            inner: http_req,
            debug: (None, vec![]),
        });

        let policies = vec![HeaderMap::new()];
        let result = create_cache_policies_from_keys(&[entity_key], policies);

        // Should return a Vec<HeaderMap>
        assert!(!result.is_empty());
    }

    #[test]
    fn test_cacheable_iterator_consolidation() {
        use std::sync::Arc;

        use apollo_compiler::Name;
        use apollo_compiler::Schema;
        use apollo_compiler::executable::FieldSet;
        use apollo_compiler::executable::OperationType;
        use serde_json_bytes::ByteString;
        use serde_json_bytes::Map;
        use serde_json_bytes::Value;

        use crate::connectors::JSONSelection;
        use crate::connectors::runtime::http_json_transport::HttpRequest;
        use crate::connectors::runtime::inputs::RequestInputs;

        let selection = Arc::new(JSONSelection::parse("field").unwrap());

        // Create mixed ResponseKeys: root fields, entity, and batch entity
        let root_key1 = ResponseKey::RootField {
            name: "foo".to_string(),
            operation_type: OperationType::Query,
            output_type: name!("TestType"),
            selection: selection.clone(),
            inputs: RequestInputs::default(),
        };

        let root_key2 = ResponseKey::RootField {
            name: "bar".to_string(),
            operation_type: OperationType::Query,
            output_type: name!("TestType"),
            selection: selection.clone(),
            inputs: RequestInputs::default(),
        };

        let entity_key = ResponseKey::Entity {
            index: 0,
            output_type: name!(User),
            selection: selection.clone(),
            inputs: RequestInputs::default(),
        };

        // Create batch entity with range 0..2
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
        let keys =
            FieldSet::parse_and_validate(&schema, Name::new("Entity").unwrap(), "id", "test")
                .unwrap();
        let mut batch = Vec::new();
        for i in 0..2 {
            let mut map = Map::new();
            map.insert(
                ByteString::from("id"),
                Value::String(ByteString::from(format!("id{}", i))),
            );
            batch.push(map);
        }

        let batch_key = ResponseKey::BatchEntity {
            type_name: name!(Entity),
            range: 0..2,
            selection,
            keys,
            inputs: RequestInputs {
                batch,
                ..Default::default()
            },
        };

        // Create transport requests for each key
        let transport1 = TransportRequest::Http(HttpRequest {
            inner: http::Request::builder()
                .method("GET")
                .uri("https://api.example.com/foo")
                .body("{}".to_string())
                .unwrap(),
            debug: (None, vec![]),
        });

        let transport2 = TransportRequest::Http(HttpRequest {
            inner: http::Request::builder()
                .method("GET")
                .uri("https://api.example.com/bar")
                .body("{}".to_string())
                .unwrap(),
            debug: (None, vec![]),
        });

        let transport3 = TransportRequest::Http(HttpRequest {
            inner: http::Request::builder()
                .method("GET")
                .uri("https://api.example.com/entity")
                .body("{}".to_string())
                .unwrap(),
            debug: (None, vec![]),
        });

        let transport4 = TransportRequest::Http(HttpRequest {
            inner: http::Request::builder()
                .method("POST")
                .uri("https://api.example.com/batch")
                .body("{}".to_string())
                .unwrap(),
            debug: (None, vec![]),
        });

        let requests = vec![
            (root_key1, transport1),
            (root_key2, transport2),
            (entity_key, transport3),
            (batch_key, transport4),
        ];

        let iterator = create_cacheable_iterator(requests, "test-subgraph");

        // Should have:
        // 1. RootFields (consolidating root_key1 and root_key2)
        // 2. Entity (entity_key)
        // 3. BatchItem (batch_key item 0)
        // 4. BatchItem (batch_key item 1)
        let items: Vec<_> = iterator.collect();
        assert_eq!(items.len(), 4);

        // First item should be consolidated RootFields with CacheKeyComponents
        match &items[0] {
            (
                CacheableItem::RootFields {
                    operation_type,
                    output_type,
                    output_names,
                },
                cache_components,
            ) => {
                assert_eq!(*operation_type, OperationType::Query);
                assert_eq!(output_type.as_str(), "TestType");
                assert_eq!(output_names.len(), 2);
                assert!(output_names.contains(&"foo".to_string()));
                assert!(output_names.contains(&"bar".to_string()));

                // Verify consolidated cache components
                assert_eq!(cache_components.subgraph_name, "test-subgraph");
                assert_eq!(cache_components.methods.len(), 2);
                assert!(cache_components.methods.contains(&"GET".to_string()));
                assert_eq!(cache_components.uris.len(), 2);
                assert!(
                    cache_components
                        .uris
                        .contains(&"https://api.example.com/foo".to_string())
                );
                assert!(
                    cache_components
                        .uris
                        .contains(&"https://api.example.com/bar".to_string())
                );
            }
            _ => panic!("Expected RootFields item"),
        }

        // Second item should be Entity with CacheKeyComponents
        match &items[1] {
            (CacheableItem::Entity { index, output_type }, cache_components) => {
                assert_eq!(*index, 2); // The index in the original keys array
                assert_eq!(output_type.as_str(), "User");

                // Verify entity cache components
                assert_eq!(cache_components.subgraph_name, "test-subgraph");
                assert_eq!(cache_components.methods, vec!["GET".to_string()]);
                assert_eq!(
                    cache_components.uris,
                    vec!["https://api.example.com/entity".to_string()]
                );
            }
            _ => panic!("Expected Entity item"),
        }

        // Third and fourth items should be BatchItems with CacheKeyComponents
        match &items[2] {
            (
                CacheableItem::BatchItem {
                    batch_index,
                    item_index,
                    output_type,
                },
                cache_components,
            ) => {
                assert_eq!(*batch_index, 3); // batch_key is at index 3
                assert_eq!(*item_index, 0);
                assert_eq!(output_type.as_str(), "Entity");

                // Verify batch cache components (should be cloned for each item)
                assert_eq!(cache_components.subgraph_name, "test-subgraph");
                assert_eq!(cache_components.methods, vec!["POST".to_string()]);
                assert_eq!(
                    cache_components.uris,
                    vec!["https://api.example.com/batch".to_string()]
                );
            }
            _ => panic!("Expected BatchItem"),
        }

        match &items[3] {
            (
                CacheableItem::BatchItem {
                    batch_index,
                    item_index,
                    output_type,
                },
                cache_components,
            ) => {
                assert_eq!(*batch_index, 3); // batch_key is at index 3
                assert_eq!(*item_index, 1);
                assert_eq!(output_type.as_str(), "Entity");

                // Verify batch cache components (should be same as previous batch item)
                assert_eq!(cache_components.subgraph_name, "test-subgraph");
                assert_eq!(cache_components.methods, vec!["POST".to_string()]);
                assert_eq!(
                    cache_components.uris,
                    vec!["https://api.example.com/batch".to_string()]
                );
            }
            _ => panic!("Expected BatchItem"),
        }
    }
}
