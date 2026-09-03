use std::collections::HashMap;
use std::sync::OnceLock;
use std::task::Context as TaskContext;
use std::task::Poll;

use apollo_compiler::Name;
use apollo_compiler::executable::ExecutableDocument;
use apollo_compiler::executable::Selection;
use apollo_compiler::executable::SelectionSet;
use apollo_compiler::response::GraphQLError;
use apollo_compiler::validation::Valid;
use futures::future::BoxFuture;
use tower::Layer;
use tower::Service;

use crate::compute_job::MaybeBackPressureError;
use crate::error::ValidationErrors;
use crate::services::query_parsing::ParsedDocument;
use crate::services::query_parsing::Request;
use crate::services::query_parsing::ServiceError;
use crate::spec::SpecError;

const ENV_DISABLE_RECURSIVE_SELECTIONS_CHECK: &str =
    "APOLLO_ROUTER_DISABLE_SECURITY_RECURSIVE_SELECTIONS_CHECK";
/// Should we enforce the recursive selections limit? Default true, can be toggled off with an
/// environment variable.
///
/// Disabling this check is very much not advisable and we don't expect that anyone will need to do
/// it. In the extremely unlikely case that the new protection breaks someone's legitimate queries,
/// though, they could temporarily disable this individual limit so they can still benefit from the
/// other new limits, until we improve the detection.
pub(crate) fn recursive_selections_check_enabled() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| {
        let disabled =
            std::env::var(ENV_DISABLE_RECURSIVE_SELECTIONS_CHECK).as_deref() == Ok("true");

        !disabled
    })
}

/// Measure the number of selections that would be encountered if we walked the given selection
/// set while recursing into fragment spreads, and add it to the given count. `None` is returned
/// instead if this number exceeds `max_recursive_selections`.
///
/// This function assumes that fragments referenced by spreads exist and that they don't form
/// cycles. If a fragment spread appears multiple times for the same named fragment, it is
/// counted multiple times.
fn count_recursive_selections<'a>(
    document: &'a Valid<ExecutableDocument>,
    fragment_cache: &mut HashMap<&'a Name, u32>,
    selection_set: &'a SelectionSet,
    mut count: u32,
    max_recursive_selections: u32,
) -> Option<u32> {
    for selection in &selection_set.selections {
        count = count
            .checked_add(1)
            .take_if(|v| *v <= max_recursive_selections)?;
        match selection {
            Selection::Field(field) => {
                count = count_recursive_selections(
                    document,
                    fragment_cache,
                    &field.selection_set,
                    count,
                    max_recursive_selections,
                )?;
            }
            Selection::InlineFragment(fragment) => {
                count = count_recursive_selections(
                    document,
                    fragment_cache,
                    &fragment.selection_set,
                    count,
                    max_recursive_selections,
                )?;
            }
            Selection::FragmentSpread(fragment) => {
                let name = &fragment.fragment_name;
                if let Some(cached) = fragment_cache.get(name) {
                    count = count
                        .checked_add(*cached)
                        .take_if(|v| *v <= max_recursive_selections)?;
                } else {
                    let old_count = count;
                    count = count_recursive_selections(
                        document,
                        fragment_cache,
                        &document
                            .fragments
                            .get(&fragment.fragment_name)
                            .expect("validation should have ensured referenced fragments exist")
                            .selection_set,
                        count,
                        max_recursive_selections,
                    )?;
                    fragment_cache.insert(name, count - old_count);
                };
            }
        }
    }
    Some(count)
}

/// Enforces the recursive selections limit.
#[derive(Clone)]
pub(crate) struct LimitRecursiveSelectionLayer {
    max_recursive_selections: u32,
    warn_only: bool,
}

impl LimitRecursiveSelectionLayer {
    pub(crate) fn new(max_recursive_selections: u32, warn_only: bool) -> Self {
        Self {
            max_recursive_selections,
            warn_only,
        }
    }
}

impl<S> Layer<S> for LimitRecursiveSelectionLayer {
    type Service = LimitRecursiveSelectionService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        LimitRecursiveSelectionService {
            inner,
            max_recursive_selections: self.max_recursive_selections,
            warn_only: self.warn_only,
        }
    }
}

#[derive(Clone)]
pub(crate) struct LimitRecursiveSelectionService<S> {
    inner: S,
    max_recursive_selections: u32,
    warn_only: bool,
}

