//! Tower fetcher for fetch node execution.

use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::task::Poll;

use apollo_compiler::validation::Valid;
use futures::future::BoxFuture;
use serde_json_bytes::Value;
use tokio::sync::mpsc;
use tower::BoxError;
use tower::ServiceExt;
use tracing::instrument::Instrumented;
use tracing::Instrument;

use super::connector_service::ConnectorServiceFactory;
use super::fetch::BoxService;
use super::fetch::SubscriptionRequest;
use super::new_service::ServiceFactory;
use super::ConnectRequest;
use super::FetchRequest;
use super::SubgraphRequest;
use super::SubscriptionTaskParams;
use crate::error::Error;
use crate::graphql::Request as GraphQLRequest;
use crate::http_ext;
use crate::plugins::subscription::SubscriptionConfig;
use crate::query_planner::build_operation_with_aliasing;
use crate::query_planner::fetch::FetchNode;
use crate::query_planner::subscription::SubscriptionNode;
use crate::query_planner::subscription::OPENED_SUBSCRIPTIONS;
use crate::query_planner::OperationKind;
use crate::query_planner::FETCH_SPAN_NAME;
use crate::query_planner::SUBSCRIBE_SPAN_NAME;
use crate::services::fetch::ErrorMapping;
use crate::services::fetch::Request;
use crate::services::subgraph::BoxGqlStream;
use crate::services::FetchResponse;
use crate::services::SubgraphServiceFactory;
use crate::spec::Schema;

/// The fetch service delegates to either the subgraph service or connector service depending
/// on whether connectors are present in the subgraph.
#[derive(Clone)]
pub(crate) struct FetchService {
    pub(crate) subgraph_service_factory: Arc<SubgraphServiceFactory>,
    pub(crate) schema: Arc<Schema>,
    pub(crate) subgraph_schemas: Arc<HashMap<String, Arc<Valid<apollo_compiler::Schema>>>>,
    pub(crate) _subscription_config: Option<SubscriptionConfig>, // TODO: add subscription support to FetchService
    pub(crate) connector_service_factory: Arc<ConnectorServiceFactory>,
}

impl tower::Service<Request> for FetchService {
    type Response = FetchResponse;
    type Error = BoxError;
    type Future = Instrumented<BoxFuture<'static, Result<Self::Response, Self::Error>>>;

    fn poll_ready(&mut self, _cx: &mut std::task::Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: Request) -> Self::Future {
        match request {
            Request::Fetch(request) => self.handle_fetch(request),
            Request::Subscription(request) => self.handle_subscription(request),
        }
    }
}

impl FetchService {
    fn handle_fetch(
        &mut self,
        request: FetchRequest,
    ) -> <FetchService as tower::Service<Request>>::Future {
        let FetchRequest {
            ref context,
            fetch_node: FetchNode {
                ref service_name, ..
            },
            ..
        } = request;
        let service_name = service_name.clone();
        let fetch_time_offset = context.created_at.elapsed().as_nanos() as i64;

        if let Some(connector) = self
            .connector_service_factory
            .connectors_by_service_name
            .get(service_name.as_ref())
        {
            Self::fetch_with_connector_service(
                self.schema.clone(),
                self.connector_service_factory.clone(),
                connector.id.subgraph_name.clone(),
                request,
            )
            .instrument(tracing::info_span!(
                FETCH_SPAN_NAME,
                "otel.kind" = "INTERNAL",
                "apollo.subgraph.name" = connector.id.subgraph_name,
                "apollo_private.sent_time_offset" = fetch_time_offset
            ))
        } else {
            Self::fetch_with_subgraph_service(
                self.schema.clone(),
                self.subgraph_service_factory.clone(),
                self.subgraph_schemas.clone(),
                request,
            )
            .instrument(tracing::info_span!(
                FETCH_SPAN_NAME,
                "otel.kind" = "INTERNAL",
                "apollo.subgraph.name" = service_name.as_ref(),
                "apollo_private.sent_time_offset" = fetch_time_offset
            ))
        }
    }

