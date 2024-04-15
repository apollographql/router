use std::ops::ControlFlow;

use anyhow::Result;
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
struct DoNotExecute {
    #[allow(dead_code)]
    configuration: bool,
}

#[async_trait::async_trait]
impl Plugin for DoNotExecute {
    type Config = bool;

    async fn new(init: PluginInit<Self::Config>) -> Result<Self, BoxError> {
        Ok(Self {
            configuration: init.config,
        })
    }

    fn supergraph_service(&self, service: supergraph::BoxService) -> supergraph::BoxService {
        ServiceBuilder::new()
            .map_request(|mut req: supergraph::Request| {
                let body = req.supergraph_request.body_mut();
                body.query = body.query.as_ref().map(|query| {
                    let query_name = format!("query Query{} ", rand::random::<usize>());
                    query.replacen("query ", query_name.as_str(), 1)
                });
                req
            })
            .service(service)
            .boxed()
    }

    fn execution_service(&self, service: execution::BoxService) -> execution::BoxService {
        ServiceBuilder::new()
            .checkpoint(|req: execution::Request| {
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

register_plugin!("apollo-test", "do_not_execute", DoNotExecute);

// Run this benchmark with cargo run --release --example planner -- --hot-reload -s <path/to/your/schema.graphql> -c ./apollo-router/examples/router.yaml
// You can then send operations to it with `ab` or `hey` or any tool you like:
// hey -n 1000 -c 10 -m POST -H 'Content-Type: application/json' -D 'path/to/an/anonymous/operation' http://localhost:4100
fn main() -> Result<()> {
    apollo_router::main()
}
