#[cfg(test)]
use http::Request;
use tower::util::BoxService;
use tower::{BoxError, ServiceBuilder, ServiceExt};
use tracing::info;
#[cfg(test)]
use tracing::Level;

#[cfg(test)]
use crate::{graphql, ApolloRouter};
use crate::{Plugin, RouterRequest, RouterResponse, SubgraphRequest};

#[derive(Default)]
struct MyPlugin;
impl Plugin for MyPlugin {
    fn subgraph_service(
        &mut self,
        _name: &str,
        service: BoxService<SubgraphRequest, RouterResponse, BoxError>,
    ) -> BoxService<SubgraphRequest, RouterResponse, BoxError> {
        // Pick up the value in context
        ServiceBuilder::new()
            .map_request(|request: SubgraphRequest| {
                let user: Option<&String> = request.context.get("user");
                info!("User: {:?}", user);
                request
            })
            .service(service)
            .boxed()
    }

    fn router_service(
        &mut self,
        service: BoxService<RouterRequest, RouterResponse, BoxError>,
    ) -> BoxService<RouterRequest, RouterResponse, BoxError> {
        // Set the value in context
        ServiceBuilder::new()
            .map_request(|mut request: RouterRequest| {
                request.context.insert("user", "Bob".to_string());
                request
            })
            .service(service)
            .boxed()
    }
}

#[tokio::test]
async fn custom_logging() -> Result<(), BoxError> {
    tracing_subscriber::fmt().with_max_level(Level::INFO).init();
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
