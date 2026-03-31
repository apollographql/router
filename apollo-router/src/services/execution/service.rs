//! Implements the Execution phase of the request lifecycle.

use std::future::ready;
use std::pin::Pin;
use std::sync::Arc;
use std::task::Context;
use std::task::Poll;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

use futures::Stream;
use futures::StreamExt as _;
use futures::future::BoxFuture;
use futures::stream::once;
use serde_json_bytes::Value;
use tokio::sync::broadcast;
use tokio::sync::mpsc;
use tokio::sync::mpsc::Receiver;
use tokio::sync::mpsc::Sender;
use tokio::sync::mpsc::error::SendError;
use tokio::sync::mpsc::error::TryRecvError;
use tokio_stream::wrappers::ReceiverStream;
use tower::BoxError;
use tower::ServiceBuilder;
use tower_service::Service;
use tracing::Instrument;
use tracing::Span;

use crate::Configuration;
use crate::apollo_studio_interop::ReferencedEnums;
use crate::apollo_studio_interop::extract_enums_from_response;
use crate::graphql::Error;
use crate::graphql::IncrementalResponse;
use crate::graphql::Response;
use crate::json_ext::Object;
use crate::json_ext::Path;
use crate::json_ext::PathElement;
use crate::json_ext::ValueExt;
use crate::layers::ServiceExt as _;
use crate::plugins::authentication::APOLLO_AUTHENTICATION_JWT_CLAIMS;
use crate::plugins::subscription::APOLLO_SUBSCRIPTION_PLUGIN;
use crate::plugins::subscription::Subscription;
use crate::plugins::subscription::SubscriptionConfig;
use crate::plugins::telemetry::Telemetry;
use crate::plugins::telemetry::apollo::Config as ApolloTelemetryConfig;
use crate::plugins::telemetry::config::ApolloMetricsReferenceMode;
use crate::query_planner::fetch::SubgraphSchemas;
use crate::query_planner::subscription::SubscriptionHandle;
use crate::services::ExecutionRequest;
use crate::services::ExecutionResponse;
use crate::services::Plugins;
use crate::services::execution;
use crate::services::fetch_service::FetchServiceFactory;
use crate::services::new_service::ServiceFactory;
use crate::spec::Query;
use crate::spec::Schema;
use crate::spec::query::EXTENSIONS_VALUE_COMPLETION_KEY;
use crate::spec::query::subselections::BooleanValues;

/// [`Service`] for query execution.
#[derive(Clone)]
pub(crate) struct ExecutionService {
    pub(crate) schema: Arc<Schema>,
    pub(crate) subgraph_schemas: Arc<SubgraphSchemas>,
    pub(crate) fetch_service_factory: Arc<FetchServiceFactory>,
    pub(crate) configuration: Arc<Configuration>,
    /// Subscription config if enabled
    subscription_config: Option<SubscriptionConfig>,
    apollo_telemetry_config: Option<ApolloTelemetryConfig>,
}

type CloseSignal = broadcast::Sender<()>;
// Used to detect when the stream is dropped and then when the client closed the connection
pub(crate) struct StreamWrapper(pub(crate) ReceiverStream<Response>, Option<CloseSignal>);

impl Stream for StreamWrapper {
    type Item = Response;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        Pin::new(&mut self.0).poll_next(cx)
    }
}

impl Drop for StreamWrapper {
    fn drop(&mut self) {
        if let Some(closed_signal) = self.1.take()
            && let Err(err) = closed_signal.send(())
        {
            tracing::trace!("cannot close the subscription: {err:?}");
        }

        self.0.close();
    }
}

impl Service<ExecutionRequest> for ExecutionService {
    type Response = ExecutionResponse;
    type Error = BoxError;
    type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn poll_ready(
        &mut self,
        _cx: &mut std::task::Context<'_>,
    ) -> Poll<std::result::Result<(), Self::Error>> {
        // We break backpressure here.
        // We can implement backpressure, but we need to think about what we want out of it.
        // For instance, should be block all services if one downstream service is not ready?
        // This may not make sense if you have hundreds of services.
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, req: ExecutionRequest) -> Self::Future {
        let clone = self.clone();
        let mut this = std::mem::replace(self, clone);

        let fut = async move { Ok(this.call_inner(req).await) }.in_current_span();
        Box::pin(fut)
    }
}

