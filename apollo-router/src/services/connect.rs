//! Connect service request and response types.

use std::fmt::Debug;
use std::sync::Arc;

use apollo_compiler::ExecutableDocument;
use apollo_compiler::executable::FieldSet;
use apollo_compiler::validation::Valid;
use apollo_federation::connectors::Connector;
use apollo_federation::connectors::runtime::cache::CacheKeyComponents;
use apollo_federation::connectors::runtime::cache::CachePolicy;
use apollo_federation::connectors::runtime::cache::CacheableDetails;
use apollo_federation::connectors::runtime::cache::CacheableItem;
use apollo_federation::connectors::runtime::cache::CacheableIterator;
use apollo_federation::connectors::runtime::cache::combine_policies;
use apollo_federation::connectors::runtime::cache::create_cacheable_iterator;
use apollo_federation::connectors::runtime::debug::ConnectorContext;
use apollo_federation::connectors::runtime::http_json_transport::TransportRequest;
use apollo_federation::connectors::runtime::key::ResponseKey;
use parking_lot::Mutex;
use static_assertions::assert_impl_all;
use tower::BoxError;

use crate::Context;
use crate::graphql;
use crate::graphql::Request as GraphQLRequest;
use crate::plugins::connectors::make_requests::make_requests;
use crate::query_planner::fetch::Variables;
use crate::services::connector::request_service::Request as ConnectorRequest;

pub(crate) type BoxService = tower::util::BoxService<Request, Response, BoxError>;
pub(crate) type BoxCloneService = tower::util::BoxCloneService<Request, Response, BoxError>;

#[non_exhaustive]
pub struct Request {
    pub(crate) service_name: Arc<str>,
    pub(crate) context: Context,
    pub(crate) prepared_requests: Vec<ConnectorRequest>,
    #[allow(dead_code)]
    pub(crate) variables: Variables,
    /// Subgraph name needed for lazy cache key generation
    pub(crate) subgraph_name: String,
    /// The "subgraph" name for the connector in the supergraph.
    pub(crate) internal_synthetic_name: String,
    /// Cached cacheable items data - computed once by response_cache plugin
    /// None if cacheable_items() hasn't been called yet
    cacheable_items_cache: Option<Arc<Vec<(CacheableItem, CacheKeyComponents)>>>,
}

impl Debug for Request {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Request")
            .field("service_name", &self.service_name)
            .field("context", &self.context)
            .field("subgraph_name", &self.subgraph_name)
            .field("prepared_requests_len", &self.prepared_requests.len())
            .finish()
    }
}

assert_impl_all!(Response: Send);
#[derive(Debug)]
#[non_exhaustive]
pub struct Response {
    pub(crate) context: Context,
    pub(crate) response: http::Response<graphql::Response>,
    pub(crate) cache_policies: Vec<CachePolicy>, // Vec of HeaderMaps
    /// Cacheable items from the request (if response_cache plugin was enabled)
    /// This ensures we use the same consolidation as the request
    request_cacheable_items: Option<Arc<Vec<(CacheableItem, CacheKeyComponents)>>>,
}

#[buildstructor::buildstructor]
impl Request {
    /// This is the constructor (or builder) to use when constructing a real Request.
    ///
    /// Required parameters are required in non-testing code to create a Request.
    #[builder(visibility = "pub")]
    fn new(
        service_name: Arc<str>,
        context: Context,
        operation: Arc<Valid<ExecutableDocument>>,
        supergraph_request: Arc<http::Request<GraphQLRequest>>,
        variables: Variables,
        keys: Option<Valid<FieldSet>>,
        connector: Arc<Connector>,
    ) -> Result<Self, BoxError> {
        // Get debug context from context extensions
        let debug = context
            .extensions()
            .with_lock(|lock| lock.get::<Arc<Mutex<ConnectorContext>>>().cloned());

        // Call make_requests to prepare HTTP requests
        let prepared_requests = make_requests(
            &operation,
            &variables,
            keys.as_ref(),
            &context,
            supergraph_request.clone(),
            connector.clone(),
            &debug,
        )
        .map_err(BoxError::from)?;

        // Store subgraph name for lazy cache key generation
        let subgraph_name = connector.id.subgraph_name.to_string();
        let internal_synthetic_name = connector.id.synthetic_name();

        Ok(Self {
            service_name,
            context: context.clone(),
            prepared_requests,
            variables,
            subgraph_name,
            internal_synthetic_name,
            cacheable_items_cache: None,
        })
    }