    fn fetch_with_connector_service(
        schema: Arc<Schema>,
        connector_service_factory: Arc<ConnectorServiceFactory>,
        subgraph_name: String,
        request: FetchRequest,
    ) -> BoxFuture<'static, Result<FetchResponse, BoxError>> {
        let FetchRequest {
            fetch_node,
            supergraph_request,
            variables,
            context,
            current_dir,
            ..
        } = request;

        let paths = variables.inverted_paths.clone();
        let operation = fetch_node.operation.as_parsed().cloned();

        Box::pin(async move {
            let (_parts, response) = match connector_service_factory
                .create()
                .oneshot(
                    ConnectRequest::builder()
                        .service_name(fetch_node.service_name.clone())
                        .context(context)
                        .operation(operation?.clone())
                        .supergraph_request(supergraph_request)
                        .variables(variables)
                        .build(),
                )
                .await
                .map_to_graphql_error(subgraph_name, &current_dir.to_owned())
            {
                Err(e) => {
                    return Ok((Value::default(), vec![e]));
                }
                Ok(res) => res.response.into_parts(),
            };

            let (value, errors) =
                fetch_node.response_at_path(&schema, &current_dir, paths, response);
            Ok((value, errors))
        })
    }

    fn fetch_with_subgraph_service(
        schema: Arc<Schema>,
        subgraph_service_factory: Arc<SubgraphServiceFactory>,
        subgraph_schemas: Arc<HashMap<String, Arc<Valid<apollo_compiler::Schema>>>>,
        request: FetchRequest,
    ) -> BoxFuture<'static, Result<FetchResponse, BoxError>> {
        let FetchRequest {
            fetch_node,
            supergraph_request,
            variables,
            current_dir,
            context,
        } = request;

        let FetchNode {
            ref service_name,
            ref operation,
            ref operation_kind,
            ref operation_name,
            ..
        } = fetch_node;

        let uri = schema
            .subgraph_url(service_name.as_ref())
            .unwrap_or_else(|| {
                panic!("schema uri for subgraph '{service_name}' should already have been checked")
            })
            .clone();

        let alias_query_string; // this exists outside the if block to allow the as_str() to be longer lived
        let aliased_operation = if let Some(ctx_arg) = &variables.contextual_arguments {
            if let Some(subgraph_schema) = subgraph_schemas.get(&service_name.to_string()) {
                match build_operation_with_aliasing(operation, ctx_arg, subgraph_schema) {
                    Ok(op) => {
                        alias_query_string = op.serialize().no_indent().to_string();
                        alias_query_string.as_str()
                    }
                    Err(errors) => {
                        tracing::debug!(
                            "couldn't generate a valid executable document? {:?}",
                            errors
                        );
                        operation.as_serialized()
                    }
                }
            } else {
                tracing::debug!(
                    "couldn't find a subgraph schema for service {:?}",
                    &service_name
                );
                operation.as_serialized()
            }
        } else {
            operation.as_serialized()
        };

        let aqs = aliased_operation.to_string(); // TODO
        let current_dir = current_dir.clone();
        let service = subgraph_service_factory
            .create(&service_name.clone())
            .expect("we already checked that the service exists during planning; qed");

        let mut subgraph_request = SubgraphRequest::builder()
            .supergraph_request(supergraph_request.clone())
            .subgraph_request(
                http_ext::Request::builder()
                    .method(http::Method::POST)
                    .uri(uri)
                    .body(
                        GraphQLRequest::builder()
                            .query(aliased_operation)
                            .and_operation_name(operation_name.as_ref().map(|n| n.to_string()))
                            .variables(variables.variables.clone())
                            .build(),
                    )
                    .build()
                    .expect("it won't fail because the url is correct and already checked; qed"),
            )
            .subgraph_name(service_name.to_string())
            .operation_kind(*operation_kind)
            .context(context.clone())
            .build();
        subgraph_request.query_hash = fetch_node.schema_aware_hash.clone();
        subgraph_request.authorization = fetch_node.authorization.clone();
        Box::pin(async move {
            Ok(fetch_node
                .subgraph_fetch(
                    service,
                    subgraph_request,
                    &current_dir,
                    &schema,
                    variables.inverted_paths,
                    &aqs,
                    variables.variables,
                )
                .await)
        })
    }

    fn handle_subscription(
        &mut self,
        request: SubscriptionRequest,
    ) -> <FetchService as tower::Service<Request>>::Future {
        let SubscriptionRequest {
            ref context,
            subscription_node: SubscriptionNode {
                ref service_name, ..
            },
            ..
        } = request;

        let service_name = service_name.clone();
        let fetch_time_offset = context.created_at.elapsed().as_nanos() as i64;

        // Subscriptions are not supported for connectors, so they always go to the subgraph service
        Self::subscription_with_subgraph_service(
            self.schema.clone(),
            self.subgraph_service_factory.clone(),
            request,
        )
        .instrument(tracing::info_span!(
            SUBSCRIBE_SPAN_NAME,
            "otel.kind" = "INTERNAL",
            "apollo.subgraph.name" = service_name.as_ref(),
            "apollo_private.sent_time_offset" = fetch_time_offset
        ))
    }

    fn subscription_with_subgraph_service(
        schema: Arc<Schema>,
        subgraph_service_factory: Arc<SubgraphServiceFactory>,
        request: SubscriptionRequest,
    ) -> BoxFuture<'static, Result<crate::services::fetch::Response, BoxError>> {
        let SubscriptionRequest {
            context,
            subscription_node,
            current_dir,
            sender,
            variables,
            supergraph_request,
            subscription_handle,
            subscription_config,
            ..
        } = request;
        let SubscriptionNode {
            ref service_name,
            ref operation,
            ref operation_name,
            ..
        } = subscription_node;

        let service_name = service_name.clone();

        if let Some(max_opened_subscriptions) = subscription_config
            .as_ref()
            .and_then(|s| s.max_opened_subscriptions)
        {
            if OPENED_SUBSCRIPTIONS.load(Ordering::Relaxed) >= max_opened_subscriptions {
                return Box::pin(async {
                    Ok((
                        Value::default(),
                        vec![Error::builder()
                            .message("can't open new subscription, limit reached")
                            .extension_code("SUBSCRIPTION_MAX_LIMIT")
                            .build()],
                    ))
                });
            }
        }
        let mode = match subscription_config.as_ref() {
            Some(config) => config
                .mode
                .get_subgraph_config(&service_name)
                .map(|mode| (config.clone(), mode)),
            None => {
                return Box::pin(async {
                    Ok((
                        Value::default(),
                        vec![Error::builder()
                            .message("subscription support is not enabled")
                            .extension_code("SUBSCRIPTION_DISABLED")
                            .build()],
                    ))
                });
            }
        };

        let service = subgraph_service_factory
            .create(&service_name.clone())
            .expect("we already checked that the service exists during planning; qed");

        let uri = schema
            .subgraph_url(service_name.as_ref())
            .unwrap_or_else(|| {
                panic!("schema uri for subgraph '{service_name}' should already have been checked")
            })
            .clone();

        let (tx_handle, rx_handle) = mpsc::channel::<BoxGqlStream>(1);

        let subscription_handle = subscription_handle
            .as_ref()
            .expect("checked in PlanNode; qed");

        let subgraph_request = SubgraphRequest::builder()
            .supergraph_request(supergraph_request.clone())
            .subgraph_request(
                http_ext::Request::builder()
                    .method(http::Method::POST)
                    .uri(uri)
                    .body(
                        crate::graphql::Request::builder()
                            .query(operation.as_serialized())
                            .and_operation_name(operation_name.as_ref().map(|n| n.to_string()))
                            .variables(variables.variables.clone())
                            .build(),
                    )
                    .build()
                    .expect("it won't fail because the url is correct and already checked; qed"),
            )
            .operation_kind(OperationKind::Subscription)
            .context(context)
            .subgraph_name(service_name.to_string())
            .subscription_stream(tx_handle)
            .and_connection_closed_signal(Some(subscription_handle.closed_signal.resubscribe()))
            .build();

        let mut subscription_handle = subscription_handle.clone();
        Box::pin(async move {
            let response = match mode {
                Some((subscription_config, _mode)) => {
                    let subscription_params = SubscriptionTaskParams {
                        client_sender: sender,
                        subscription_handle: subscription_handle.clone(),
                        subscription_config: subscription_config.clone(),
                        stream_rx: rx_handle.into(),
                        service_name: service_name.to_string(),
                    };

                    let subscription_conf_tx =
                        match subscription_handle.subscription_conf_tx.take() {
                            Some(sc) => sc,
                            None => {
                                return Ok((
                                    Value::default(),
                                    vec![Error::builder()
                                .message("no subscription conf sender provided for a subscription")
                                .extension_code("NO_SUBSCRIPTION_CONF_TX")
                                .build()],
                                ));
                            }
                        };

                    if let Err(err) = subscription_conf_tx.send(subscription_params).await {
                        return Ok((
                            Value::default(),
                            vec![Error::builder()
                                .message(format!("cannot send the subscription data: {err:?}"))
                                .extension_code("SUBSCRIPTION_DATA_SEND_ERROR")
                                .build()],
                        ));
                    }

                    match service
                        .oneshot(subgraph_request)
                        .instrument(tracing::trace_span!("subscription_call"))
                        .await
                        .map_to_graphql_error(service_name.to_string(), &current_dir)
                    {
                        Err(e) => {
                            failfast_error!("subgraph call fetch error: {}", e);
                            vec![e]
                        }
                        Ok(response) => response.response.into_parts().1.errors,
                    }
                }
                None => {
                    vec![Error::builder()
                        .message(format!(
                            "subscription mode is not configured for subgraph {:?}",
                            service_name
                        ))
                        .extension_code("INVALID_SUBSCRIPTION_MODE")
                        .build()]
                }
            };
            Ok((Value::default(), response))
        })
    }
}

