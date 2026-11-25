//! Works out an operation's authorization requirements once, when its document is parsed.

use std::sync::Arc;
use std::task::Context as TaskContext;
use std::task::Poll;

use apollo_compiler::Schema;
use apollo_compiler::validation::Valid;
use apollo_federation::schema::AccessControlDirectiveNames;
use apollo_federation::schema::ValidFederationSchema;
use futures::future::BoxFuture;
use tower::Layer;
use tower::Service;

use crate::plugins::authorization::AuthorizationPlugin;
use crate::plugins::authorization::CacheKeyMetadata;
use crate::services::query_parsing::ParsedDocument;
use crate::services::query_parsing::ParsedDocumentInner;
use crate::services::query_parsing::Request;

/// Which `@authenticated`, `@requiresScopes` and `@policy` requirements the document's selected
/// operation has, given the supergraph and the names its authorization directives have there.
pub(crate) fn operation_requirements(
    doc: &ParsedDocumentInner,
    supergraph_schema: &Valid<Schema>,
    directives: &AccessControlDirectiveNames,
) -> CacheKeyMetadata {
    AuthorizationPlugin::generate_cache_metadata(
        &doc.executable,
        doc.operation.name.as_deref(),
        supergraph_schema,
        false,
        directives,
    )
}

/// Stores each parsed document's authorization requirements on the document.
///
/// Placed below the parse cache, this runs only when a query is parsed, and every cached copy of
/// the document carries its requirements. Parse errors pass through unchanged.
#[derive(Clone)]
pub(crate) struct AuthorizationRequirementsLayer {
    supergraph_schema: ValidFederationSchema,
    directives: Arc<AccessControlDirectiveNames>,
}

impl AuthorizationRequirementsLayer {
    pub(crate) fn new(
        supergraph_schema: ValidFederationSchema,
        directives: AccessControlDirectiveNames,
    ) -> Self {
        Self {
            supergraph_schema,
            directives: Arc::new(directives),
        }
    }
}

impl<S> Layer<S> for AuthorizationRequirementsLayer {
    type Service = AuthorizationRequirementsService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        AuthorizationRequirementsService {
            inner,
            supergraph_schema: self.supergraph_schema.clone(),
            directives: self.directives.clone(),
        }
    }
}

#[derive(Clone)]
pub(crate) struct AuthorizationRequirementsService<S> {
    inner: S,
    supergraph_schema: ValidFederationSchema,
    directives: Arc<AccessControlDirectiveNames>,
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
        let supergraph_schema = self.supergraph_schema.clone();
        let directives = self.directives.clone();

        Box::pin(async move {
            let doc = inner.call(req).await?;
            doc.authorization.get_or_init(|| {
                operation_requirements(&doc, supergraph_schema.schema(), &directives)
            });
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
    use crate::pipeline::tests::stage_stack::query_parsing_plugins_for;
    use crate::plugin::test::await_mock_driver;
    use crate::services::Plugins;
    use crate::spec::Query;
    use crate::spec::Schema;
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

        let result = AuthorizationRequirementsLayer::new(
            schema.federation_supergraph().schema.clone(),
            schema.authorization_directives.clone(),
        )
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

    /// Builds the pipeline's query parsing service with the authorization plugin installed.
    async fn query_parsing_service(
        sdl: &str,
        configuration: Configuration,
    ) -> crate::services::query_parsing::BoxCloneService {
        let schema = schema(sdl, &configuration);
        let plugins = query_parsing_plugins_for(sdl, &configuration).await;
        build_query_parsing_service(schema, Arc::new(configuration), &plugins)
    }

    #[tokio::test]
    async fn cached_documents_keep_their_requirements() {
        let service = query_parsing_service(POLICY_SCHEMA, Configuration::default()).await;

        let first = parse(service.clone(), "{ private { id } }").await;
        let second = parse(service, "{ private { id } }").await;

        assert!(
            Arc::ptr_eq(&first, &second),
            "the second request should be served from the parse cache"
        );
        assert_eq!(second.authorization.get(), Some(&requires_admin()));
    }

    #[tokio::test]
    #[should_panic(expected = "mandatory plugin")]
    async fn panics_without_the_authorization_plugin() {
        let configuration = Arc::new(Configuration::default());
        let schema = schema(POLICY_SCHEMA, &configuration);

        build_query_parsing_service(schema, configuration, &Plugins::default());
    }

    #[tokio::test]
    async fn not_installed_when_directives_are_disabled() {
        let configuration: Configuration = serde_json::from_value(serde_json::json!({
            "authorization": { "directives": { "enabled": false } }
        }))
        .unwrap();

        let doc = parse(
            query_parsing_service(POLICY_SCHEMA, configuration).await,
            "{ private { id } }",
        )
        .await;

        assert_eq!(doc.authorization.get(), None);
    }

    #[tokio::test]
    async fn not_installed_when_the_schema_has_no_authorization_directives() {
        let doc = parse(
            query_parsing_service(NO_AUTHORIZATION_SCHEMA, Configuration::default()).await,
            "{ me { id } }",
        )
        .await;

        assert_eq!(doc.authorization.get(), None);
    }
}
