//! Works out an operation's authorization requirements once, when its document is parsed.

use std::sync::Arc;
use std::task::Context as TaskContext;
use std::task::Poll;

use futures::future::BoxFuture;
use tower::Layer;
use tower::Service;

use crate::plugins::authorization::AuthorizationPlugin;
use crate::plugins::authorization::CacheKeyMetadata;
use crate::services::query_parsing::ParsedDocument;
use crate::services::query_parsing::ParsedDocumentInner;
use crate::services::query_parsing::Request;
use crate::spec::Schema;

/// Which `@authenticated`, `@requiresScopes` and `@policy` requirements the document's selected
/// operation has.
pub(crate) fn operation_requirements(
    doc: &ParsedDocumentInner,
    schema: &Schema,
) -> CacheKeyMetadata {
    AuthorizationPlugin::generate_cache_metadata(
        &doc.executable,
        doc.operation.name.as_deref(),
        schema.supergraph_schema(),
        false,
        &schema.authorization_directives,
    )
}

/// Stores each parsed document's authorization requirements on the document.
///
/// Placed below the parse cache, this runs only when a query is parsed, and every cached copy of
/// the document carries its requirements. Parse errors pass through unchanged.
#[derive(Clone)]
pub(crate) struct AuthorizationRequirementsLayer {
    schema: Arc<Schema>,
}

impl AuthorizationRequirementsLayer {
    pub(crate) fn new(schema: Arc<Schema>) -> Self {
        Self { schema }
    }
}

impl<S> Layer<S> for AuthorizationRequirementsLayer {
    type Service = AuthorizationRequirementsService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        AuthorizationRequirementsService {
            inner,
            schema: self.schema.clone(),
        }
    }
}

#[derive(Clone)]
pub(crate) struct AuthorizationRequirementsService<S> {
    inner: S,
    schema: Arc<Schema>,
}

impl<S> Service<Request> for AuthorizationRequirementsService<S>
where
    S: Service<Request, Response = ParsedDocument> + Clone + Send + 'static,
    S::Future: Send + 'static,
{
    type Response = ParsedDocument;
    type Error = S::Error;
    type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn poll_ready(&mut self, cx: &mut TaskContext<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: Request) -> Self::Future {
        let inner = self.inner.clone();
        let mut inner = std::mem::replace(&mut self.inner, inner);
        let schema = self.schema.clone();

        Box::pin(async move {
            let doc = inner.call(req).await?;
            doc.authorization
                .get_or_init(|| operation_requirements(&doc, &schema));
            Ok(doc)
        })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use tower::BoxError;
    use tower::ServiceExt as _;

    use super::*;
    use crate::Configuration;
    use crate::pipeline::build_query_parsing_service;
    use crate::plugin::test::await_mock_driver;
    use crate::spec::Query;
    use crate::spec::SpecError;

    const POLICY_SCHEMA: &str =
        include_str!("../../../tests/fixtures/directives/policy/policy_basic_schema.graphql");
    const NO_AUTHORIZATION_SCHEMA: &str = include_str!("../../../testing_schema.graphql");

    fn requires_admin() -> CacheKeyMetadata {
        CacheKeyMetadata {
            policies: vec!["admin".to_string()],
            ..Default::default()
        }
    }

    fn schema(sdl: &str, configuration: &Configuration) -> Arc<Schema> {
        Arc::new(Schema::parse(sdl, configuration).unwrap())
    }

    /// Sends one request through the layer over a mock parser, which answers it by parsing the
    /// query as the query parsing service does.
    async fn call_layer(
        query: &str,
        operation_name: Option<&str>,
    ) -> Result<ParsedDocument, BoxError> {
        let configuration = Configuration::default();
        let schema = schema(POLICY_SCHEMA, &configuration);

        let (mock, mut handle) = tower_test::mock::pair::<Request, ParsedDocument>();
        let parser_schema = schema.clone();
        let driver = tokio::spawn(async move {
            let (req, responder) = handle.next_request().await.unwrap();
            match Query::parse_document(
                &req.query,
                req.operation_name.as_deref(),
                &parser_schema,
                &configuration,
            ) {
                Ok(doc) => responder.send_response(doc),
                Err(err) => responder.send_error(err),
            }
        });

        let result = AuthorizationRequirementsLayer::new(schema)
            .layer(mock)
            .oneshot(Request::new(
                query.to_string(),
                operation_name.map(str::to_string),
            ))
            .await;
        await_mock_driver(driver).await;
        result
    }

    #[tokio::test]
    async fn stores_requirements_on_the_parsed_document() {
        let doc = call_layer("{ private { id } }", None).await.unwrap();

        assert_eq!(doc.authorization.get(), Some(&requires_admin()));
    }

    #[tokio::test]
    async fn passes_parse_errors_through() {
        let err = call_layer("{ doesNotExist }", None).await.unwrap_err();

        assert!(matches!(
            err.downcast_ref::<SpecError>(),
            Some(SpecError::ValidationError(_))
        ));
    }

    #[tokio::test]
    async fn stores_requirements_of_the_requested_operation() {
        let query = "query Public { public { id } } query Private { private { id } }";

        let public = call_layer(query, Some("Public")).await.unwrap();
        let private = call_layer(query, Some("Private")).await.unwrap();

        assert_eq!(
            public.authorization.get(),
            Some(&CacheKeyMetadata::default())
        );
        assert_eq!(private.authorization.get(), Some(&requires_admin()));
    }

    /// Sends one request through the query parsing service the pipeline builds.
    async fn parse(
        service: crate::services::query_parsing::BoxCloneService,
        query: &str,
    ) -> ParsedDocument {
        service
            .oneshot(Request::new(query.to_string(), None))
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn cached_documents_keep_their_requirements() {
        let configuration = Arc::new(Configuration::default());
        let schema = schema(POLICY_SCHEMA, &configuration);
        let service = build_query_parsing_service(schema, configuration, &Default::default());

        let first = parse(service.clone(), "{ private { id } }").await;
        let second = parse(service, "{ private { id } }").await;

        assert!(
            Arc::ptr_eq(&first, &second),
            "the second request should be served from the parse cache"
        );
        assert_eq!(second.authorization.get(), Some(&requires_admin()));
    }

    #[tokio::test]
    async fn not_installed_when_directives_are_disabled() {
        let configuration: Configuration = serde_json::from_value(serde_json::json!({
            "authorization": { "directives": { "enabled": false } }
        }))
        .unwrap();
        let configuration = Arc::new(configuration);
        let schema = schema(POLICY_SCHEMA, &configuration);

        let doc = parse(
            build_query_parsing_service(schema, configuration, &Default::default()),
            "{ private { id } }",
        )
        .await;

        assert_eq!(doc.authorization.get(), None);
    }

    #[tokio::test]
    async fn not_installed_when_the_schema_has_no_authorization_directives() {
        let configuration = Arc::new(Configuration::default());
        let schema = schema(NO_AUTHORIZATION_SCHEMA, &configuration);

        let doc = parse(
            build_query_parsing_service(schema, configuration, &Default::default()),
            "{ me { id } }",
        )
        .await;

        assert_eq!(doc.authorization.get(), None);
    }
}
