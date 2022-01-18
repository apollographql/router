use std::time::Duration;

#[cfg(test)]
use http::Request;
use tower::util::BoxService;
use tower::{BoxError, ServiceBuilder, ServiceExt};
#[cfg(test)]
use tracing::info;
#[cfg(test)]
use tracing::Level;

#[cfg(test)]
use crate::{graphql, ApolloRouter};
use crate::{Plugin, RouterRequest, RouterResponse, SubgraphRequest};

#[derive(Default)]
struct MyPlugin;
impl Plugin for MyPlugin {
    fn router_service(
        &mut self,
        service: BoxService<RouterRequest, RouterResponse, BoxError>,
    ) -> BoxService<RouterRequest, RouterResponse, BoxError> {
        //Rate limit the overall router service to 1 request per second
        ServiceBuilder::new()
            .rate_limit(1, Duration::from_secs(1))
            .service(service)
            .boxed()
    }

    fn subgraph_service(
        &mut self,
        name: &str,
        service: BoxService<SubgraphRequest, RouterResponse, BoxError>,
    ) -> BoxService<SubgraphRequest, RouterResponse, BoxError> {
        if name == "books" {
            //Rate limit the books subgraph service to 1 request per second
            return ServiceBuilder::new()
                .rate_limit(1, Duration::from_secs(1))
                .service(service)
                .boxed();
        }
        service
    }
}

#[tokio::test]
async fn rate_limiting() -> Result<(), BoxError> {
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