impl<S> Service<Request> for LimitRecursiveSelectionService<S>
where
    S: Service<Request, Response = ParsedDocument, Error = ServiceError> + Clone + Send + 'static,
    S::Future: Send + 'static,
{
    type Response = ParsedDocument;
    type Error = ServiceError;
    type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn poll_ready(&mut self, cx: &mut TaskContext<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: Request) -> Self::Future {
        let inner = self.inner.clone();
        let mut inner = std::mem::replace(&mut self.inner, inner);
        let max_recursive_selections = self.max_recursive_selections;
        let warn_only = self.warn_only;
        let operation_name = req.operation_name.clone();

        Box::pin(async move {
            let doc = inner.call(req).await?;

            let recursive_selections = count_recursive_selections(
                &doc.executable,
                &mut Default::default(),
                &doc.operation.selection_set,
                0,
                max_recursive_selections,
            );
            if recursive_selections.is_none() {
                if recursive_selections_check_enabled() {
                    if warn_only {
                        tracing::warn!(
                            operation_name = ?operation_name,
                            max_recursive_selections,
                            "operation exceeded maximum recursive selections limit",
                        );
                    } else {
                        return Err(MaybeBackPressureError::PermanentError(
                            SpecError::ValidationError(ValidationErrors {
                                errors: vec![GraphQLError {
                                    message:
                                        "Maximum recursive selections limit exceeded in this operation"
                                            .to_string(),
                                    locations: Default::default(),
                                    path: Default::default(),
                                    extensions: Default::default(),
                                }],
                            }),
                        ));
                    }
                } else {
                    tracing::info!(
                        operation_name = ?operation_name,
                        max_recursive_selections,
                        "operation exceeded maximum recursive selections limit, but limit is forcefully disabled",
                    );
                }
            }

            Ok(doc)
        })
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Arc;

    use apollo_compiler::ExecutableDocument;
    use apollo_compiler::Schema;
    use tower::ServiceBuilder;
    use tower::ServiceExt as _;

    use super::*;
    use crate::Configuration;

    const SUPERGRAPH_SCHEMA: &str = include_str!("../../../testing_schema.graphql");

    fn parse(
        schema_sdl: &str,
        query: &str,
    ) -> apollo_compiler::validation::Valid<ExecutableDocument> {
        let schema = Schema::parse_and_validate(schema_sdl, "./").unwrap();
        ExecutableDocument::parse_and_validate(&schema, query, "./").unwrap()
    }

    const SCHEMA: &str = "type Query { a: A } type A { b: B } type B { c: String }";

    #[test]
    fn count_recursive_selections_simple_query() {
        let doc = parse(SCHEMA, "query { a { b { c } } }");
        let op = doc.operations.get(None).unwrap();
        let count =
            count_recursive_selections(&doc, &mut HashMap::new(), &op.selection_set, 0, 10_000_000);
        // a(1) + b(2) + c(3) = 3 selections
        assert_eq!(count, Some(3));
    }

    #[test]
    fn count_recursive_selections_exceeds_limit() {
        let doc = parse(SCHEMA, "query { a { b { c } } }");
        let op = doc.operations.get(None).unwrap();
        let count = count_recursive_selections(
            &doc,
            &mut HashMap::new(),
            &op.selection_set,
            0,
            2, // limit is 2, but query has 3 selections
        );
        assert_eq!(count, None);
    }

    #[test]
    fn count_recursive_selections_exactly_at_limit() {
        let doc = parse(SCHEMA, "query { a { b { c } } }");
        let op = doc.operations.get(None).unwrap();
        let count = count_recursive_selections(&doc, &mut HashMap::new(), &op.selection_set, 0, 3);
        assert_eq!(count, Some(3));
    }

    #[test]
    fn count_recursive_selections_with_fragment_spread() {
        let schema = "type Query { a: A } type A { b: String, c: String }";
        let query = "query { a { ...F } } fragment F on A { b c }";
        let doc = parse(schema, query);
        let op = doc.operations.get(None).unwrap();

        // Under a generous limit: a(1) + spread(2) + b(3) + c(4) = 4
        let count =
            count_recursive_selections(&doc, &mut HashMap::new(), &op.selection_set, 0, 100);
        assert_eq!(count, Some(4));

        // With a tight limit that the fragment exceeds
        let count = count_recursive_selections(&doc, &mut HashMap::new(), &op.selection_set, 0, 3);
        assert_eq!(count, None);
    }

    #[test]
    fn count_recursive_selections_with_inline_fragment() {
        let schema = "type Query { a: A } type A { b: String }";
        let query = "query { a { ... on A { b } } }";
        let doc = parse(schema, query);
        let op = doc.operations.get(None).unwrap();
        // a(1) + inline_fragment(2) + b(3) = 3
        let count =
            count_recursive_selections(&doc, &mut HashMap::new(), &op.selection_set, 0, 100);
        assert_eq!(count, Some(3));
    }

    #[test]
    fn count_recursive_selections_fragment_reused() {
        let schema = "type Query { a: A, a2: A } type A { b: String }";
        let query = "query { a { ...F } a2 { ...F } } fragment F on A { b }";
        let doc = parse(schema, query);
        let op = doc.operations.get(None).unwrap();

        // a(1) + spread(2) + b(3) + a2(4) + spread(5) + b_cached(6) = 6
        let count =
            count_recursive_selections(&doc, &mut HashMap::new(), &op.selection_set, 0, 100);
        assert_eq!(count, Some(6));
    }

    #[test]
    fn count_recursive_selections_limit_zero_always_exceeds() {
        let doc = parse("type Query { a: String }", "query { a }");
        let op = doc.operations.get(None).unwrap();
        let count = count_recursive_selections(&doc, &mut HashMap::new(), &op.selection_set, 0, 0);
        assert_eq!(count, None);
    }

    fn downcast_mock_err(err: tower::BoxError) -> ServiceError {
        *err.downcast()
            .expect("mock should only return ServiceErrors")
    }

    async fn mock_parser(
        mut handle: tower_test::mock::Handle<Request, ParsedDocument>,
        schema: Arc<crate::spec::Schema>,
        config: Arc<Configuration>,
    ) {
        let (req, responder) = handle.next_request().await.unwrap();
        match crate::spec::Query::parse_document(
            &req.query,
            req.operation_name.as_deref(),
            &schema,
            &config,
        ) {
            Ok(document) => responder.send_response(document),
            Err(err) => responder.send_error(err),
        }
    }

    // This query has 3 selections
    const QUERY: &str = "query { me { id name } }";

    #[tokio::test]
    async fn passes_through_when_within_limit() {
        let config = Arc::new(Configuration::default());
        let schema = Arc::new(crate::spec::Schema::parse(SUPERGRAPH_SCHEMA, &config).unwrap());

        let (mock, mut handle) = tower_test::mock::pair::<Request, ParsedDocument>();
        let mut service = ServiceBuilder::new()
            .layer(LimitRecursiveSelectionLayer::new(10, false))
            .map_err(downcast_mock_err)
            .service(mock);

        handle.allow(1);
        let driver = tokio::spawn(mock_parser(handle, schema, config));

        let _res = service
            .ready()
            .await
            .unwrap()
            .call(Request::new(QUERY.to_string(), None))
            .await
            .unwrap();

        drop(service);
        crate::plugin::test::await_mock_driver(driver).await;
    }

    #[tokio::test]
    async fn errors_when_limit_exceeded() {
        let config = Arc::new(Configuration::default());
        let schema = Arc::new(crate::spec::Schema::parse(SUPERGRAPH_SCHEMA, &config).unwrap());

        let (mock, mut handle) = tower_test::mock::pair::<Request, ParsedDocument>();
        // 3 selections in the query exceed a limit of 2.
        let mut service = ServiceBuilder::new()
            .layer(LimitRecursiveSelectionLayer::new(2, false))
            .map_err(downcast_mock_err)
            .service(mock);

        handle.allow(1);
        let driver = tokio::spawn(mock_parser(handle, schema, config));

        let err = service
            .ready()
            .await
            .unwrap()
            .call(Request::new(QUERY.to_string(), None))
            .await
            .expect_err("should exceed the recursive selections limit");
        assert!(matches!(
            err,
            MaybeBackPressureError::PermanentError(SpecError::ValidationError(_))
        ));

        drop(service);
        crate::plugin::test::await_mock_driver(driver).await;
    }

    #[tokio::test]
    async fn warn_only_lets_it_through_when_limit_exceeded() {
        let config = Arc::new(Configuration::default());
        let schema = Arc::new(crate::spec::Schema::parse(SUPERGRAPH_SCHEMA, &config).unwrap());

        let (mock, mut handle) = tower_test::mock::pair::<Request, ParsedDocument>();
        let mut service = ServiceBuilder::new()
            .layer(LimitRecursiveSelectionLayer::new(2, true))
            .map_err(downcast_mock_err)
            .service(mock);

        handle.allow(1);
        let driver = tokio::spawn(mock_parser(handle, schema, config));

        let _res = service
            .ready()
            .await
            .unwrap()
            .call(Request::new(QUERY.to_string(), None))
            .await
            .unwrap();

        drop(service);
        crate::plugin::test::await_mock_driver(driver).await;
    }
}
