//! Implements the Execution phase of the request lifecycle.

use std::future::ready;
use std::pin::Pin;
use std::sync::Arc;
use std::task::Context;
use std::task::Poll;

use futures::channel::mpsc;
use futures::channel::mpsc::Receiver;
use futures::channel::mpsc::SendError;
use futures::channel::mpsc::Sender;
use futures::future::BoxFuture;
use futures::stream::once;
use futures::SinkExt;
use futures::Stream;
use futures::StreamExt;
use serde_json_bytes::Value;
use tokio::sync::broadcast;
use tower::BoxError;
use tower::ServiceBuilder;
use tower::ServiceExt;
use tower_service::Service;
use tracing::event;
use tracing::Instrument;
use tracing::Span;
use tracing_core::Level;

use super::new_service::ServiceFactory;
use super::Plugins;
use super::SubgraphServiceFactory;
use crate::graphql::Error;
use crate::graphql::IncrementalResponse;
use crate::graphql::Response;
use crate::json_ext::Object;
use crate::json_ext::Path;
use crate::json_ext::PathElement;
use crate::json_ext::ValueExt;
use crate::plugins::subscription::Subscription;
use crate::plugins::subscription::SubscriptionConfig;
use crate::plugins::subscription::APOLLO_SUBSCRIPTION_PLUGIN;
use crate::query_planner::subscription::SubscriptionHandle;
use crate::services::execution;
use crate::services::ExecutionRequest;
use crate::services::ExecutionResponse;
use crate::spec::query::subselections::BooleanValues;
use crate::spec::Query;
use crate::spec::Schema;

/// [`Service`] for query execution.
#[derive(Clone)]
pub(crate) struct ExecutionService {
    pub(crate) schema: Arc<Schema>,
    pub(crate) subgraph_service_factory: Arc<SubgraphServiceFactory>,
    /// Subscription config if enabled
    subscription_config: Option<SubscriptionConfig>,
}

type CloseSignal = broadcast::Sender<()>;
// Used to detect when the stream is dropped and then when the client closed the connection
pub(crate) struct StreamWrapper(pub(crate) Receiver<Response>, Option<CloseSignal>);

impl Stream for StreamWrapper {
    type Item = Response;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        Pin::new(&mut self.0).poll_next(cx)
    }
}

impl Drop for StreamWrapper {
    fn drop(&mut self) {
        if let Some(closed_signal) = self.1.take() {
            if let Err(err) = closed_signal.send(()) {
                tracing::trace!("cannot close the subscription: {err:?}");
            }
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

        let fut = async move { this.call_inner(req).await }.in_current_span();
        Box::pin(fut)
    }
}

impl ExecutionService {
    async fn call_inner(&mut self, req: ExecutionRequest) -> Result<ExecutionResponse, BoxError> {
        let context = req.context;
        let ctx = context.clone();
        let variables = req.supergraph_request.body().variables.clone();
        let operation_name = req.supergraph_request.body().operation_name.clone();

        let (sender, receiver) = mpsc::channel(10);
        let is_deferred = req
            .query_plan
            .is_deferred(operation_name.as_deref(), &variables);
        let is_subscription = req.query_plan.is_subscription(operation_name.as_deref());
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
                &self.subgraph_service_factory,
                &Arc::new(req.supergraph_request),
                &self.schema,
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
            once(ready(first)).chain(receiver).boxed()
        };

        if has_initial_data {
            return Ok(ExecutionResponse::new_from_response(
                http::Response::new(stream as _),
                ctx,
            ));
        }

        let schema = self.schema.clone();
        let mut nullified_paths: Vec<Path> = vec![];

        let execution_span = Span::current();

        let stream = stream
            .filter_map(move |response: Response| {
                ready(execution_span.in_scope(|| {
                    Self::process_graphql_response(
                        &query,
                        operation_name.as_deref(),
                        &variables,
                        is_deferred,
                        &schema,
                        &mut nullified_paths,
                        response,
                    )
                }))
            })
            .boxed();

        Ok(ExecutionResponse::new_from_response(
            http::Response::new(stream as _),
            ctx,
        ))
    }