    #[cfg(test)]
    pub(crate) fn test_new(prepared_requests: Vec<ConnectorRequest>) -> Self {
        Self {
            service_name: Arc::from("test_service"),
            context: Context::default(),
            prepared_requests,
            variables: Default::default(),
            subgraph_name: "test_subgraph".into(),
            internal_synthetic_name: "test_subgraph_Query_field_0".into(),
            cacheable_items_cache: None,
        }
    }

    /// Get an iterator over cacheable items with consolidation logic applied.
    ///
    /// Returns an iterator that:
    /// - Consolidates multiple RootField requests into a single cacheable unit
    /// - Emits one item per Entity/EntityField request for independent caching
    /// - Materializes BatchEntity requests into separate items per batch range
    ///
    /// Results are memoized - subsequent calls return the same cached data.
    pub fn cacheable_items(&mut self) -> CacheableIterator {
        // If already computed, return cached version
        if let Some(ref cache) = self.cacheable_items_cache {
            return CacheableIterator::from_vec(cache.clone());
        }

        // Compute and cache for future use
        let requests: Vec<(ResponseKey, TransportRequest)> = self
            .prepared_requests
            .iter()
            .map(|req| (req.key.clone(), req.transport_request.clone()))
            .collect();

        let items: Vec<_> =
            create_cacheable_iterator(requests, &self.subgraph_name, &self.internal_synthetic_name)
                .collect();
        self.cacheable_items_cache = Some(Arc::new(items.clone()));

        CacheableIterator::from_vec(Arc::new(items))
    }

    /// Take the cached cacheable items (used by execute to move to response)
    /// Returns None if cacheable_items() hasn't been called yet.
    pub fn take_cacheable_items_cache(
        &mut self,
    ) -> Option<Arc<Vec<(CacheableItem, CacheKeyComponents)>>> {
        self.cacheable_items_cache.take()
    }

    /// Remove requests from this Request that correspond to cacheable items.
    ///
    /// This is used by cache plugins to remove requests they can serve from cache.
    /// The method uses batch-aware logic to ensure correctness:
    ///
    /// - For RootFields: removes all requests only if all root field items are in the removal list
    /// - For Entity/EntityField: removes individual requests at the specified indices
    /// - For BatchItem: only removes batch requests where ALL items in the batch are cacheable
    ///   (if batch fetches A,B,C,D but only A,C,D are cached, the batch request remains
    ///   to fetch B)
    pub fn remove_cacheable_items(&mut self, items: &[CacheableItem]) {
        if items.is_empty() {
            return;
        }

        // Collect indices to remove, sorted in descending order to avoid index shifting issues
        let mut indices_to_remove = std::collections::BTreeSet::new();

        // Handle different item types
        let mut has_root_fields = false;
        let mut entity_indices = Vec::new();
        let mut batch_items: std::collections::HashMap<usize, Vec<usize>> =
            std::collections::HashMap::new();

        for item in items {
            match item {
                CacheableItem::RootFields { .. } => {
                    has_root_fields = true;
                }
                CacheableItem::Entity { index, .. } => {
                    entity_indices.push(*index);
                }
                CacheableItem::BatchItem {
                    batch_index,
                    entity_index,
                    ..
                } => {
                    batch_items
                        .entry(*batch_index)
                        .or_default()
                        .push(*entity_index);
                }
            }
        }

        // Handle RootFields - remove all requests if we have any root field items
        if has_root_fields {
            // For root fields, we assume if any are cacheable, all consolidated requests should be removed
            // This matches the previous behavior where root fields are treated as a single unit
            self.prepared_requests.clear();
            return; // Early return since all requests are removed
        }

        // Handle Entity items - add their indices directly
        for index in entity_indices {
            if index < self.prepared_requests.len() {
                indices_to_remove.insert(index);
            }
        }

        // Handle BatchItems - only remove batch requests where ALL items are cached
        for (batch_index, cached_item_indices) in batch_items {
            if batch_index >= self.prepared_requests.len() {
                continue;
            }

            // Get the original batch request to find out how many items it contains
            let request = &self.prepared_requests[batch_index];
            if let ResponseKey::BatchEntity { range, .. } = &request.key {
                let total_batch_items: Vec<usize> = range.clone().collect();
                let cached_item_set: std::collections::HashSet<usize> =
                    cached_item_indices.into_iter().collect();

                // Only remove the batch request if ALL items in the batch are cached
                if total_batch_items
                    .iter()
                    .all(|&item_idx| cached_item_set.contains(&item_idx))
                {
                    indices_to_remove.insert(batch_index);
                }
            }
        }

        // Remove requests in descending order to avoid index shifting
        for &index in indices_to_remove.iter().rev() {
            self.prepared_requests.remove(index);
        }
    }
}