impl ExecutionService {
    async fn call_inner(&mut self, req: ExecutionRequest) -> ExecutionResponse {
        let context = req.context;
        let ctx = context.clone();
        let variables = req.supergraph_request.body().variables.clone();

        let (sender, receiver) = mpsc::channel(10);
        let is_deferred = req.query_plan.is_deferred(&variables);
        let is_subscription = req.query_plan.is_subscription();
        let mut claims = None;
        if is_deferred {
            claims = context.get(APOLLO_AUTHENTICATION_JWT_CLAIMS).ok().flatten()
        }
        let (tx_close_signal, subscription_handle) = if is_subscription {
            let (tx_close_signal, rx_close_signal) = broadcast::channel(1);
            (
                Some(tx_close_signal),
                Some(SubscriptionHandle::new(
                    rx_close_signal,
                    req.subscription_tx,
                )),
            )
        } else {
            (None, None)
        };

        let has_initial_data = req.source_stream_value.is_some();
        let mut first = req
            .query_plan
            .execute(
                &context,
                &self.fetch_service_factory,
                &Arc::new(req.supergraph_request),
                &self.schema,
                &self.subgraph_schemas,
                sender,
                subscription_handle.clone(),
                &self.subscription_config,
                req.source_stream_value,
            )
            .await;
        let query = req.query_plan.query.clone();
        let stream = if (is_deferred || is_subscription) && !has_initial_data {
            let stream_mode = if is_deferred {
                StreamMode::Defer
            } else {
                // Keep the connection opened only if there is no error when init the subscription
                first.subscribed = Some(first.errors.is_empty());
                StreamMode::Subscription
            };
            let stream = filter_stream(first, receiver, stream_mode);
            StreamWrapper(stream, tx_close_signal).boxed()
        } else if has_initial_data {
            // If it's a subscription event
            once(ready(first)).boxed()
        } else {
            once(ready(first))
                .chain(ReceiverStream::new(receiver))
                .boxed()
        };

        if has_initial_data {
            return ExecutionResponse::new_from_response(http::Response::new(stream as _), ctx);
        }

        let schema = self.schema.clone();
        let mut nullified_paths: Vec<Path> = vec![];

        let metrics_ref_mode = match &self.apollo_telemetry_config {
            Some(conf) => conf.metrics_reference_mode,
            _ => ApolloMetricsReferenceMode::default(),
        };

        let execution_span = Span::current();
        let insert_result_coercion_errors =
            self.configuration.supergraph.enable_result_coercion_errors;

        let stream = stream
            .map(move |mut response: Response| {
                // Enforce JWT expiry for deferred responses
                if is_deferred {
                    let ts_opt = claims.as_ref().and_then(|x: &Value| {
                        if !x.is_object() {
                            tracing::error!("JWT claims should be an object");
                            return None;
                        }
                        let claims = x.as_object().expect("claims should be an object");
                        let exp = claims.get("exp")?;
                        if !exp.is_number() {
                            tracing::error!("JWT 'exp' (expiry) claim should be a number");
                            return None;
                        }
                        exp.as_i64()
                    });
                    if let Some(ts) = ts_opt {
                        let now = SystemTime::now()
                            .duration_since(UNIX_EPOCH)
                            .expect("we should not run before EPOCH")
                            .as_secs() as i64;
                        if ts < now {
                            tracing::debug!("token has expired, shut down the subscription");
                            response = Response::builder()
                                .has_next(false)
                                .error(
                                    Error::builder()
                                        .message(
                                            "deferred response closed because the JWT has expired",
                                        )
                                        .extension_code("DEFERRED_RESPONSE_JWT_EXPIRED")
                                        .build(),
                                )
                                .build()
                        }
                    }
                }
                response
            })
            .filter_map(move |response: Response| {
                ready(execution_span.in_scope(|| {
                    Self::process_graphql_response(
                        &query,
                        &variables,
                        is_deferred,
                        &schema,
                        &mut nullified_paths,
                        metrics_ref_mode,
                        &context,
                        insert_result_coercion_errors,
                        response,
                    )
                }))
            })
            .boxed();

        ExecutionResponse::new_from_response(http::Response::new(stream as _), ctx)
    }