    fn process_graphql_response(
        query: &Arc<Query>,
        operation_name: Option<&str>,
        variables: &Object,
        is_deferred: bool,
        schema: &Arc<Schema>,
        nullified_paths: &mut Vec<Path>,
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
        let variables_set = query.defer_variables_set(operation_name, variables);

        tracing::debug_span!("format_response").in_scope(|| {
            let mut paths = Vec::new();
            if let Some(filtered_query) = query.filtered_query.as_ref() {
                let unauthorized_paths = query.unauthorized_paths.iter().map(|path| path.to_string()).collect::<Vec<_>>();
                if !unauthorized_paths.is_empty() {
                    event!(Level::ERROR, unauthorized_query_paths = ?unauthorized_paths, "Authorization error",);
                }

                paths = filtered_query.format_response(
                    &mut response,
                    operation_name,
                    variables.clone(),
                    schema.api_schema(),
                    variables_set,
                );

                for path in &query.unauthorized_paths {
                    response.errors.push(Error::builder()
                    .message("Unauthorized field or type")
                    .path(path.clone())
                    .extension_code("UNAUTHORIZED_FIELD_OR_TYPE").build());
                }
            }

            paths.extend(
                query
                    .format_response(
                        &mut response,
                        operation_name,
                        variables.clone(),
                        schema.api_schema(),
                        variables_set,
                    )
                    ,
            );
            nullified_paths.extend(paths);
        });

        match (response.path.as_ref(), response.data.as_ref()) {
            (None, _) | (_, None) => {
                if is_deferred {
                    response.has_next = Some(has_next);
                }

                response.errors.retain(|error| match &error.path {
                    None => true,
                    Some(error_path) => query.contains_error_path(
                        operation_name,
                        &response.label,
                        error_path,
                        variables_set,
                    ),
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
                    operation_name,
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
        operation_name: Option<&str>,
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
                            query.contains_error_path(
                                operation_name,
                                &response.label,
                                error_path,
                                variables_set,
                            ) && error_path.starts_with(&path)
                        }
                    })
                    .cloned()
                    .collect::<Vec<_>>();

                let extensions: Object = response
                    .extensions
                    .iter()
                    .map(|(key, value)| {
                        if key.as_str() == "valueCompletion" {
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
) -> Receiver<Response> {
    let (mut sender, receiver) = mpsc::channel(10);

    tokio::task::spawn(async move {
        let mut seen_last_message =
            consume_responses(first, &mut stream, &mut sender, stream_mode).await?;

        while let Some(current_response) = stream.next().await {
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

        Ok::<_, SendError>(())
    });

    receiver
}

// returns Ok(true) when we saw the last message
async fn consume_responses(
    mut current_response: Response,
    stream: &mut Receiver<Response>,
    sender: &mut Sender<Response>,
    stream_mode: StreamMode,
) -> Result<bool, SendError> {
    loop {
        match stream.try_next() {
            // no messages available, but the channel is not closed
            // this means more deferred responses can come
            Err(_) => {
                sender.send(current_response).await?;
                return Ok(false);
            }

            // there might be other deferred responses after this one,
            // so we should call `try_next` again
            Ok(Some(response)) => {
                sender.send(current_response).await?;
                current_response = response;
            }
            // the channel is closed
            // there will be no other deferred responses after that,
            // so we set `has_next` to `false`
            Ok(None) => {
                match stream_mode {
                    StreamMode::Defer => current_response.has_next = Some(false),
                    StreamMode::Subscription => current_response.subscribed = Some(false),
                }

                sender.send(current_response).await?;
                return Ok(true);
            }
        }
    }
}

#[derive(Clone)]
pub(crate) struct ExecutionServiceFactory {
    pub(crate) schema: Arc<Schema>,
    pub(crate) plugins: Arc<Plugins>,
    pub(crate) subgraph_service_factory: Arc<SubgraphServiceFactory>,
}

impl ServiceFactory<ExecutionRequest> for ExecutionServiceFactory {
    type Service = execution::BoxService;

    fn create(&self) -> Self::Service {
        let subscription_plugin_conf = self
            .plugins
            .iter()
            .find(|i| i.0.as_str() == APOLLO_SUBSCRIPTION_PLUGIN)
            .and_then(|plugin| (*plugin.1).as_any().downcast_ref::<Subscription>())
            .map(|p| p.config.clone());

        ServiceBuilder::new()
            .service(
                self.plugins.iter().rev().fold(
                    crate::services::execution_service::ExecutionService {
                        schema: self.schema.clone(),
                        subgraph_service_factory: self.subgraph_service_factory.clone(),
                        subscription_config: subscription_plugin_conf,
                    }
                    .boxed(),
                    |acc, (_, e)| e.execution_service(acc),
                ),
            )
            .boxed()
    }
}
