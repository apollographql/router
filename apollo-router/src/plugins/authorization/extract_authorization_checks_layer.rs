//! Extracts authorization-directive usage (required scopes/policies) from the parsed document in
//! the request context.

use std::collections::HashMap;
use std::sync::Arc;

use futures::future::BoxFuture;
use http::StatusCode;
use tower::BoxError;
use tower::Service;

use crate::graphql::Error;
use crate::plugins::authorization::AUTHENTICATION_REQUIRED_KEY;
use crate::plugins::authorization::AuthorizationPlugin;
use crate::plugins::authorization::CacheKeyMetadata;
use crate::plugins::authorization::REQUIRED_POLICIES_KEY;
use crate::plugins::authorization::REQUIRED_SCOPES_KEY;
use crate::services::query_parsing::ParsedDocument;
use crate::services::supergraph;
use crate::spec::Schema;

/// Extract the use of `@authenticated`, `@requiredScopes` and `@policy` fields in a GraphQL query.
///
/// # Context
/// Requires context key:
/// - [`ParsedDocument`] - If absent, short-circuits with an error response
///
/// Populates context keys:
/// - "apollo::authorization::authentication_required"
/// - "apollo::authorization::required_scopes"
/// - "apollo::authorization::required_policies"
pub(crate) struct ExtractAuthorizationChecksLayer {
    schema: Arc<Schema>,
}

impl ExtractAuthorizationChecksLayer {
    pub(crate) fn new(schema: Arc<Schema>) -> Self {
        Self { schema }
    }
}

impl<S> tower::Layer<S> for ExtractAuthorizationChecksLayer {
    type Service = ExtractAuthorizationChecksService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        ExtractAuthorizationChecksService {
            inner,
            schema: self.schema.clone(),
        }
    }
}

#[derive(Clone)]
pub(crate) struct ExtractAuthorizationChecksService<S> {
    inner: S,
    schema: Arc<Schema>,
}

impl<S> Service<supergraph::Request> for ExtractAuthorizationChecksService<S>
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
                    // (i.e. programmer error: this layer must run after `ParseQueryLayer`), but
                    // do something better than panicking just in case.
                    let errors = vec![
                        Error::builder()
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

            let operation_name = req.supergraph_request.body().operation_name.as_deref();

            let CacheKeyMetadata {
                is_authenticated,
                scopes,
                policies,
            } = AuthorizationPlugin::generate_cache_metadata(
                &doc.executable,
                operation_name,
                schema.supergraph_schema(),
                false,
            );
            if is_authenticated {
                req.context
                    .insert(AUTHENTICATION_REQUIRED_KEY, true)
                    .unwrap();
            }

            if !scopes.is_empty() {
                req.context.insert(REQUIRED_SCOPES_KEY, scopes).unwrap();
            }

            if !policies.is_empty() {
                let policies: HashMap<String, Option<bool>> =
                    policies.into_iter().map(|policy| (policy, None)).collect();
                req.context.insert(REQUIRED_POLICIES_KEY, policies).unwrap();
            }

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
    use crate::plugins::authorization::REQUIRED_SCOPES_KEY;
    use crate::spec::Query;

    const SUPERGRAPH_AUTH_SCHEMA: &str =
        include_str!("../../../tests/fixtures/supergraph-auth.graphql");

    #[tokio::test]
    async fn extracts_scopes_from_parsed_document() {
        let query = "query { me { id name } }";
        let config = Configuration::default();
        let schema = Arc::new(Schema::parse(SUPERGRAPH_AUTH_SCHEMA, &config).unwrap());
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
            .layer(ExtractAuthorizationChecksLayer::new(schema))
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

        assert!(
            res.context.contains_key(REQUIRED_SCOPES_KEY),
            "required scopes should have been inserted into context"
        );

        crate::plugin::test::await_mock_driver(driver).await;
    }

    #[tokio::test]
    async fn errors_without_a_parsed_document() {
        let config = Configuration::default();
        let schema = Arc::new(Schema::parse(SUPERGRAPH_AUTH_SCHEMA, &config).unwrap());

        // Inner service is never reached — the layer errors out before calling it.
        let (mock, handle) = tower_test::mock::pair::<supergraph::Request, supergraph::Response>();

        let mut service = ServiceBuilder::new()
            .layer(ExtractAuthorizationChecksLayer::new(schema))
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