    #[allow(clippy::too_many_arguments)]
    fn process_graphql_response(
        query: &Arc<Query>,
        variables: &Object,
        is_deferred: bool,
        schema: &Arc<Schema>,
        nullified_paths: &mut Vec<Path>,
        metrics_ref_mode: ApolloMetricsReferenceMode,
        context: &crate::Context,
        insert_result_coercion_errors: bool,
        mut response: Response,
    ) -> Option<Response> {
        // responses that would fall under a path that was previously nullified are not sent
        if response
            .path
            .as_ref()
            .map(|response_path| {
                nullified_paths
                    .iter()
                    .any(|path| response_path.starts_with(path))
            })
            .unwrap_or(false)
        {
            if response.has_next == Some(false) {
                return Some(Response::builder().has_next(false).build());
            } else {
                return None;
            }
        }

        // Empty response (could happen when a subscription stream is closed from the subgraph)
        if response.subscribed == Some(false)
            && response.data.is_none()
            && response.errors.is_empty()
        {
            return response.into();
        }

        let has_next = response.has_next.unwrap_or(true);
        let variables_set = query.defer_variables_set(variables);

        tracing::debug_span!("format_response").in_scope(|| {
            let mut paths = Vec::new();
            if !query.unauthorized.paths.is_empty() {
                query.unauthorized.log_unauthorized_paths();
                query
                    .unauthorized
                    .update_response_with_unauthorized_path_errors(&mut response);
            }

            if let Some(filtered_query) = query.filtered_query.as_ref() {
                paths = filtered_query.format_response(
                    &mut response,
                    variables.clone(),
                    schema.api_schema(),
                    variables_set,
                    insert_result_coercion_errors,
                );
            }

            paths.extend(query.format_response(
                &mut response,
                variables.clone(),
                schema.api_schema(),
                variables_set,
                insert_result_coercion_errors,
            ));

            for error in response.errors.iter_mut() {
                if let Some(path) = &mut error.path {
                    // Check if path can be matched to the supergraph query and truncate if not
                    let matching_len = query.matching_error_path_length(path);
                    if path.len() != matching_len {
                        path.0.drain(matching_len..);

                        if path.is_empty() {
                            error.path = None;
                        }

                        // if path was invalid that means we can't trust locations either
                        error.locations.clear();
                    }
                }
            }

            nullified_paths.extend(paths);

            let mut referenced_enums = context
                .extensions()
                .with_lock(|lock| lock.get::<ReferencedEnums>().cloned())
                .unwrap_or_default();
            if let (ApolloMetricsReferenceMode::Extended, Some(Value::Object(response_body))) =
                (metrics_ref_mode, &response.data)
            {
                extract_enums_from_response(
                    query.clone(),
                    schema.api_schema(),
                    response_body,
                    &mut referenced_enums,
                )
            };

            context
                .extensions()
                .with_lock(|lock| lock.insert::<ReferencedEnums>(referenced_enums));
        });

        match (response.path.as_ref(), response.data.as_ref()) {
            (None, _) | (_, None) => {
                if is_deferred {
                    response.has_next = Some(has_next);
                }

                response.errors.retain(|error| match &error.path {
                    None => true,
                    Some(error_path) => {
                        query.contains_error_path(&response.label, error_path, variables_set)
                    }
                });

                response.label = rewrite_defer_label(&response);
                Some(response)
            }
            // if the deferred response specified a path, we must extract the
            // values matched by that path and create a separate response for
            // each of them.
            // While { "data": { "a": { "b": 1 } } } and { "data": { "b": 1 }, "path: ["a"] }
            // would merge in the same ways, some clients will generate code
            // that checks the specific type of the deferred response at that
            // path, instead of starting from the root object, so to support
            // this, we extract the value at that path.
            // In particular, that means that a deferred fragment in an object
            // under an array would generate one response par array element
            (Some(response_path), Some(response_data)) => {
                let mut sub_responses = Vec::new();
                // TODO: this selection at `response_path` below is applied on the response data _after_
                // is has been post-processed with the user query (in the "format_response" span above).
                // It is not quite right however, because `response_path` (sent by the query planner)
                // may contain `PathElement::Fragment`, whose goal is to filter out only those entities that
                // match the fragment type. However, because the data is been filtered, `response_data` will
                // not contain the `__typename` value for entities (even though those are in the unfiltered
                // data), at least unless the user query selects them manually. The result being that those
                // `PathElement::Fragment` in the path will be essentially ignored (we'll match any object
                // for which we don't have a `__typename` as we would otherwise miss the data that we need
                // to return). I believe this might make it possible to return some data that should not have
                // been returned (at least not in that particular response). And while this is probably only
                // true in fairly contrived examples, this is not working as intended by the query planner,
                // so it is dodgy and could create bigger problems in the future.
                response_data.select_values_and_paths(schema, response_path, |path, value| {
                    // if the deferred path points to an array, split it into multiple subresponses
                    // because the root must be an object
                    if let Value::Array(array) = value {
                        let mut parent = path.clone();
                        for (i, value) in array.iter().enumerate() {
                            parent.push(PathElement::Index(i));
                            sub_responses.push((parent.clone(), value.clone()));
                            parent.pop();
                        }
                    } else {
                        sub_responses.push((path.clone(), value.clone()));
                    }
                });

                Self::split_incremental_response(
                    query,
                    has_next,
                    variables_set,
                    response,
                    sub_responses,
                )
            }
        }
    }

