//! Tower fetcher for fetch node execution.

use std::collections::HashMap;
use std::sync::Arc;
use std::task::Poll;

use apollo_compiler::validation::Valid;
// use apollo_federation::sources::connect::Connectors;
use futures::future::BoxFuture;
use tower::BoxError;

use super::SubgraphRequest;
use crate::graphql::Request as GraphQLRequest;
use crate::http_ext;
use crate::json_ext::Object;
use crate::json_ext::Value;
// use crate::plugins::subscription::SubscriptionConfig;
use crate::query_planner::build_operation_with_aliasing;
use crate::query_planner::fetch::FetchNode;
use crate::query_planner::fetch::Protocol;
use crate::query_planner::fetch::RestFetchNode;
use crate::query_planner::fetch::Variables;
use crate::services::FetchRequest;
use crate::services::FetchResponse;
use crate::services::SubgraphServiceFactory;
use crate::spec::Schema;
use crate::Context;

#[derive(Clone)]
pub(crate) struct FetchService {
    pub(crate) context: Context,
    pub(crate) service_factory: Arc<SubgraphServiceFactory>,
    pub(crate) schema: Arc<Schema>,
    pub(crate) subgraph_schemas: Arc<HashMap<String, Arc<Valid<apollo_compiler::Schema>>>>,
    // pub(crate) subscription_config: Option<SubscriptionConfig>,
    // pub(crate) connectors: Connectors,
}

impl<'a> tower::Service<FetchRequest<'a>> for FetchService {
    type Response = FetchResponse;
    type Error = BoxError;
    type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn poll_ready(&mut self, _cx: &mut std::task::Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: FetchRequest<'a>) -> Self::Future {
        let FetchRequest {
            fetch_node,
            supergraph_request,
            deferred_fetches,
            data,
            current_dir,
        } = request;

        let FetchNode {
            operation,
            operation_kind,
            operation_name,
            service_name,
            requires,
            input_rewrites,
            context_rewrites,
            variable_usages,
            output_rewrites,
            id,
            ..
        } = fetch_node;

        let Variables {
            variables,
            inverted_paths: paths,
            contextual_arguments,
        } = match Variables::new(
            &requires,
            &variable_usages,
            data,
            current_dir,
            // Needs the original request here
            supergraph_request.body(),
            self.schema.as_ref(),
            &input_rewrites,
            &context_rewrites,
        ) {
            Some(variables) => variables,
            None => {
                return Box::pin(async { Ok((Value::Object(Object::default()), Vec::new())) });
            }
        };

        let service_name_string = service_name.to_string();

        // TODO: perf
        let (service_name, subgraph_service_name) = match &*fetch_node.protocol {
            Protocol::RestFetch(RestFetchNode {
                connector_service_name,
                parent_service_name,
                ..
            }) => (parent_service_name.clone(), connector_service_name.clone()),
            _ => (service_name_string.clone(), service_name_string),
        };

        let uri = self
            .schema
            .subgraph_url(service_name.as_ref())
            .unwrap_or_else(|| {
                panic!("schema uri for subgraph '{service_name}' should already have been checked")
            })
            .clone();

        let alias_query_string; // this exists outside the if block to allow the as_str() to be longer lived
        let aliased_operation = if let Some(ctx_arg) = contextual_arguments {
            if let Some(subgraph_schema) = self.subgraph_schemas.get(&service_name.to_string()) {
                match build_operation_with_aliasing(&operation, &ctx_arg, subgraph_schema) {
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
                            .variables(variables.clone())
                            .build(),
                    )
                    .build()
                    .expect("it won't fail because the url is correct and already checked; qed"),
            )
            .subgraph_name(subgraph_service_name)
            .operation_kind(operation_kind)
            .context(self.context.clone())
            .build();
        subgraph_request.query_hash = fetch_node.schema_aware_hash.clone();
        subgraph_request.authorization = fetch_node.authorization.clone();

        let schema = self.schema.clone();
        let aqs = aliased_operation.to_string(); // TODO
        let sns = service_name.clone();
        let sf = self.service_factory.clone();
        let current_dir = current_dir.clone();
        let deferred_fetches = deferred_fetches.clone();
        // TODO: dont' panic Oo
        let service = sf
            .create(&sns)
            .expect("we already checked that the service exists during planning; qed");

        Box::pin(async move {
            let fut = FetchNode::subgraph_fetch(
                service,
                subgraph_request,
                &sns,
                &current_dir,
                &requires,
                &output_rewrites,
                &schema,
                paths,
                id,
                &deferred_fetches,
                &aqs,
                variables,
            );
            Ok(fut.await)
        })
    }
}

// #[derive(Clone)]
// pub(crate) struct FetchServiceFactory {
//     pub(crate) _subgraph_service_factory: Arc<SubgraphServiceFactory>,
// }

// impl FetchServiceFactory {
//     pub(crate) fn new(subgraph_service_factory: Arc<SubgraphServiceFactory>) -> Self {
//         Self {
//             subgraph_service_factory,
//         }
//     }
// }
