use std::ops::ControlFlow;

use anyhow::Result;
use apollo_router::graphql;
use apollo_router::layers::ServiceBuilderExt;
use apollo_router::plugin::Plugin;
use apollo_router::plugin::PluginInit;
use apollo_router::register_plugin;
use apollo_router::services::execution;
use apollo_router::services::supergraph;
use tower::BoxError;
use tower::ServiceBuilder;
use tower::ServiceExt;

#[derive(Debug)]
struct ExposeReferencedFieldsByType {
    #[allow(dead_code)]
    configuration: bool,
}

#[async_trait::async_trait]
impl Plugin for ExposeReferencedFieldsByType {
    type Config = bool;

    async fn new(init: PluginInit<Self::Config>) -> Result<Self, BoxError> {
        Ok(Self {
            configuration: init.config,
        })
    }

    fn supergraph_service(&self, service: supergraph::BoxService) -> supergraph::BoxService {
        ServiceBuilder::new().map_request(|mut req:supergraph::Request| {
            let mut body = req.supergraph_request.body_mut();
            let mut query = body.query.as_mut();

            body.query =  body.query.as_ref().map(|query| {
                let query_name = format!("query Query{} (", rand::random::<usize>());
                query.replace("query (", query_name.as_str())
            });
            req
        })
            .map_first_graphql_response(
                |context, http_parts, mut graphql_response: graphql::Response| {
                    let stuff = {
                        let s = context
                            .get("usageReporting")
                            .unwrap()
                            .unwrap_or(serde_json_bytes::Value::Null);
                        s
                    };
                    let _ = graphql_response.extensions.insert("usageReporting", stuff);

                    (http_parts, graphql_response)
                },
            )
            .service(service)
            .boxed()
    }

    fn execution_service(&self, service: execution::BoxService) -> execution::BoxService {
        ServiceBuilder::new()
            .checkpoint(|req: execution::Request| {
                let as_json: serde_json_bytes::Value =
                    serde_json_bytes::to_value(&req.query_plan).unwrap_or_default();

                req.context.insert_json_value("usageReporting", as_json);
                // we don't need to execute the request, there's no subgraphs anyway
                Ok(ControlFlow::Break(
                    execution::Response::fake_builder()
                        .context(req.context)
                        .build()
                        .unwrap(),
                ))
            })
            .service(service)
            .boxed()
    }
}

register_plugin!(
    "apollo-test",
    "expose_referenced_fields_by_type",
    ExposeReferencedFieldsByType
);

fn main() -> Result<()> {
    apollo_router::main()
}