    fn split_incremental_response(
        query: &Arc<Query>,
        has_next: bool,
        variables_set: BooleanValues,
        response: Response,
        sub_responses: Vec<(Path, Value)>,
    ) -> Option<Response> {
        let query = query.clone();

        let rewritten_label = rewrite_defer_label(&response);
        let incremental = sub_responses
            .into_iter()
            .filter_map(move |(path, data)| {
                // filter errors that match the path of this incremental response
                let errors = response
                    .errors
                    .iter()
                    .filter(|error| match &error.path {
                        None => false,
                        Some(error_path) => {
                            query.contains_error_path(&response.label, error_path, variables_set)
                                && error_path_matches_response_path(error_path, &path)
                        }
                    })
                    .cloned()
                    .collect::<Vec<_>>();

                let extensions: Object = response
                    .extensions
                    .iter()
                    .map(|(key, value)| {
                        if key.as_str() == EXTENSIONS_VALUE_COMPLETION_KEY {
                            let value = match value.as_array() {
                                None => Value::Null,
                                Some(v) => Value::Array(
                                    v.iter()
                                        .filter(|ext| {
                                            ext.as_object()
                                                .and_then(|ext| ext.get("path"))
                                                .and_then(|v| {
                                                    serde_json_bytes::from_value::<Path>(v.clone())
                                                        .ok()
                                                })
                                                .map(|ext_path| ext_path.starts_with(&path))
                                                .unwrap_or(false)
                                        })
                                        .cloned()
                                        .collect(),
                                ),
                            };

                            (key.clone(), value)
                        } else {
                            (key.clone(), value.clone())
                        }
                    })
                    .collect();

                // an empty response should not be sent
                // still, if there's an error or extension to show, we should
                // send it
                if !data.is_null() || !errors.is_empty() || !extensions.is_empty() {
                    Some(
                        IncrementalResponse::builder()
                            .and_label(rewritten_label.clone())
                            .data(data)
                            .path(path)
                            .errors(errors)
                            .extensions(extensions)
                            .build(),
                    )
                } else {
                    None
                }
            })
            .collect();

        Some(
            Response::builder()
                .has_next(has_next)
                .incremental(incremental)
                .build(),
        )
    }
}

/// Whether an error at `error_path` should be included in a deferred incremental
/// sub-response at `response_path`. An error matches if it is at or below the
/// sub-response (the original behavior), OR if it is a parent of the sub-response
/// path (e.g., an error at `topProducts` matches a sub-response at
/// `topProducts/0`). The parent-path case handles errors produced by
/// `response_at_path`'s `fallback_dir` truncation, which strips wildcard
/// segments to prevent error multiplication in FlattenNode
fn error_path_matches_response_path(error_path: &Path, response_path: &Path) -> bool {
    error_path.starts_with(response_path) || response_path.starts_with(error_path)
}

fn rewrite_defer_label(response: &Response) -> Option<String> {
    if let Some(label) = &response.label {
        #[allow(clippy::manual_map)] // use an explicit `if` to comment each case
        if let Some(rest) = label.strip_prefix('_') {
            // Drop the prefix added in labeler.rs
            Some(rest.to_owned())
        } else {
            // Remove the synthetic lable generated in labeler.rs
            None
        }
    } else {
        None
    }
}

#[derive(Clone, Copy)]
enum StreamMode {
    Defer,
    Subscription,
}

