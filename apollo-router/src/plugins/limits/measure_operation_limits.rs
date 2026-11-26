use std::task::Context as TaskContext;
use std::task::Poll;

use futures::future::BoxFuture;
use tower::Layer;
use tower::Service;

use crate::plugins::limits::operation_limits;
use crate::services::query_parsing::ParsedDocument;
use crate::services::query_parsing::Request;

/// Stores each parsed document's operation limit counts on the document.
///
/// Errors from the inner service pass through unchanged.
#[derive(Clone)]
pub(crate) struct MeasureOperationLimitsLayer;

impl<S> Layer<S> for MeasureOperationLimitsLayer {
    type Service = MeasureOperationLimitsService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        MeasureOperationLimitsService { inner }
    }
}

#[derive(Clone)]
pub(crate) struct MeasureOperationLimitsService<S> {
    inner: S,
}

impl<S> Service<Request> for MeasureOperationLimitsService<S>
where
    S: Service<Request, Response = ParsedDocument>,
    S::Future: Send + 'static,
{
    type Response = ParsedDocument;
    type Error = S::Error;
    type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn poll_ready(&mut self, cx: &mut TaskContext<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: Request) -> Self::Future {
        let parsed = self.inner.call(req);

        Box::pin(async move {
            let doc = parsed.await?;
            doc.operation_limits
                .get_or_init(|| operation_limits::measure(&doc.executable, &doc.operation));
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
    use crate::pipeline::tests::stage_stack::query_parsing_plugins;
    use crate::plugin::test::await_mock_driver;
    use crate::plugins::limits::operation_limits::OperationLimits;
    use crate::spec::Query;
    use crate::spec::Schema;
    use crate::spec::SpecError;

    fn schema(configuration: &Configuration) -> Arc<Schema> {
        Arc::new(
            Schema::parse(
                include_str!("../../testdata/supergraph.graphql"),
                configuration,
            )
            .unwrap(),
        )
    }

    async fn call_layer(
        query: &str,
        operation_name: Option<&str>,
    ) -> Result<ParsedDocument, BoxError> {
        let configuration = Configuration::default();
        let schema = schema(&configuration);

        let (mock, mut handle) = tower_test::mock::pair::<Request, ParsedDocument>();
        let driver = tokio::spawn(async move {
            let (req, responder) = handle.next_request().await.unwrap();
            match Query::parse_document(
                &req.query,
                req.operation_name.as_deref(),
                &schema,
                &configuration,
            ) {
                Ok(doc) => responder.send_response(doc),
                Err(err) => responder.send_error(err),
            }
        });

        let result = MeasureOperationLimitsLayer
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
    async fn stores_the_counts_of_the_requested_operation() {
        let query = indoc::indoc! {"
            query Small { me { id } }
            query Large { a: me { id } b: me { id name } topProducts { upc } }
        "};

        let small = call_layer(query, Some("Small")).await.unwrap();
        let large = call_layer(query, Some("Large")).await.unwrap();

        assert_eq!(
            small.operation_limits.get(),
            Some(&OperationLimits {
                depth: 2,
                height: 2,
                root_fields: 1,
                aliases: 0,
            })
        );
        assert_eq!(
            large.operation_limits.get(),
            Some(&OperationLimits {
                depth: 2,
                height: 7,
                root_fields: 3,
                aliases: 2,
            })
        );
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
    async fn cached_documents_keep_their_counts() {
        let configuration = Arc::new(Configuration::default());
        let service = build_query_parsing_service(
            schema(&configuration),
            configuration,
            &query_parsing_plugins().await,
        );

        let first = service
            .clone()
            .oneshot(Request::new("{ me { id } }".to_string(), None))
            .await
            .unwrap();
        let second = service
            .oneshot(Request::new("{ me { id } }".to_string(), None))
            .await
            .unwrap();

        assert!(
            Arc::ptr_eq(&first, &second),
            "the second request should be served from the parse cache"
        );
        assert!(second.operation_limits.get().is_some());
    }
}