#[derive(Clone)]
pub(crate) struct FetchServiceFactory {
    pub(crate) schema: Arc<Schema>,
    pub(crate) subgraph_schemas: Arc<HashMap<String, Arc<Valid<apollo_compiler::Schema>>>>,
    pub(crate) subgraph_service_factory: Arc<SubgraphServiceFactory>,
    pub(crate) subscription_config: Option<SubscriptionConfig>,
    pub(crate) connector_service_factory: Arc<ConnectorServiceFactory>,
}

impl FetchServiceFactory {
    pub(crate) fn new(
        schema: Arc<Schema>,
        subgraph_schemas: Arc<HashMap<String, Arc<Valid<apollo_compiler::Schema>>>>,
        subgraph_service_factory: Arc<SubgraphServiceFactory>,
        subscription_config: Option<SubscriptionConfig>,
        connector_service_factory: Arc<ConnectorServiceFactory>,
    ) -> Self {
        Self {
            subgraph_service_factory,
            subgraph_schemas,
            schema,
            subscription_config,
            connector_service_factory,
        }
    }
}

impl ServiceFactory<Request> for FetchServiceFactory {
    type Service = BoxService;

    fn create(&self) -> Self::Service {
        FetchService {
            subgraph_service_factory: self.subgraph_service_factory.clone(),
            schema: self.schema.clone(),
            subgraph_schemas: self.subgraph_schemas.clone(),
            _subscription_config: self.subscription_config.clone(),
            connector_service_factory: self.connector_service_factory.clone(),
        }
        .boxed()
    }
}
