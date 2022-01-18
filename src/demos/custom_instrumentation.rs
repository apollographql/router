use http::HeaderValue;
#[cfg(test)]
use http::Request;
use tower::util::BoxService;
use tower::{BoxError, ServiceBuilder, ServiceExt};
use tracing::info_span;
#[cfg(test)]
use tracing::{info, Level};

#[cfg(test)]
use crate::{graphql, ApolloRouter};
use crate::{
    PlannedRequest, Plugin, RouterRequest, RouterResponse, ServiceBuilderExt, SubgraphRequest,
};

#[derive(Default)]
struct MyPlugin;
impl Plugin for MyPlugin {
    fn subgraph_service(
        &mut self,
        _name: &str,
        service: BoxService<SubgraphRequest, RouterResponse, BoxError>,
    ) -> BoxService<SubgraphRequest, RouterResponse, BoxError> {
        ServiceBuilder::new()
            .instrument(|_| info_span!("subgraph_service"))
            .service(service)
            .boxed()
    }

    fn router_service(
        &mut self,
        service: BoxService<RouterRequest, RouterResponse, BoxError>,
    ) -> BoxService<RouterRequest, RouterResponse, BoxError> {
        ServiceBuilder::new()
            .instrument(|r: &RouterRequest| {
                info_span!(
                    "router_service",
                    correlation_id = r
                        .request
                        .headers()
                        .get("A")
                        .unwrap_or(&HeaderValue::from_static(""))
                        .to_str()
                        .unwrap()
                )
            })
            .service(service)
            .boxed()
    }

    fn query_planning_service(
        &mut self,
        service: BoxService<RouterRequest, PlannedRequest, BoxError>,
    ) -> BoxService<RouterRequest, PlannedRequest, BoxError> {
        ServiceBuilder::new()
            .instrument(|_| info_span!("query_planning_service"))
            .service(service)
            .boxed()
    }

    fn execution_service(
        &mut self,
        service: BoxService<PlannedRequest, RouterResponse, BoxError>,
    ) -> BoxService<PlannedRequest, RouterResponse, BoxError> {
        ServiceBuilder::new()
            .instrument(|_| info_span!("execution_service"))
            .service(service)
            .boxed()
    }
}

#[tokio::test]
async fn custom_instrumentation() -> Result<(), BoxError> {
    tracing_subscriber::fmt()
        .with_max_level(Level::INFO)
        .try_init();
    let router = ApolloRouter::builder()
        .with_plugin(MyPlugin::default())
        .build();

    let response = router
        .call(
            Request::builder()
                .header("A", "HEADER_A")
                .body(graphql::Request {
                    body: "Hello1".to_string(),
                })
                .unwrap(),
        )
        .await?;
    info!("{:?}", response);

    Ok(())
}