impl Response {
    /// Create a new Response with the given HTTP response and cache policies
    pub fn new(
        context: Context,
        response: http::Response<graphql::Response>,
        cache_policies: Vec<CachePolicy>,
        request_cacheable_items: Option<Arc<Vec<(CacheableItem, CacheKeyComponents)>>>,
    ) -> Self {
        Self {
            context,
            response,
            cache_policies,
            request_cacheable_items,
        }
    }

    /// Create a new Response with default cache policy (no caching)
    pub fn with_default_cache_policy(
        context: Context,
        response: http::Response<graphql::Response>,
    ) -> Self {
        Self {
            context,
            response,
            cache_policies: Vec::new(),
            request_cacheable_items: None,
        }
    }

    /// Get an iterator over cacheable items with their details.
    ///
    /// Returns None if request.cacheable_items() was never called.
    /// The response_cache plugin must be enabled for this to work.
    ///
    /// Returns tuples of (item, details) where:
    /// - item: The cacheable unit identifier
    /// - details: Lazily-evaluated cache details including policies, key components, and response data
    pub fn cacheable_items(&mut self) -> Option<Vec<(CacheableItem, CacheableDetails<'_>)>> {
        use apollo_federation::connectors::runtime::cache::ResponseData;

        let items = self.request_cacheable_items.as_ref()?;

        // Get reference to the response data directly (no JSON conversion needed)
        // Use a null value if there's no data
        let response_data = self
            .response
            .body()
            .data
            .as_ref()
            .unwrap_or(&serde_json_bytes::Value::Null);

        let results = items
            .iter()
            .map(|(item, cache_key_components)| {
                let details = match item {
                    CacheableItem::RootFields { .. } => CacheableDetails::new(
                        combine_policies(&self.cache_policies),
                        cache_key_components.clone(),
                        ResponseData::Full(response_data), // Pass direct reference to GraphQL data
                    ),
                    CacheableItem::Entity { index, .. } => CacheableDetails::new(
                        self.cache_policies.get(*index).cloned().unwrap_or_default(),
                        cache_key_components.clone(),
                        ResponseData::Entity {
                            data: response_data, // Pass direct reference to GraphQL data
                            index: *index,
                        },
                    ),
                    CacheableItem::BatchItem {
                        batch_index,
                        entity_index,
                        ..
                    } => CacheableDetails::new(
                        self.cache_policies
                            .get(*batch_index)
                            .cloned()
                            .unwrap_or_default(),
                        cache_key_components.clone(),
                        ResponseData::BatchItem {
                            data: response_data,         // Pass direct reference to GraphQL data
                            entity_index: *entity_index, // Need entity_index for extraction
                        },
                    ),
                };
                (item.clone(), details)
            })
            .collect();

        Some(results)
    }

