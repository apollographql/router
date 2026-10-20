//! Implements GraphQL schema introspection.
use std::future::Ready;
use std::hash::Hash;
use std::sync::Arc;

use apollo_cache_memory::InMemoryConfig;
use apollo_cache_memory::ServiceBuilderExt as _;
use futures::future::BoxFuture;
use futures::future::Either;
use tower::BoxError;
use tower::ServiceBuilder;
use tower::ServiceExt as _;
use tower::util::BoxCloneService;

use crate::Configuration;
use crate::compute_job;
use crate::compute_job::ComputeJobType;
use crate::graphql;
use crate::json_ext::Object;
use crate::services::query_parsing::ParsedDocument;
use crate::spec;

const DEFAULT_INTROSPECTION_CACHE_CAPACITY: u64 = 5;

/// Request type for [IntrospectionService].
#[derive(Clone)]
pub(crate) struct IntrospectionRequest {
    /// The GraphQL schema to introspect.
    pub(crate) schema: Arc<spec::Schema>,
    /// Document representing the introspection operation to execute.
    pub(crate) document: ParsedDocument,
    /// JSON variable values used to execute the query.
    pub(crate) variables: Object,
}

impl PartialEq for IntrospectionRequest {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.schema, &other.schema)
            && self.document.hash == other.document.hash
            && self.variables == other.variables
    }
}

impl Eq for IntrospectionRequest {}

impl Hash for IntrospectionRequest {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        // The document's `hash` field includes the schema, the query text, and the operation name,
        // so it covers most of the introspection request
        self.document.hash.hash(state);
        self.variables.hash(state);
    }
}

/// A terminal service that handles (partial) execution of introspection.
pub(crate) type IntrospectionService =
    BoxCloneService<IntrospectionRequest, graphql::Response, BoxError>;

#[derive(Clone)]
enum Mode {
    Disabled,
    Enabled { max_depth: MaxDepth },
}

#[derive(Copy, Clone)]
enum MaxDepth {
    Check,
    Ignore,
}

/// Determine the introspection mode based on YAML configuration.
fn introspection_mode(configuration: &Configuration) -> Mode {
    if configuration.supergraph.introspection {
        Mode::Enabled {
            max_depth: if configuration.limits.router.introspection_max_depth {
                MaxDepth::Check
            } else {
                MaxDepth::Ignore
            },
        }
    } else {
        Mode::Disabled
    }
}

/// Returns a terminal service that does cached, partial execution of introspection.
///
/// If introspection is disabled in config, always returns an error response.
/// If a query contains both introspection and concrete fields, returns an error response.
pub(crate) fn introspection_service(configuration: &Configuration) -> IntrospectionService {
    let builder = ServiceBuilder::new()
        .load_shed()
        .layer(RejectMixedIntrospectionLayer::new());

    match introspection_mode(configuration) {
        Mode::Enabled { max_depth } => builder
            .cache_in_memory(
                "introspection",
                InMemoryConfig::<DEFAULT_INTROSPECTION_CACHE_CAPACITY>::default(),
            )
            .service(IntrospectionExecutionService::new(max_depth))
            .boxed_clone(),
        Mode::Disabled => builder
            .service(IntrospectionDisabledService::new())
            .boxed_clone(),
    }
}

/// Returns if the document contains an introspection query. If this function returns true, it is
/// appropriate to use the introspection service to resolve the query.
///
/// That is:
/// - The operation is a query operation, AND:
/// - The operation has schema introspection fields (__schema or __type)
///
/// Notably, { __typename } is not considered an introspection query.
pub(crate) fn is_introspection_query(document: &ParsedDocument) -> bool {
    let operation = &document.operation;

    operation.is_query()
        && operation
            .root_fields(&document.executable)
            .any(|field| matches!(field.name.as_str(), "__schema" | "__type"))
}

/// Terminal service for GraphQL introspection queries that always returns an error saying
/// introspection is disabled.
#[derive(Clone)]
struct IntrospectionDisabledService {
    _private: (),
}

impl IntrospectionDisabledService {
    fn new() -> Self {
        Self { _private: () }
    }
}

impl tower::Service<IntrospectionRequest> for IntrospectionDisabledService {
    // Actually Infallible, but this matches the IntrospectionExecutionService.
    type Error = BoxError;
    type Response = graphql::Response;
    type Future = Ready<Result<Self::Response, Self::Error>>;

    fn poll_ready(
        &mut self,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        std::task::Poll::Ready(Ok(()))
    }

    fn call(&mut self, _req: IntrospectionRequest) -> Self::Future {
        let error = graphql::Error::builder()
            .message(String::from("introspection has been disabled"))
            .extension_code("INTROSPECTION_DISABLED")
            .build();
        std::future::ready(Ok(graphql::Response::builder().error(error).build()))
    }
}

