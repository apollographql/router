//! Extracts authorization-directive usage (required scopes/policies) from the parsed document in
//! the request context.

use std::collections::HashMap;

use futures::future::BoxFuture;
use http::StatusCode;
use tower::BoxError;
use tower::Service;

use crate::graphql::Error;
use crate::plugins::authorization::AUTHENTICATION_REQUIRED_KEY;
use crate::plugins::authorization::CacheKeyMetadata;
use crate::plugins::authorization::REQUIRED_POLICIES_KEY;
use crate::plugins::authorization::REQUIRED_SCOPES_KEY;
use crate::services::query_parsing::ParsedDocument;
use crate::services::supergraph;

/// Extract the use of `@authenticated`, `@requiredScopes` and `@policy` fields in a GraphQL query.
///
/// # Context
/// Requires context key:
/// - [`ParsedDocument`], carrying the requirements the query parsing service stored on it. If
///   either is absent, short-circuits with an internal server error.
///
/// Populates context keys:
/// - "apollo::authorization::authentication_required"
/// - "apollo::authorization::required_scopes"
/// - "apollo::authorization::required_policies"
#[derive(Clone, Default)]
pub(crate) struct ExtractAuthorizationChecksLayer;

impl<S> tower::Layer<S> for ExtractAuthorizationChecksLayer {
    type Service = ExtractAuthorizationChecksService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        ExtractAuthorizationChecksService { inner }
    }
}

#[derive(Clone)]
pub(crate) struct ExtractAuthorizationChecksService<S> {
    inner: S,
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

