use tower::ServiceBuilder;
use tower::layer::util::Stack;

pub mod bytes_client_to_http_client;
pub mod bytes_server_to_json_server;
pub mod cache;
pub mod error_to_graphql;
pub mod http_server_to_bytes_server;
pub mod json_client_to_bytes_client;
pub mod prepare_query;

pub use bytes_client_to_http_client::Error as BytesToHttpError;
pub use bytes_server_to_json_server::Error as BytesToJsonError;
// Re-export cache layer types and functions for convenience
pub use cache::{ArcError, CacheLayer, CacheService, query_parse_cache};
pub use http_server_to_bytes_server::Error as HttpToBytesError;
pub use json_client_to_bytes_client::Error as JsonToBytesError;
pub use prepare_query::Error as PrepareQueryError;

/// Extension trait for `ServiceBuilder` that provides Apollo Router Core layer methods.
///
/// This trait adds convenient methods to Tower's `ServiceBuilder` for composing Apollo Router
/// layers into service stacks. The methods are organized by their role in the request/response
/// pipeline:
///
/// # Layer Categories
///
/// ## Server-Side Request Transformations
/// - `http_to_bytes()` - Extracts HTTP request bodies as bytes
/// - `bytes_to_json()` - Parses bytes as JSON
/// - `prepare_query()` - Orchestrates GraphQL query parsing and planning (composite layer)
///
/// ## Client-Side Request Transformations
/// - `http_client_to_bytes_client()` - Serializes HTTP client requests to bytes
/// - `bytes_client_to_json_client()` - Deserializes bytes to JSON for client services
///
/// ## Response Transformations (Reverse Direction)
/// - `json_to_bytes()` - Serializes JSON responses back to bytes
/// - `bytes_to_http()` - Wraps bytes in HTTP responses
///
/// ## Utility Layers
/// - `cache()` - Adds intelligent caching with configurable predicates
/// - (error_to_graphql layer available directly, not via ServiceBuilderExt)
///
/// # Usage Example
///
/// ```rust,ignore
/// use apollo_router_core::layers::ServiceBuilderExt;
/// use tower::ServiceBuilder;
///
/// # fn example() {
/// # let (parse_service, _handle) = tower_test::mock::spawn();
/// # let (plan_service, _handle) = tower_test::mock::spawn();
/// # let (execution_service, _handle) = tower_test::mock::spawn();
/// # let cache_layer = apollo_router_core::layers::cache::CacheLayer::new(100, |_: &()| (), |_| false);
/// # let (http_client, _handle) = tower_test::mock::spawn();
/// // Server-side pipeline
/// let server = ServiceBuilder::new()
///     .http_to_bytes()        // HTTP → Bytes
///     .bytes_to_json()        // Bytes → JSON
///     .prepare_query(         // JSON → Execution (composite)
///         parse_service,
///         plan_service
///     )
///     .service(execution_service);
///
/// // Client-side pipeline
/// let client = ServiceBuilder::new()
///     .json_to_bytes()                    // JSON → Bytes
///     .bytes_to_http()                    // Bytes → HTTP
///     .cache(cache_layer)                 // Add caching
///     .service(http_client);
/// # }
/// ```
///
/// # Extensions Handling
///
/// All layers follow the standard Extensions pattern:
/// - Create **cloned** Extensions for inner services using `clone()`
/// - Inner services receive Extensions with access to parent context
/// - Responses return **original** Extensions from the request
/// - Parent values always take precedence over child values
pub trait ServiceBuilderExt<L> {
    // Server-side transformations (request pipeline)

    /// Adds HTTP to bytes transformation layer to the service stack.
    ///
    /// This layer extracts HTTP request bodies as bytes and converts HTTP responses
    /// back to streaming format. Used early in server-side request pipelines.
    fn http_server_to_bytes_server(
        self,
    ) -> ServiceBuilder<Stack<http_server_to_bytes_server::HttpToBytesLayer, L>>;

    /// Adds bytes to JSON transformation layer to the service stack.
    ///
    /// This layer parses bytes as JSON with fail-fast error handling and converts
    /// JSON responses back to bytes. Used after HTTP body extraction.
    fn bytes_server_to_json_server(
        self,
    ) -> ServiceBuilder<Stack<bytes_server_to_json_server::BytesToJsonLayer, L>>;