// modifies the response stream to set `has_next` to `false` and `subscribed` to `false` on the last response
fn filter_stream(
    first: Response,
    mut stream: Receiver<Response>,
    stream_mode: StreamMode,
) -> ReceiverStream<Response> {
    let (mut sender, receiver) = mpsc::channel(10);

    tokio::task::spawn(async move {
        let mut seen_last_message =
            consume_responses(first, &mut stream, &mut sender, stream_mode).await?;

        while let Some(current_response) = stream.recv().await {
            seen_last_message =
                consume_responses(current_response, &mut stream, &mut sender, stream_mode).await?;
        }

        // the response stream disconnected early so we could not add `has_next = false` to the
        // last message, so we add an empty one
        if !seen_last_message {
            let res = match stream_mode {
                StreamMode::Defer => Response::builder().has_next(false).build(),
                StreamMode::Subscription => Response::builder().subscribed(false).build(),
            };
            sender.send(res).await?;
        }

        Ok::<_, SendError<Response>>(())
    });

    receiver.into()
}

// returns Ok(true) when we saw the last message
async fn consume_responses(
    mut current_response: Response,
    stream: &mut Receiver<Response>,
    sender: &mut Sender<Response>,
    stream_mode: StreamMode,
) -> Result<bool, SendError<Response>> {
    loop {
        match stream.try_recv() {
            Err(err) => {
                match err {
                    // no messages available, but the channel is not closed
                    // this means more deferred responses can come
                    TryRecvError::Empty => {
                        sender.send(current_response).await?;
                        return Ok(false);
                    }
                    // the channel is closed
                    // there will be no other deferred responses after that,
                    // so we set `has_next` to `false`
                    TryRecvError::Disconnected => {
                        match stream_mode {
                            StreamMode::Defer => current_response.has_next = Some(false),
                            StreamMode::Subscription => current_response.subscribed = Some(false),
                        }

                        sender.send(current_response).await?;
                        return Ok(true);
                    }
                }
            }
            // there might be other deferred responses after this one,
            // so we should call `try_next` again
            Ok(response) => {
                sender.send(current_response).await?;
                current_response = response;
            }
        }
    }
}

#[derive(Clone)]
pub(crate) struct ExecutionServiceFactory {
    pub(crate) schema: Arc<Schema>,
    pub(crate) subgraph_schemas: Arc<SubgraphSchemas>,
    pub(crate) plugins: Arc<Plugins>,
    pub(crate) fetch_service_factory: Arc<FetchServiceFactory>,
    pub(crate) configuration: Arc<Configuration>,
}

impl ServiceFactory<ExecutionRequest> for ExecutionServiceFactory {
    type Service = execution::BoxCloneSyncService;