/// Short-circuits requests that contain both introspection and concrete fields, responding with GraphQL errors.
struct RejectMixedIntrospectionLayer {
    _private: (),
}
impl RejectMixedIntrospectionLayer {
    fn new() -> Self {
        Self { _private: () }
    }
}

impl<S> tower::Layer<S> for RejectMixedIntrospectionLayer {
    type Service = RejectMixedIntrospectionService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        RejectMixedIntrospectionService { inner }
    }
}

/// Short-circuits requests that contain both introspection and concrete fields, responding with GraphQL errors.
#[derive(Clone)]
struct RejectMixedIntrospectionService<S> {
    inner: S,
}

impl<S> tower::Service<IntrospectionRequest> for RejectMixedIntrospectionService<S>
where
    S: tower::Service<IntrospectionRequest, Response = graphql::Response>,
{
    type Response = graphql::Response;
    type Error = S::Error;
    type Future = Either<
        S::Future,
        // Rejection
        Ready<Result<Self::Response, Self::Error>>,
    >;

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: IntrospectionRequest) -> Self::Future {
        let operation = &req.document.operation;

        // We should only receive an IntrospectionRequest if the operation was already determined to
        // contain introspection fields. So, we only have to check if it contains any
        // _non-introspection_ fields to decide if it's a mixed operation.
        if operation
            .root_fields(&req.document.executable)
            .any(|field| !matches!(field.name.as_str(), "__typename" | "__schema" | "__type"))
        {
            let error = graphql::Error::builder()
                .message(
                    "\
                    Mixed queries with both schema introspection and concrete fields \
                    are not supported yet: https://github.com/apollographql/router/issues/2789\
                ",
                )
                .extension_code("MIXED_INTROSPECTION")
                .build();
            Either::Right(std::future::ready(Ok(graphql::Response::builder()
                .error(error)
                .build())))
        } else {
            Either::Left(self.inner.call(req))
        }
    }
}

/// Terminal service for executing GraphQL introspection queries against a schema.
///
/// Only the introspection parts of the input GraphQL query are executed. Non-introspection parts
/// are silently ignored.
///
/// When the introspection depth limit is exceeded, returns an error response.
#[derive(Clone)]
struct IntrospectionExecutionService {
    max_depth: MaxDepth,
}

impl IntrospectionExecutionService {
    fn new(max_depth: MaxDepth) -> Self {
        Self { max_depth }
    }
}

impl tower::Service<IntrospectionRequest> for IntrospectionExecutionService {
    type Response = graphql::Response;
    type Error = BoxError;
    type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn poll_ready(
        &mut self,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        std::task::Poll::Ready(Ok(()))
    }

    fn call(&mut self, req: IntrospectionRequest) -> Self::Future {
        let max_depth = self.max_depth;

        Box::pin(async move {
            Ok(
                compute_job::execute(ComputeJobType::Introspection, move |_| {
                    execute_introspection(max_depth, &req.schema, &req.document, req.variables)
                })?
                .await,
            )
        })
    }
}

