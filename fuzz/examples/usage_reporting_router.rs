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
        ServiceBuilder::new()
            .map_first_graphql_response(
                |context, http_parts, mut graphql_response: graphql::Response| {
                    graphql_response.extensions.insert(
                        "usageReporting",
                        context.get("usageReporting").unwrap().unwrap(),
                    );
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
                    serde_json_bytes::to_value(&req.query_plan).unwrap();

                req.context.insert_json_value(
                    "usageReporting",
                    as_json.get("usage_reporting").unwrap().clone(),
                );
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

// make sure you rebuild before you fuzz!
// in the /fuzz directory (you need to be there because fuzz is not in the workspace)
// $ cargo build --example usage_reporting_router
fn main() -> Result<()> {
    apollo_router::main()
}