    /// Adds query preparation composite layer to the service stack.
    ///
    /// This composite layer orchestrates GraphQL query parsing and planning services
    /// to transform JSON requests into execution requests. Requires both parse and
    /// plan services to be provided.
    ///
    /// # Arguments
    /// * `query_parse_service` - Service for parsing GraphQL queries
    /// * `query_plan_service` - Service for creating query execution plans
    fn prepare_query<P, Pl>(
        self,
        query_parse_service: P,
        query_plan_service: Pl,
    ) -> ServiceBuilder<Stack<prepare_query::PrepareQueryLayer<P, Pl>, L>>;

    // Response transformations (reverse direction)
    fn json_client_to_bytes_client(
        self,
    ) -> ServiceBuilder<Stack<json_client_to_bytes_client::JsonToBytesLayer, L>>;
    fn bytes_client_to_http_client(
        self,
    ) -> ServiceBuilder<Stack<bytes_client_to_http_client::BytesToHttpLayer, L>>;

    // Caching layer

    /// Adds intelligent caching layer to the service stack.
    ///
    /// This layer provides configurable caching of successful responses and specific
    /// error types. Uses Arc-based storage for zero-copy cache hits and Clock-PRO
    /// eviction algorithm for optimal performance.
    ///
    /// # Arguments
    /// * `cache_layer` - Pre-configured cache layer with key extraction and error predicate
    ///
    /// # Example
    /// ```rust,ignore
    /// use apollo_router_core::layers::cache::CacheLayer;
    /// use tower::ServiceBuilder;
    ///
    /// # fn example() {
    /// # let (inner, _handle) = tower_test::mock::spawn();
    /// let cache_layer = CacheLayer::new(
    ///     1000,
    ///     |_req: &()| String::new(),
    ///     |_err| false
    /// );
    /// let service = ServiceBuilder::new().cache(cache_layer).service(inner);
    /// # }
    /// ```
    #[allow(clippy::type_complexity)]
    fn cache<Req, Resp, K, F, P>(
        self,
        cache_layer: cache::CacheLayer<Req, Resp, K, F, P>,
    ) -> ServiceBuilder<Stack<cache::CacheLayer<Req, Resp, K, F, P>, L>>
    where
        K: std::hash::Hash + Eq + Clone + Send + Sync + 'static,
        Resp: Send + Sync + 'static,
        F: Fn(&Req) -> K + Clone + Send + Sync + 'static,
        P: Fn(&cache::ArcError) -> bool + Clone + Send + Sync + 'static;
}

impl<L> ServiceBuilderExt<L> for ServiceBuilder<L> {
    // Server-side transformations (request pipeline)
    fn http_server_to_bytes_server(
        self,
    ) -> ServiceBuilder<Stack<http_server_to_bytes_server::HttpToBytesLayer, L>> {
        self.layer(http_server_to_bytes_server::HttpToBytesLayer)
    }

    fn bytes_server_to_json_server(
        self,
    ) -> ServiceBuilder<Stack<bytes_server_to_json_server::BytesToJsonLayer, L>> {
        self.layer(bytes_server_to_json_server::BytesToJsonLayer)
    }

    fn prepare_query<P, Pl>(
        self,
        query_parse_service: P,
        query_plan_service: Pl,
    ) -> ServiceBuilder<Stack<prepare_query::PrepareQueryLayer<P, Pl>, L>> {
        self.layer(prepare_query::PrepareQueryLayer::new(
            query_parse_service,
            query_plan_service,
        ))
    }

    // Response transformations (reverse direction)
    fn json_client_to_bytes_client(
        self,
    ) -> ServiceBuilder<Stack<json_client_to_bytes_client::JsonToBytesLayer, L>> {
        self.layer(json_client_to_bytes_client::JsonToBytesLayer)
    }

    fn bytes_client_to_http_client(
        self,
    ) -> ServiceBuilder<Stack<bytes_client_to_http_client::BytesToHttpLayer, L>> {
        self.layer(bytes_client_to_http_client::BytesToHttpLayer::new())
    }

    // Caching layer
    fn cache<Req, Resp, K, F, P>(
        self,
        cache_layer: cache::CacheLayer<Req, Resp, K, F, P>,
    ) -> ServiceBuilder<Stack<cache::CacheLayer<Req, Resp, K, F, P>, L>>
    where
        K: std::hash::Hash + Eq + Clone + Send + Sync + 'static,
        Resp: Send + Sync + 'static,
        F: Fn(&Req) -> K + Clone + Send + Sync + 'static,
        P: Fn(&cache::ArcError) -> bool + Clone + Send + Sync + 'static,
    {
        self.layer(cache_layer)
    }
}