fn execute_introspection(
    max_depth: MaxDepth,
    schema: &spec::Schema,
    doc: &ParsedDocument,
    variables: Object,
) -> graphql::Response {
    let api_schema = schema.api_schema();
    let operation = &doc.operation;
    let max_depth_result = match max_depth {
        MaxDepth::Check => {
            apollo_compiler::introspection::check_max_depth(&doc.executable, operation)
        }
        MaxDepth::Ignore => Ok(()),
    };
    let result = max_depth_result
        .and_then(|()| {
            apollo_compiler::request::coerce_variable_values(api_schema, operation, &variables)
        })
        .and_then(|variable_values| {
            apollo_compiler::introspection::partial_execute(
                api_schema,
                &schema.implementers_map,
                &doc.executable,
                operation,
                &variable_values,
            )
        });
    match result {
        Ok(response) => response.into(),
        Err(e) => {
            let error = e.to_graphql_error(&doc.executable.sources);
            graphql::Response::builder().error(error).build()
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use tower::Service as _;
    use tower::ServiceBuilder;
    use tower::ServiceExt as _;

    use super::IntrospectionRequest;
    use super::RejectMixedIntrospectionLayer;
    use super::introspection_service;
    use crate::Configuration;
    use crate::graphql;
    use crate::metrics::FutureMetricsExt as _;
    use crate::spec::Query;
    use crate::spec::Schema;

    #[tokio::test]
    async fn introspection_cache_hit() {
        async {
            let mut config = Configuration::default();
            config.supergraph.introspection = true;
            let schema = Arc::new(
                Schema::parse(include_str!("testdata/supergraph.graphql"), &config).unwrap(),
            );
            let query = "{ __schema { queryType { name } } }";

            let mut service = introspection_service(&config);

            // Two identical requests should result in one `cache.status = miss`, then one
            // `cache.status = hit` metric
            let mut responses = vec![];
            for _ in 0..2 {
                let document = Query::parse_document(query, None, &schema, &config).unwrap();
                let response = service
                    .ready()
                    .await
                    .unwrap()
                    .call(IntrospectionRequest {
                        schema: schema.clone(),
                        document,
                        variables: Default::default(),
                    })
                    .await
                    .unwrap();
                responses.push(response);
            }

            assert_eq!(responses[0], responses[1]);
            assert_eq!(
                responses[0].data,
                Some(serde_json_bytes::json!({
                    "__schema": {
                        "queryType": {
                            "name": "Query",
                        },
                    },
                })),
            );

            assert_histogram_count!(
                "apollo.platform.cache.lookup.duration",
                1u64,
                "cache.type" = "memory",
                "cache.name" = "introspection",
                "cache.status" = "miss"
            );
            assert_histogram_count!(
                "apollo.platform.cache.lookup.duration",
                1u64,
                "cache.type" = "memory",
                "cache.name" = "introspection",
                "cache.status" = "hit"
            );
        }
        .with_metrics()
        .await
    }

    #[tokio::test]
    async fn test_reject_mixed_introspection() {
        let (mock, mut handle) =
            tower_test::mock::pair::<IntrospectionRequest, graphql::Response>();

        // We expect one introspection request to go through, and the mixed request not to reach the
        // inner service.
        let driver = tokio::task::spawn(async move {
            let (_request, responder) = handle.next_request().await.unwrap();
            responder.send_response(
                graphql::Response::builder()
                    .data(serde_json_bytes::json!({
                        "__schema": {
                            "queryType": {
                                "name": "Query",
                            },
                        },
                    }))
                    .build(),
            );
        });

        let config = Configuration::default();
        let schema =
            Arc::new(Schema::parse(include_str!("testdata/supergraph.graphql"), &config).unwrap());

        let introspection_query = r#"{ __schema { queryType { name } } }"#;
        let introspection_document =
            Query::parse_document(introspection_query, None, &schema, &config).unwrap();

        let mixed_query = r#"{
            __schema { queryType { name } }
            me { id }
        }"#;
        let mixed_document = Query::parse_document(mixed_query, None, &schema, &config).unwrap();

        let mut service = ServiceBuilder::new()
            .layer(RejectMixedIntrospectionLayer::new())
            .service(mock);

        let mixed_response = service
            .ready()
            .await
            .unwrap()
            .call(IntrospectionRequest {
                schema: schema.clone(),
                document: mixed_document,
                variables: Default::default(),
            })
            .await
            .unwrap();
        assert!(mixed_response.contains_error_code("MIXED_INTROSPECTION"));

        let introspection_response = service
            .ready()
            .await
            .unwrap()
            .call(IntrospectionRequest {
                schema,
                document: introspection_document,
                variables: Default::default(),
            })
            .await
            .unwrap();
        assert!(introspection_response.errors.is_empty());

        crate::plugin::test::await_mock_driver(driver).await;
    }

    #[tokio::test]
    async fn test_single_aliased_root_typename() {
        let mut config = Configuration::default();
        config.supergraph.introspection = true;
        let schema =
            Arc::new(Schema::parse(include_str!("testdata/supergraph.graphql"), &config).unwrap());
        let query = "{ x: __typename }";
        let document = Query::parse_document(query, None, &schema, &config).unwrap();

        let service = introspection_service(&config);
        let response = service
            .oneshot(IntrospectionRequest {
                schema,
                document,
                variables: Default::default(),
            })
            .await
            .unwrap();

        assert_eq!(
            response.data,
            Some(serde_json_bytes::json!({
                "x": "Query",
            })),
        );
    }

    #[tokio::test]
    async fn test_two_root_typenames() {
        let mut config = Configuration::default();
        config.supergraph.introspection = true;

        let schema =
            Arc::new(Schema::parse(include_str!("testdata/supergraph.graphql"), &config).unwrap());
        let query = "{ x: __typename __typename }";
        let document = Query::parse_document(query, None, &schema, &config).unwrap();

        let service = introspection_service(&config);
        let response = service
            .oneshot(IntrospectionRequest {
                schema,
                document,
                variables: Default::default(),
            })
            .await
            .unwrap();

        assert_eq!(
            response.data,
            Some(serde_json_bytes::json!({
                "x": "Query",
                "__typename": "Query",
            })),
        );
    }
}
