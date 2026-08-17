//! Generates Apollo Studio extended usage references from the parsed document into the request
//! context.

use std::sync::Arc;

use futures::future::BoxFuture;
use http::StatusCode;
use tower::BoxError;
use tower::Service;

use crate::apollo_studio_interop::ExtendedReferenceStats;
use crate::apollo_studio_interop::generate_extended_references;
use crate::graphql::Error;
use crate::services::query_parsing::ParsedDocument;
use crate::services::supergraph;
use crate::spec::Schema;

/// Compute extended references for a GraphQL query, and store the result in request context.
///
/// # Context
/// Requires context key:
/// - [`ParsedDocument`] - If absent, short-circuits with an error response
///
/// Populates context key:
/// - [`ExtendedReferenceStats`]
pub(crate) struct ExtendedReferencesLayer {
    schema: Arc<Schema>,
}

impl ExtendedReferencesLayer {
    pub(crate) fn new(schema: Arc<Schema>) -> Self {
        Self { schema }
    }
}

impl<S> tower::Layer<S> for ExtendedReferencesLayer {
    type Service = ExtendedReferencesService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        ExtendedReferencesService {
            inner,
            schema: self.schema.clone(),
        }
    }
}

#[derive(Clone)]
pub(crate) struct ExtendedReferencesService<S> {
    inner: S,
    schema: Arc<Schema>,
}

impl<S> Service<supergraph::Request> for ExtendedReferencesService<S>
where
    S: Service<supergraph::Request, Response = supergraph::Response, Error = BoxError>
        + Clone
        + Send
        + 'static,
    S::Future: Send + 'static,
{
    type Response = supergraph::Response;
    type Error = BoxError;
    type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: supergraph::Request) -> Self::Future {
        let inner = self.inner.clone();
        let mut inner = std::mem::replace(&mut self.inner, inner);
        let schema = self.schema.clone();

        Box::pin(async move {
            let doc = req
                .context
                .extensions()
                .with_lock(|lock| lock.get::<ParsedDocument>().cloned());

            let doc = match doc {
                Some(doc) => doc,
                None => {
                    // We shouldn't ever reach here unless the pipeline was set up improperly
                    // (i.e. programmer error), but do something better than panicking just in case.
                    let errors = vec![
                        Error::request_error_builder()
                            .message("Cannot find executable document".to_string())
                            .extension_code("MISSING_EXECUTABLE_DOCUMENT")
                            .build(),
                    ];
                    return Ok(supergraph::Response::builder()
                        .errors(errors)
                        .status_code(StatusCode::INTERNAL_SERVER_ERROR)
                        .context(req.context)
                        .build()
                        .expect("response is valid"));
                }
            };

            let operation_name = req.supergraph_request.body().operation_name.clone();
            let stats = generate_extended_references(
                doc.executable.clone(),
                operation_name,
                schema.api_schema(),
                &req.supergraph_request.body().variables,
            );
            req.context
                .extensions()
                .with_lock(|lock| lock.insert::<ExtendedReferenceStats>(stats));

            inner.call(req).await
        })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use tower::ServiceBuilder;
    use tower::ServiceExt as _;

    use super::*;
    use crate::Configuration;
    use crate::Context;
    use crate::spec::Query;

    const SUPERGRAPH_SCHEMA: &str = include_str!("../../tests/fixtures/supergraph.graphql");

    #[tokio::test]
    async fn inserts_extended_reference_stats() {
        let query = "query { me { id } }";
        let config = Configuration::default();
        let schema = Arc::new(Schema::parse(SUPERGRAPH_SCHEMA, &config).unwrap());
        let doc = Query::parse_document(query, None, &schema, &config).unwrap();

        let (mock, mut handle) =
            tower_test::mock::pair::<supergraph::Request, supergraph::Response>();
        let driver = tokio::spawn(async move {
            let (req, responder) = handle.next_request().await.unwrap();
            responder.send_response(
                supergraph::Response::fake_builder()
                    .context(req.context)
                    .build()
                    .unwrap(),
            );
        });

        let mut service = ServiceBuilder::new()
            .layer(ExtendedReferencesLayer::new(schema))
            .service(mock);

        let context = Context::new();
        context
            .extensions()
            .with_lock(|lock| lock.insert::<ParsedDocument>(doc));

        let req = supergraph::Request::fake_builder()
            .query(query)
            .context(context)
            .build()
            .unwrap();

        let res = service.ready().await.unwrap().call(req).await.unwrap();

        res.context
            .extensions()
            .with_lock(|lock| lock.get::<ExtendedReferenceStats>().cloned())
            .expect("extended reference stats should have been inserted");

        crate::plugin::test::await_mock_driver(driver).await;
    }

    #[tokio::test]
    async fn errors_without_a_parsed_document() {
        let config = Configuration::default();
        let schema = Arc::new(Schema::parse(SUPERGRAPH_SCHEMA, &config).unwrap());

        // Inner service is never reached — the layer errors out before calling it.
        let (mock, handle) = tower_test::mock::pair::<supergraph::Request, supergraph::Response>();

        let mut service = ServiceBuilder::new()
            .layer(ExtendedReferencesLayer::new(schema))
            .service(mock);

        let mut response = service
            .ready()
            .await
            .unwrap()
            .call(
                supergraph::Request::fake_builder()
                    .query("query { me { id name } }")
                    .build()
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(
            StatusCode::INTERNAL_SERVER_ERROR,
            response.response.status()
        );
        let graphql_response = response.next_response().await.unwrap();
        assert!(graphql_response.contains_error_code("MISSING_EXECUTABLE_DOCUMENT"));

        crate::plugin::test::assert_no_mock_calls(handle).await;
    }
}