    fn create(&self) -> Self::Service {
        let subscription_plugin_conf = self
            .plugins
            .iter()
            .find(|i| i.0.as_str() == APOLLO_SUBSCRIPTION_PLUGIN)
            .and_then(|plugin| (*plugin.1).as_any().downcast_ref::<Subscription>())
            .map(|p| p.config.clone());
        let apollo_telemetry_conf = self
            .plugins
            .iter()
            .find(|i| i.0.as_str() == "apollo.telemetry")
            .and_then(|plugin| (*plugin.1).as_any().downcast_ref::<Telemetry>())
            .map(|t| t.config.apollo.clone());

        ServiceBuilder::new()
            .service(
                self.plugins.iter().rev().fold(
                    crate::services::execution::service::ExecutionService {
                        schema: self.schema.clone(),
                        fetch_service_factory: self.fetch_service_factory.clone(),
                        subscription_config: subscription_plugin_conf,
                        subgraph_schemas: self.subgraph_schemas.clone(),
                        apollo_telemetry_config: apollo_telemetry_conf,
                        configuration: Arc::clone(&self.configuration),
                    }
                    .boxed_clone_sync(),
                    |acc, (_, e)| e.execution_service(acc),
                ),
            )
            .boxed_clone_sync()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use apollo_compiler::Name;
    use apollo_compiler::schema;
    use rstest::rstest;
    use serde_json_bytes::ByteString;

    use super::*;
    use crate::graphql::Error;
    use crate::graphql::Response;
    use crate::json_ext::Path;
    use crate::json_ext::PathElement;
    use crate::spec::FieldType;
    use crate::spec::IncludeSkip;
    use crate::spec::Query;
    use crate::spec::Selection;
    use crate::spec::query::subselections::BooleanValues;

    fn key(name: &str) -> PathElement {
        PathElement::Key(name.to_string(), None)
    }

    fn index(i: usize) -> PathElement {
        PathElement::Index(i)
    }

    fn path(elements: Vec<PathElement>) -> Path {
        Path(elements)
    }

    fn dummy_field_type() -> FieldType {
        FieldType(schema::Type::Named(Name::new_unchecked("String")))
    }

    fn field(name: &str, sub: Option<Vec<Selection>>) -> Selection {
        Selection::Field {
            name: ByteString::from(name),
            alias: None,
            selection_set: sub,
            field_type: dummy_field_type(),
            include_skip: IncludeSkip::default(),
        }
    }

    /// Builds a Query whose selection set is:
    ///   topProducts { name reviews { author { username } } }
    /// This validates error paths through topProducts/N/reviews/N/author/...
    fn make_test_query() -> Arc<Query> {
        let mut query = Query::empty_for_tests();
        query.operation.selection_set = vec![field(
            "topProducts",
            Some(vec![
                field("name", None),
                field(
                    "reviews",
                    Some(vec![field("author", Some(vec![field("username", None)]))]),
                ),
            ]),
        )];
        Arc::new(query)
    }

    fn make_error_at(p: Path, message: &str) -> Error {
        Error::builder().message(message).path(p).build()
    }

    fn make_error_no_path(message: &str) -> Error {
        Error::builder().message(message).build()
    }

    #[rstest]
    #[case::exact_match(
        vec![key("topProducts"), index(0)],
        vec![key("topProducts"), index(0)],
        true
    )]
    #[case::error_deeper(
        vec![key("topProducts"), index(0), key("name")],
        vec![key("topProducts"), index(0)],
        true
    )]
    #[case::error_is_parent(
        vec![key("topProducts")],
        vec![key("topProducts"), index(0)],
        true
    )]
    #[case::unrelated(
        vec![key("otherField")],
        vec![key("topProducts"), index(0)],
        false
    )]
    #[case::diverging_indices(
        vec![key("topProducts"), index(0), key("name")],
        vec![key("topProducts"), index(1)],
        false
    )]
    #[case::empty_error_path(
        vec![],
        vec![key("topProducts"), index(0)],
        true
    )]
    #[case::empty_response_path(
        vec![key("topProducts")],
        vec![],
        true
    )]
    #[case::both_empty(vec![], vec![], true)]
    #[case::parent_matches_index_0(
        vec![key("topProducts")],
        vec![key("topProducts"), index(0)],
        true
    )]
    #[case::parent_matches_index_1(
        vec![key("topProducts")],
        vec![key("topProducts"), index(1)],
        true
    )]
    #[case::parent_matches_index_99(
        vec![key("topProducts")],
        vec![key("topProducts"), index(99)],
        true
    )]
    #[case::nested_parent_matches_descendant(
        vec![key("topProducts"), index(0), key("reviews")],
        vec![key("topProducts"), index(0), key("reviews"), index(0), key("author")],
        true
    )]
    #[case::nested_parent_no_match_different_branch(
        vec![key("topProducts"), index(0), key("reviews")],
        vec![key("topProducts"), index(1), key("reviews"), index(0)],
        false
    )]
    fn error_path_matching(
        #[case] error_elements: Vec<PathElement>,
        #[case] response_elements: Vec<PathElement>,
        #[case] expected: bool,
    ) {
        let ep = path(error_elements);
        let rp = path(response_elements);
        assert_eq!(
            error_path_matches_response_path(&ep, &rp),
            expected,
            "error_path={ep}, response_path={rp}"
        );
    }

    #[rstest]
    #[case::exact_path(
        vec![make_error_at(path(vec![key("topProducts"), index(0)]), "err")],
        vec![(path(vec![key("topProducts"), index(0)]), Value::Object(Object::default()))],
        vec![1],
        vec![vec!["err"]]
    )]
    #[case::deeper_error(
        vec![make_error_at(
            path(vec![key("topProducts"), index(0), key("reviews"), index(0), key("author")]),
            "deep err",
        )],
        vec![(path(vec![key("topProducts"), index(0)]), Value::Object(Object::default()))],
        vec![1],
        vec![vec!["deep err"]]
    )]
    #[case::parent_error(
        vec![make_error_at(path(vec![key("topProducts")]), "parent err")],
        vec![(path(vec![key("topProducts"), index(0)]), Value::Object(Object::default()))],
        vec![1],
        vec![vec!["parent err"]]
    )]
    #[case::parent_fans_out(
        vec![make_error_at(path(vec![key("topProducts")]), "parent err")],
        vec![
            (path(vec![key("topProducts"), index(0)]), Value::Object(Object::default())),
            (path(vec![key("topProducts"), index(1)]), Value::Object(Object::default())),
            (path(vec![key("topProducts"), index(2)]), Value::Object(Object::default())),
        ],
        vec![1, 1, 1],
        vec![vec!["parent err"], vec!["parent err"], vec!["parent err"]]
    )]
    #[case::no_path(
        vec![make_error_no_path("no path")],
        vec![(path(vec![key("topProducts"), index(0)]), Value::Object(Object::default()))],
        vec![0],
        vec![vec![]]
    )]
    #[case::wrong_index(
        vec![make_error_at(path(vec![key("topProducts"), index(1), key("name")]), "wrong index")],
        vec![(path(vec![key("topProducts"), index(0)]), Value::Object(Object::default()))],
        vec![0],
        vec![vec![]]
    )]
    #[case::multi_error_distribution(
        vec![
            make_error_at(path(vec![key("topProducts"), index(0), key("name")]), "err for 0"),
            make_error_at(path(vec![key("topProducts"), index(1), key("name")]), "err for 1"),
        ],
        vec![
            (path(vec![key("topProducts"), index(0)]), Value::Object(Object::default())),
            (path(vec![key("topProducts"), index(1)]), Value::Object(Object::default())),
        ],
        vec![1, 1],
        vec![vec!["err for 0"], vec!["err for 1"]]
    )]
    fn split_incremental_error_distribution(
        #[case] errors: Vec<Error>,
        #[case] sub_responses: Vec<(Path, Value)>,
        #[case] expected_error_counts: Vec<usize>,
        #[case] expected_messages: Vec<Vec<&str>>,
    ) {
        let query = make_test_query();
        let response = Response::builder().errors(errors).build();

        let result = ExecutionService::split_incremental_response(
            &query,
            false,
            BooleanValues { bits: 0 },
            response,
            sub_responses,
        )
        .unwrap();

        assert_eq!(result.incremental.len(), expected_error_counts.len());
        for (i, inc) in result.incremental.iter().enumerate() {
            assert_eq!(
                inc.errors.len(),
                expected_error_counts[i],
                "sub-response {i} error count mismatch"
            );
            let messages: Vec<&str> = inc.errors.iter().map(|e| e.message.as_str()).collect();
            assert_eq!(
                messages, expected_messages[i],
                "sub-response {i} error messages mismatch"
            );
        }
    }

    #[test]
    fn null_data_sub_response_with_no_errors_is_filtered_out() {
        let query = make_test_query();
        let response = Response::builder().build();
        let sub_responses = vec![(path(vec![key("topProducts"), index(0)]), Value::Null)];

        let result = ExecutionService::split_incremental_response(
            &query,
            false,
            BooleanValues { bits: 0 },
            response,
            sub_responses,
        )
        .unwrap();

        assert!(result.incremental.is_empty());
    }

    #[test]
    fn null_data_sub_response_with_errors_is_kept() {
        let query = make_test_query();
        let response = Response::builder()
            .errors(vec![make_error_at(
                path(vec![key("topProducts"), index(0)]),
                "err",
            )])
            .build();
        let sub_responses = vec![(path(vec![key("topProducts"), index(0)]), Value::Null)];

        let result = ExecutionService::split_incremental_response(
            &query,
            false,
            BooleanValues { bits: 0 },
            response,
            sub_responses,
        )
        .unwrap();

        assert_eq!(result.incremental.len(), 1);
        assert_eq!(result.incremental[0].errors.len(), 1);
    }

    #[test]
    fn has_next_is_propagated() {
        let query = make_test_query();
        let response = Response::builder().build();
        let sub_responses = vec![(
            path(vec![key("topProducts"), index(0)]),
            Value::Object(Object::default()),
        )];

        let result = ExecutionService::split_incremental_response(
            &query,
            true,
            BooleanValues { bits: 0 },
            response,
            sub_responses,
        )
        .unwrap();

        assert_eq!(result.has_next, Some(true));
    }
}
