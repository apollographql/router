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
use crate::{Plugin, RouterResponse, SubgraphRequest};

#[derive(Default)]
struct MyPlugin;
impl Plugin for MyPlugin {
    fn subgraph_service(
        &mut self,
        name: &str,
        service: BoxService<SubgraphRequest, RouterResponse, BoxError>,
    ) -> BoxService<SubgraphRequest, RouterResponse, BoxError> {
        if name == "books" {
            //Add timeout for books service
            return ServiceBuilder::new()
                .timeout(Duration::from_secs(1))
                .service(service)
                .boxed();
        }
        service
    }
}

#[tokio::test]
async fn timeouts() -> Result<(), BoxError> {
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
