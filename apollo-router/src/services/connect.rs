//! Connect service request and response types.

use std::fmt::Debug;
use std::sync::Arc;

use apollo_compiler::ExecutableDocument;
use apollo_compiler::executable::FieldSet;
use apollo_compiler::validation::Valid;
use apollo_federation::connectors::Connector;
use apollo_federation::connectors::runtime::cache::CachePolicy;
use apollo_federation::connectors::runtime::cache::CacheableItem;
use apollo_federation::connectors::runtime::cache::CacheableIterator;
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

#[non_exhaustive]
pub struct Request {
    pub(crate) service_name: Arc<str>,
    pub(crate) context: Context,
    pub(crate) prepared_requests: Vec<ConnectorRequest>,
    #[allow(dead_code)]
    pub(crate) variables: Variables,
    /// Subgraph name needed for lazy cache key generation
    pub(crate) subgraph_name: String,
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
    pub(crate) response: http::Response<graphql::Response>,
    pub(crate) cache_policy: CachePolicy,
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

        Ok(Self {
            service_name,
            context: context.clone(),
            prepared_requests,
            variables,
            subgraph_name,
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
        }
    }

    /// Get an iterator over cacheable items with consolidation logic applied.
    ///
    /// Returns an iterator that:
    /// - Consolidates multiple RootField requests into a single cacheable unit
    /// - Emits one item per Entity/EntityField request for independent caching
    /// - Materializes BatchEntity requests into separate items per batch range
    pub fn cacheable_items(&self) -> CacheableIterator {
        let requests: Vec<(ResponseKey, TransportRequest)> = self
            .prepared_requests
            .iter()
            .map(|req| (req.key.clone(), req.transport_request.clone()))
            .collect();
        create_cacheable_iterator(requests, &self.subgraph_name)
    }

    /// Remove requests from this Request that correspond to a cacheable item.
    ///
    /// This is used by cache plugins to remove requests they can serve from cache.
    ///
    /// - For RootFields: removes all requests (since they're consolidated)
    /// - For Entity/EntityField: removes the request at the specified index
    /// - For BatchItem: removes the request at the specified batch_index
    pub fn remove_cacheable_item(&mut self, item: &CacheableItem) {
        match item {
            CacheableItem::RootFields { .. } => {
                // For root fields, remove all requests since they're consolidated
                self.prepared_requests.clear();
            }
            CacheableItem::Entity { index, .. } => {
                // Remove the specific request at the given index
                if *index < self.prepared_requests.len() {
                    self.prepared_requests.remove(*index);
                }
            }
            CacheableItem::BatchItem { batch_index, .. } => {
                // Remove the specific batch request
                if *batch_index < self.prepared_requests.len() {
                    self.prepared_requests.remove(*batch_index);
                }
            }
        }
    }
}

impl Response {
    /// Create a new Response with the given HTTP response and cache policy
    pub fn new(response: http::Response<graphql::Response>, cache_policy: CachePolicy) -> Self {
        Self {
            response,
            cache_policy,
        }
    }

    /// Create a new Response with default cache policy (no caching)
    pub fn with_default_cache_policy(response: http::Response<graphql::Response>) -> Self {
        Self {
            response,
            cache_policy: CachePolicy::Roots(Vec::new()),
        }
    }

    /// Add cached data to the response using a CacheableItem identifier.
    ///
    /// This will be used by cache plugins to populate responses with cached data
    /// for requests that were removed via `remove_cacheable_item()`.
    ///
    /// The exact implementation will depend on how cached data is structured.
    /// For now, this is a placeholder for future implementation.
    pub fn add_cached_data(&mut self, _item: &CacheableItem, _cached_data: graphql::Response) {
        // TODO: Implement cached data merging logic
        // This will need to:
        // - For RootFields: merge all cached responses into the main response
        // - For Entity/EntityField: add the cached entity data at the correct path
        // - For BatchItem: add the cached item data at the correct batch position
        //
        // The implementation will likely require:
        // 1. Understanding the GraphQL response structure
        // 2. Merging cached data at the correct path/position
        // 3. Handling errors and partial cache hits
        todo!("Implementation will be added when cache plugin integration is ready");
    }

    #[cfg(test)]
    pub(crate) fn test_new() -> Self {
        Self::with_default_cache_policy(
            http::Response::builder()
                .body(graphql::Response::default())
                .unwrap(),
        )
    }
}

#[cfg(test)]
mod tests {
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

    use super::*;

    #[test]
    fn test_remove_cacheable_item_root_fields() {
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
                    key_indices: [
                        0,
                        1,
                    ],
                    operation_type: Query,
                    output_type: "T",
                    output_names: [
                        "ts",
                        "alias",
                    ],
                },
                "connector:v1:subgraph_name:GET|GET:http://localhost/api/path/1|http://localhost/api/path/2:|:|",
            ),
        ]
        "###);

        let item_key = &items.first().unwrap().0;
        request.remove_cacheable_item(item_key);

        assert!(request.prepared_requests.is_empty());
    }

    #[test]
    fn test_remove_cacheable_item_entities() {
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
                    index: 0,
                    output_type: "T",
                },
                "connector:v1:subgraph_name:GET:http://localhost/api/path/1::",
            ),
            (
                Entity {
                    index: 1,
                    output_type: "T",
                },
                "connector:v1:subgraph_name:GET:http://localhost/api/path/2::",
            ),
            (
                Entity {
                    index: 2,
                    output_type: "T",
                },
                "connector:v1:subgraph_name:GET:http://localhost/api/path/3::",
            ),
        ]
        "###);

        let item_key = &items.get(1).unwrap().0;
        request.remove_cacheable_item(item_key);

        assert_eq!(request.prepared_requests.len(), 2);
    }

    #[test]
    fn test_remove_cacheable_item_batch_entities() {
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
                    batch_index: 0,
                    item_index: 0,
                    output_type: "T",
                },
                "connector:v1:subgraph_name:GET:http://localhost/api/path/1%2C2::",
            ),
            (
                BatchItem {
                    batch_index: 0,
                    item_index: 1,
                    output_type: "T",
                },
                "connector:v1:subgraph_name:GET:http://localhost/api/path/1%2C2::",
            ),
            (
                BatchItem {
                    batch_index: 1,
                    item_index: 2,
                    output_type: "T",
                },
                "connector:v1:subgraph_name:GET:http://localhost/api/path/3::",
            ),
        ]
        "###);

        let item_key = &items.get(1).unwrap().0;
        request.remove_cacheable_item(item_key);

        // assert_eq!(request.prepared_requests.len(), 2); // TODO
    }
}