    /// Add cached data to the response using a CacheableItem identifier.
    ///
    /// This will be used by cache plugins to populate responses with cached data
    /// for requests that were removed via `remove_cacheable_items()`.
    ///
    /// Only the data content is cached - errors and extensions are not preserved.
    ///
    /// - For RootFields: replaces the entire "data" field with cached data
    /// - For Entity/BatchItem: inserts cached entity data into "data._entities[index]"
    pub fn add_cached_data(&mut self, item: &CacheableItem, cached_data: serde_json_bytes::Value) {
        use serde_json_bytes::Value;

        let response_body = self.response.body_mut();

        match item {
            CacheableItem::RootFields { .. } => {
                // For root fields, replace the entire data field with cached data
                response_body.data = Some(cached_data);
            }
            CacheableItem::Entity { index, .. } => {
                // Ensure we have a data object
                if response_body.data.is_none() {
                    response_body.data = Some(Value::Object(Default::default()));
                }

                let data = response_body.data.as_mut().unwrap();
                if let Value::Object(data_obj) = data {
                    // Ensure _entities array exists
                    let entities = data_obj
                        .entry("_entities")
                        .or_insert_with(|| Value::Array(Vec::new()));

                    if let Value::Array(entities_array) = entities {
                        // Extend array if needed to accommodate the index
                        while entities_array.len() <= *index {
                            entities_array.push(Value::Null);
                        }

                        // Insert the cached entity data at the correct index
                        entities_array[*index] = cached_data;
                    }
                }
            }
            CacheableItem::BatchItem { entity_index, .. } => {
                // Ensure we have a data object
                if response_body.data.is_none() {
                    response_body.data = Some(Value::Object(Default::default()));
                }

                let data = response_body.data.as_mut().unwrap();
                if let Value::Object(data_obj) = data {
                    // Ensure _entities array exists
                    let entities = data_obj
                        .entry("_entities")
                        .or_insert_with(|| Value::Array(Vec::new()));

                    if let Value::Array(entities_array) = entities {
                        // Extend array if needed to accommodate the entity_index
                        while entities_array.len() <= *entity_index {
                            entities_array.push(Value::Null);
                        }

                        // Insert the cached entity data at the correct entity_index
                        entities_array[*entity_index] = cached_data;
                    }
                }
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn test_new() -> Self {
        Self::with_default_cache_policy(
            Context::new(),
            http::Response::builder()
                .body(graphql::Response::default())
                .unwrap(),
        )
    }
}

#[cfg(test)]
mod tests {
    use std::slice;

    use apollo_compiler::collections::IndexMap;
    use apollo_compiler::collections::IndexSet;
    use apollo_compiler::name;
    use apollo_federation::connectors::ConnectBatchArguments;
    use apollo_federation::connectors::ConnectId;
    use apollo_federation::connectors::ConnectSpec;
    use apollo_federation::connectors::EntityResolver;
    use apollo_federation::connectors::HttpJsonTransport;
    use apollo_federation::connectors::JSONSelection;
    use apollo_federation::connectors::Namespace;
    use insta::assert_debug_snapshot;
    use itertools::Itertools;
    use serde_json_bytes::Value;
    use serde_json_bytes::json;

    use super::*;

    #[test]
    fn test_remove_cacheable_items_root_fields() {
        let schema = apollo_compiler::Schema::parse_and_validate(
            "type Query { ts(x: Int!): T, _entities(representations: [_Any!]): [_Entity] } type T { id: ID } union _Entity = T scalar _Any",
            "schema.graphql",
        )
        .unwrap();
        let operation = apollo_compiler::ExecutableDocument::parse_and_validate(
            &schema,
            "query Test { ts(x: 1) { id } alias: ts(x: 2) { id } }",
            "operation.graphql",
        )
        .unwrap();

        let connector = Connector {
            spec: ConnectSpec::V0_1,
            id: ConnectId::new(
                "subgraph_name".into(),
                None,
                name!(Query),
                name!(ts),
                None,
                0,
                name!("T"),
            ),
            transport: HttpJsonTransport {
                source_template: "http://localhost/api".parse().ok(),
                connect_template: "/path/{$args.x}".parse().unwrap(),
                ..Default::default()
            },
            selection: JSONSelection::parse("id").unwrap(),
            entity_resolver: None,
            config: Default::default(),
            max_requests: None,
            batch_settings: None,
            request_headers: Default::default(),
            response_headers: Default::default(),
            request_variable_keys: IndexMap::from_iter([(
                Namespace::Args,
                IndexSet::from_iter(["x".to_string()]),
            )]),
            response_variable_keys: Default::default(),
            error_settings: Default::default(),
            label: "test label".into(),
        };

        let mut request = Request::new(
            Arc::from("test_service"),
            Default::default(),
            Arc::new(operation),
            Arc::new(http::Request::new(GraphQLRequest::default())),
            Default::default(),
            None,
            Arc::new(connector),
        )
        .unwrap();

        let items = request
            .cacheable_items()
            .map(|(k, v)| (k, v.to_string()))
            .collect_vec();
        assert_debug_snapshot!(items, @r###"
        [
            (
                RootFields {
                    internal_synthetic_name: "subgraph_name_Query_ts_0",
                    operation_type: Query,
                    output_type: "T",
                    output_names: [
                        "ts",
                        "alias",
                    ],
                    surrogate_key_data: {
                        "x": Number(1),
                    },
                },
                "connector:v1:subgraph_name:GET|GET:http://localhost/api/path/1|http://localhost/api/path/2:|:|",
            ),
        ]
        "###);

        let item_key = &items.first().unwrap().0;
        request.remove_cacheable_items(slice::from_ref(item_key));

        assert!(request.prepared_requests.is_empty());
    }

    #[test]
    fn test_remove_cacheable_items_entities() {
        let schema = apollo_compiler::Schema::parse_and_validate(
            "type Query { ts(x: Int!): T, _entities(representations: [_Any!]): [_Entity] } type T { id: ID b: Int } union _Entity = T scalar _Any",
            "schema.graphql",
        )
        .unwrap();
        let operation = apollo_compiler::ExecutableDocument::parse_and_validate(
            &schema,
            "query ($representations: [_Any!]!) { _entities(representations: $representations) { ... on T { b } } }",
            "operation.graphql",
        )
        .unwrap();

        let connector = Connector {
            spec: ConnectSpec::V0_1,
            id: ConnectId::new_on_object("subgraph_name".into(), None, name!(T), None, 0, name!(T)),
            transport: HttpJsonTransport {
                source_template: "http://localhost/api".parse().ok(),
                connect_template: "/path/{$this.id}".parse().unwrap(),
                ..Default::default()
            },
            selection: JSONSelection::parse("b").unwrap(),
            entity_resolver: Some(EntityResolver::TypeSingle),
            config: Default::default(),
            max_requests: None,
            batch_settings: None,
            request_headers: Default::default(),
            response_headers: Default::default(),
            request_variable_keys: IndexMap::from_iter([(
                Namespace::This,
                IndexSet::from_iter(["id".to_string()]),
            )]),
            response_variable_keys: Default::default(),
            error_settings: Default::default(),
            label: "test label".into(),
        };

        let variables = Variables {
            variables: serde_json_bytes::json!({
                "representations": [
                    { "__typename": "T", "id": "1" },
                    { "__typename": "T", "id": "2" },
                    { "__typename": "T", "id": "3" }
                ]
            })
            .as_object()
            .unwrap()
            .clone(),
            inverted_paths: Default::default(),
            contextual_arguments: Default::default(),
        };

        let mut request = Request::new(
            Arc::from("test_service"),
            Default::default(),
            Arc::new(operation),
            Arc::new(http::Request::new(GraphQLRequest::default())),
            variables,
            None,
            Arc::new(connector),
        )
        .unwrap();

        let items = request
            .cacheable_items()
            .map(|(k, v)| (k, v.to_string()))
            .collect_vec();
        assert_debug_snapshot!(items, @r###"
        [
            (
                Entity {
                    internal_synthetic_name: "subgraph_name_T_0",
                    index: 0,
                    output_type: "T",
                    surrogate_key_data: {
                        "__typename": String(
                            "T",
                        ),
                        "id": String(
                            "1",
                        ),
                    },
                },
                "connector:v1:subgraph_name:GET:http://localhost/api/path/1::",
            ),
            (
                Entity {
                    internal_synthetic_name: "subgraph_name_T_0",
                    index: 1,
                    output_type: "T",
                    surrogate_key_data: {
                        "__typename": String(
                            "T",
                        ),
                        "id": String(
                            "2",
                        ),
                    },
                },
                "connector:v1:subgraph_name:GET:http://localhost/api/path/2::",
            ),
            (
                Entity {
                    internal_synthetic_name: "subgraph_name_T_0",
                    index: 2,
                    output_type: "T",
                    surrogate_key_data: {
                        "__typename": String(
                            "T",
                        ),
                        "id": String(
                            "3",
                        ),
                    },
                },
                "connector:v1:subgraph_name:GET:http://localhost/api/path/3::",
            ),
        ]
        "###);

        let item_key = &items.get(1).unwrap().0;
        request.remove_cacheable_items(slice::from_ref(item_key));

        assert_eq!(request.prepared_requests.len(), 2);
    }

    #[test]
    fn test_remove_cacheable_items_batch_partial() {
        let schema = apollo_compiler::Schema::parse_and_validate(
            "type Query { ts(x: Int!): T, _entities(representations: [_Any!]): [_Entity] } type T { id: ID b: Int } union _Entity = T scalar _Any",
            "schema.graphql",
        )
        .unwrap();
        let operation = apollo_compiler::ExecutableDocument::parse_and_validate(
            &schema,
            "query ($representations: [_Any!]!) { _entities(representations: $representations) { ... on T { b } } }",
            "operation.graphql",
        )
        .unwrap();

        let connector = Connector {
            spec: ConnectSpec::V0_1,
            id: ConnectId::new_on_object("subgraph_name".into(), None, name!(T), None, 0, name!(T)),
            transport: HttpJsonTransport {
                source_template: "http://localhost/api".parse().ok(),
                connect_template: "/path/{$batch.id->joinNotNull(',')}".parse().unwrap(),
                ..Default::default()
            },
            selection: JSONSelection::parse("b").unwrap(),
            entity_resolver: Some(EntityResolver::TypeBatch),
            config: Default::default(),
            max_requests: None,
            batch_settings: Some(ConnectBatchArguments { max_size: Some(2) }),
            request_headers: Default::default(),
            response_headers: Default::default(),
            request_variable_keys: IndexMap::from_iter([(
                Namespace::Batch,
                IndexSet::from_iter(["id".to_string()]),
            )]),
            response_variable_keys: Default::default(),
            error_settings: Default::default(),
            label: "test label".into(),
        };

        let variables = Variables {
            variables: serde_json_bytes::json!({
                "representations": [
                    { "__typename": "T", "id": "1" },
                    { "__typename": "T", "id": "2" },
                    { "__typename": "T", "id": "3" }
                ]
            })
            .as_object()
            .unwrap()
            .clone(),
            inverted_paths: Default::default(),
            contextual_arguments: Default::default(),
        };

        let key = connector.resolvable_key(&schema).unwrap();

        let mut request = Request::new(
            Arc::from("test_service"),
            Default::default(),
            Arc::new(operation),
            Arc::new(http::Request::new(GraphQLRequest::default())),
            variables,
            key,
            Arc::new(connector),
        )
        .unwrap();

        let items = request
            .cacheable_items()
            .map(|(k, v)| (k, v.to_string()))
            .collect_vec();
        assert_debug_snapshot!(items, @r###"
        [
            (
                BatchItem {
                    internal_synthetic_name: "subgraph_name_T_0",
                    batch_index: 0,
                    entity_index: 0,
                    batch_position: 0,
                    output_type: "T",
                    surrogate_key_data: {
                        "__typename": String(
                            "T",
                        ),
                        "id": String(
                            "1",
                        ),
                    },
                },
                "connector:v1:subgraph_name:GET:http://localhost/api/path/1%2C2::",
            ),
            (
                BatchItem {
                    internal_synthetic_name: "subgraph_name_T_0",
                    batch_index: 0,
                    entity_index: 1,
                    batch_position: 1,
                    output_type: "T",
                    surrogate_key_data: {
                        "__typename": String(
                            "T",
                        ),
                        "id": String(
                            "2",
                        ),
                    },
                },
                "connector:v1:subgraph_name:GET:http://localhost/api/path/1%2C2::",
            ),
            (
                BatchItem {
                    internal_synthetic_name: "subgraph_name_T_0",
                    batch_index: 1,
                    entity_index: 2,
                    batch_position: 0,
                    output_type: "T",
                    surrogate_key_data: {
                        "__typename": String(
                            "T",
                        ),
                        "id": String(
                            "3",
                        ),
                    },
                },
                "connector:v1:subgraph_name:GET:http://localhost/api/path/3::",
            ),
        ]
        "###);

        let item_key = &items.get(1).unwrap().0;
        request.remove_cacheable_items(slice::from_ref(item_key));

        // Should NOT remove the batch request since only 1 of 2 items in batch 0 is cached
        assert_eq!(request.prepared_requests.len(), 2);
    }

    #[test]
    fn test_remove_cacheable_items_batch_complete() {
        let schema = apollo_compiler::Schema::parse_and_validate(
            "type Query { ts(x: Int!): T, _entities(representations: [_Any!]): [_Entity] } type T { id: ID b: Int } union _Entity = T scalar _Any",
            "schema.graphql",
        )
        .unwrap();
        let operation = apollo_compiler::ExecutableDocument::parse_and_validate(
            &schema,
            "query ($representations: [_Any!]!) { _entities(representations: $representations) { ... on T { b } } }",
            "operation.graphql",
        )
        .unwrap();

        let connector = Connector {
            spec: ConnectSpec::V0_1,
            id: ConnectId::new_on_object("subgraph_name".into(), None, name!(T), None, 0, name!(T)),
            transport: HttpJsonTransport {
                source_template: "http://localhost/api".parse().ok(),
                connect_template: "/path/{$batch.id->joinNotNull(',')}".parse().unwrap(),
                ..Default::default()
            },
            selection: JSONSelection::parse("b").unwrap(),
            entity_resolver: Some(EntityResolver::TypeBatch),
            config: Default::default(),
            max_requests: None,
            batch_settings: Some(ConnectBatchArguments { max_size: Some(2) }),
            request_headers: Default::default(),
            response_headers: Default::default(),
            request_variable_keys: IndexMap::from_iter([(
                Namespace::Batch,
                IndexSet::from_iter(["id".to_string()]),
            )]),
            response_variable_keys: Default::default(),
            error_settings: Default::default(),
            label: "test label".into(),
        };

        let variables = Variables {
            variables: serde_json_bytes::json!({
                "representations": [
                    { "__typename": "T", "id": "1" },
                    { "__typename": "T", "id": "2" },
                    { "__typename": "T", "id": "3" }
                ]
            })
            .as_object()
            .unwrap()
            .clone(),
            inverted_paths: Default::default(),
            contextual_arguments: Default::default(),
        };

        let key = connector.resolvable_key(&schema).unwrap();

        let mut request = Request::new(
            Arc::from("test_service"),
            Default::default(),
            Arc::new(operation),
            Arc::new(http::Request::new(GraphQLRequest::default())),
            variables,
            key,
            Arc::new(connector),
        )
        .unwrap();

        let items = request
            .cacheable_items()
            .map(|(k, v)| (k, v.to_string()))
            .collect_vec();

        // Remove both items from batch 0 (items 0 and 1)
        let batch_items = vec![
            items.first().unwrap().0.clone(),
            items.get(1).unwrap().0.clone(),
        ];
        request.remove_cacheable_items(&batch_items);

        // Should remove batch request 0 since ALL items in that batch are cached
        // Should have 1 request left (batch request 1 with item 2)
        assert_eq!(request.prepared_requests.len(), 1);
    }

    #[test]
    fn test_add_cached_data_root_fields() {
        let mut response = Response::with_default_cache_policy(
            Context::new(),
            http::Response::builder()
                .body(graphql::Response::default())
                .unwrap(),
        );

        let cached_data = json!({ "test": "cached" });

        let cache_item = CacheableItem::RootFields {
            internal_synthetic_name: "test_subgraph_Query_field_0".to_string(),
            operation_type: apollo_compiler::ast::OperationType::Query,
            output_type: apollo_compiler::Name::new("Test").unwrap(),
            output_names: vec!["test".to_string()],
            surrogate_key_data: json!({ "first": 5 }).as_object().cloned().unwrap(),
        };

        response.add_cached_data(&cache_item, cached_data);

        // Verify that the data was set correctly
        assert!(response.response.body().data.is_some());
        if let Some(Value::Object(data)) = &response.response.body().data {
            assert_eq!(data.get("test"), Some(&Value::String("cached".into())));
        } else {
            panic!("Expected object data");
        }
    }

    #[test]
    fn test_add_cached_data_entity() {
        let mut response = Response::with_default_cache_policy(
            Context::new(),
            http::Response::builder()
                .body(graphql::Response::default())
                .unwrap(),
        );

        let cached_data = json!({ "id": "123" });

        let cache_item = CacheableItem::Entity {
            internal_synthetic_name: "test_subgraph_Query_field_0".to_string(),
            index: 1,
            output_type: apollo_compiler::Name::new("User").unwrap(),
            surrogate_key_data: json!({ "__typename": "User", "id": "123" })
                .as_object()
                .cloned()
                .unwrap(),
        };

        response.add_cached_data(&cache_item, cached_data);

        // Verify that the entity was added at the correct index
        if let Some(Value::Object(data)) = &response.response.body().data {
            if let Some(Value::Array(entities)) = data.get("_entities") {
                // Should have extended the array to fit index 1
                assert_eq!(entities.len(), 2);
                assert_eq!(entities[0], Value::Null); // Padding
                if let Value::Object(entity) = &entities[1] {
                    assert_eq!(entity.get("id"), Some(&Value::String("123".into())));
                } else {
                    panic!("Expected object at entities[1]");
                }
            } else {
                panic!("Expected _entities array");
            }
        } else {
            panic!("Expected object data");
        }
    }

    #[test]
    fn test_add_cached_data_batch_item() {
        let mut response = Response::with_default_cache_policy(
            Context::new(),
            http::Response::builder()
                .body(graphql::Response::default())
                .unwrap(),
        );

        let cached_data = json!({ "name": "batch_item" });

        let cache_item = CacheableItem::BatchItem {
            internal_synthetic_name: "test_subgraph_Query_field_0".to_string(),
            batch_index: 0,
            entity_index: 2,
            batch_position: 0,
            output_type: apollo_compiler::Name::new("Product").unwrap(),
            surrogate_key_data: json!({ "__typename": "Product", "id": "batch_item" })
                .as_object()
                .cloned()
                .unwrap(),
        };

        response.add_cached_data(&cache_item, cached_data);

        // Verify that the batch item was added at the correct entity_index
        if let Some(Value::Object(data)) = &response.response.body().data {
            if let Some(Value::Array(entities)) = data.get("_entities") {
                // Should have extended the array to fit entity_index 2
                assert_eq!(entities.len(), 3);
                assert_eq!(entities[0], Value::Null); // Padding
                assert_eq!(entities[1], Value::Null); // Padding
                if let Value::Object(entity) = &entities[2] {
                    assert_eq!(
                        entity.get("name"),
                        Some(&Value::String("batch_item".into()))
                    );
                } else {
                    panic!("Expected object at entities[2]");
                }
            } else {
                panic!("Expected _entities array");
            }
        } else {
            panic!("Expected object data");
        }
    }
}