        Box::pin(async move {
            let doc = req
                .context
                .extensions()
                .with_lock(|lock| lock.get::<ParsedDocument>().cloned());

            // Neither should ever be missing unless the pipeline was set up improperly (i.e.
            // programmer error: this layer must run after `ParseQueryLayer`, and the
            // authorization plugin provides it only together with the layer that stores the
            // requirements), but do something better than panicking just in case. A missing
            // value never means the operation requires nothing.
            let Some(doc) = doc else {
                return Ok(internal_error(
                    req,
                    "Cannot find executable document",
                    "MISSING_EXECUTABLE_DOCUMENT",
                ));
            };
            let Some(requirements) = doc.authorization.get() else {
                return Ok(internal_error(
                    req,
                    "Cannot find the operation's authorization requirements",
                    "MISSING_AUTHORIZATION_REQUIREMENTS",
                ));
            };
            let CacheKeyMetadata {
                is_authenticated,
                scopes,
                policies,
            } = requirements.clone();
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

fn internal_error(req: supergraph::Request, message: &str, code: &str) -> supergraph::Response {
    supergraph::Response::builder()
        .errors(vec![
            Error::builder()
                .message(message.to_string())
                .extension_code(code)
                .build(),
        ])
        .status_code(StatusCode::INTERNAL_SERVER_ERROR)
        .context(req.context)
        .build()
        .expect("response is valid")
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::sync::Arc;

    use tower::ServiceBuilder;
    use tower::ServiceExt as _;

    use super::*;
    use crate::Configuration;
    use crate::Context;
    use crate::plugins::authorization::REQUIRED_SCOPES_KEY;
    use crate::services::query_parsing::authorization_requirements::operation_requirements;
    use crate::spec::Query;
    use crate::spec::Schema;

    /// Parses `query` and stores its requirements on the document, as the query parsing
    /// service does.
    fn parse(query: &str, schema: &Schema, config: &Configuration) -> ParsedDocument {
        let doc = Query::parse_document(query, None, schema, config).unwrap();
        doc.authorization
            .set(operation_requirements(
                &doc,
                schema.supergraph_schema(),
                &schema.authorization_directives,
            ))
            .unwrap();
        doc
    }

    const REQUIRES_SCOPES_SCHEMA: &str =
        include_str!("../../../tests/fixtures/supergraph-auth.graphql");
    const POLICY_SCHEMA: &str =
        include_str!("../../../tests/fixtures/directives/policy/policy_basic_schema.graphql");
    const AUTHENTICATED_SCHEMA: &str =
        include_str!("../../../tests/integration/fixtures/authenticated_directive.graphql");

    #[tokio::test]
    async fn extracts_scopes() {
        let query = "query { me { id name } }";
        let config = Configuration::default();
        let schema = Arc::new(Schema::parse(REQUIRES_SCOPES_SCHEMA, &config).unwrap());
        let doc = parse(query, &schema, &config);

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
            .layer(ExtractAuthorizationChecksLayer)
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

        assert_eq!(
            res.context
                .get::<_, HashSet<String>>(REQUIRED_SCOPES_KEY)
                .unwrap(),
            Some(HashSet::from([
                "profile".to_string(),
                "read:name".to_string(),
                "read:user".to_string()
            ])),
            "required scopes should have been inserted into context"
        );

        crate::plugin::test::await_mock_driver(driver).await;
    }

    #[tokio::test]
    async fn extracts_authenticated() {
        let query = "query { products(limit: 1) { price } }";
        let config = Configuration::default();
        let schema = Arc::new(Schema::parse(AUTHENTICATED_SCHEMA, &config).unwrap());
        let doc = parse(query, &schema, &config);

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
            .layer(ExtractAuthorizationChecksLayer)
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

        assert_eq!(
            res.context
                .get_json_value(AUTHENTICATION_REQUIRED_KEY)
                .unwrap(),
            serde_json_bytes::json!(true),
            "required scopes should have been inserted into context"
        );

        crate::plugin::test::await_mock_driver(driver).await;
    }

    #[tokio::test]
    async fn extracts_policies() {
        let query = "query { private { id } }";
        let config = Configuration::default();
        let schema = Arc::new(Schema::parse(POLICY_SCHEMA, &config).unwrap());
        let doc = parse(query, &schema, &config);

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
            .layer(ExtractAuthorizationChecksLayer)
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

        assert_eq!(
            res.context.get_json_value(REQUIRED_POLICIES_KEY).unwrap(),
            serde_json_bytes::json!({ "admin": null }),
            "required policies should have been inserted into context"
        );

        crate::plugin::test::await_mock_driver(driver).await;
    }

    /// Runs the layer for one request carrying `doc`, and returns the request's context.
    async fn extract(doc: ParsedDocument) -> Context {
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
        let service = ServiceBuilder::new()
            .layer(ExtractAuthorizationChecksLayer)
            .service(mock);

        let context = Context::new();
        context
            .extensions()
            .with_lock(|lock| lock.insert::<ParsedDocument>(doc));
        let req = supergraph::Request::fake_builder()
            .query("query { private { id } }")
            .context(context)
            .build()
            .unwrap();

        let context = service.oneshot(req).await.unwrap().context;
        crate::plugin::test::await_mock_driver(driver).await;
        context
    }

    #[tokio::test]
    async fn uses_requirements_stored_on_the_document() {
        let config = Configuration::default();
        let schema = Arc::new(Schema::parse(POLICY_SCHEMA, &config).unwrap());
        let doc =
            Query::parse_document("query { private { id } }", None, &schema, &config).unwrap();
        doc.authorization
            .set(CacheKeyMetadata {
                policies: vec!["stored".to_string()],
                ..Default::default()
            })
            .unwrap();

        let context = extract(doc).await;

        assert_eq!(
            context.get_json_value(REQUIRED_POLICIES_KEY).unwrap(),
            serde_json_bytes::json!({ "stored": null }),
        );
    }

    #[tokio::test]
    async fn requests_sharing_a_document_get_their_own_policy_results() {
        let config = Configuration::default();
        let schema = Arc::new(Schema::parse(POLICY_SCHEMA, &config).unwrap());
        let doc =
            Query::parse_document("query { private { id } }", None, &schema, &config).unwrap();
        doc.authorization
            .set(CacheKeyMetadata {
                policies: vec!["admin".to_string()],
                ..Default::default()
            })
            .unwrap();

        let first = extract(doc.clone()).await;
        // What a coprocessor or Rhai script does once it has evaluated the policy.
        first
            .insert(
                REQUIRED_POLICIES_KEY,
                HashMap::from([("admin".to_string(), Some(true))]),
            )
            .unwrap();
        let second = extract(doc).await;

        assert_eq!(
            second.get_json_value(REQUIRED_POLICIES_KEY).unwrap(),
            serde_json_bytes::json!({ "admin": null }),
        );
    }

    /// Sends `req` through the layer, checks that it answered with an internal server error
    /// carrying `code`, and that the inner service was never reached.
    async fn assert_internal_error(req: supergraph::Request, code: &str) {
        let (mock, handle) = tower_test::mock::pair::<supergraph::Request, supergraph::Response>();
        let service = ServiceBuilder::new()
            .layer(ExtractAuthorizationChecksLayer)
            .service(mock);

        let mut response = service.oneshot(req).await.unwrap();

        assert_eq!(
            StatusCode::INTERNAL_SERVER_ERROR,
            response.response.status()
        );
        let graphql_response = response.next_response().await.unwrap();
        assert!(graphql_response.contains_error_code(code));
        crate::plugin::test::assert_no_mock_calls(handle).await;
    }

    #[tokio::test]
    async fn errors_without_document() {
        let req = supergraph::Request::fake_builder()
            .query("query { me { id name } }")
            .build()
            .unwrap();

        assert_internal_error(req, "MISSING_EXECUTABLE_DOCUMENT").await;
    }

    #[tokio::test]
    async fn errors_without_stored_requirements() {
        let query = "query { private { id } }";
        let config = Configuration::default();
        let schema = Schema::parse(POLICY_SCHEMA, &config).unwrap();
        let doc = Query::parse_document(query, None, &schema, &config).unwrap();
        let context = Context::new();
        context
            .extensions()
            .with_lock(|lock| lock.insert::<ParsedDocument>(doc));
        let req = supergraph::Request::fake_builder()
            .query(query)
            .context(context)
            .build()
            .unwrap();

        assert_internal_error(req, "MISSING_AUTHORIZATION_REQUIREMENTS").await;
    }
}
