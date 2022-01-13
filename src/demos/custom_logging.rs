use http::Request;

use tower::util::BoxService;
use tower::{BoxError, ServiceBuilder, ServiceExt};

use crate::{
    graphql, ApolloRouter, PlannedRequest, Plugin, RouterRequest, RouterResponse, SubgraphRequest,
};

#[derive(Default)]
struct MyPlugin;
impl Plugin for MyPlugin {
    fn subgraph_service(
        &mut self,
        name: &str,
        service: BoxService<SubgraphRequest, RouterResponse, BoxError>,
    ) -> BoxService<SubgraphRequest, RouterResponse, BoxError> {
        let name = name.to_string();
        ServiceBuilder::new()
            .map_future(move |f| {
                let name = name.clone();
                async move {
                    //Actually tracing-rs would be used.
                    println!("Before subgraph service {}", name.clone());
                    let r = f.await;
                    println!("After subgraph service {}", name.clone());
                    r
                }
            })
            .service(service)
            .boxed()
    }

    fn router_service(
        &mut self,
        service: BoxService<RouterRequest, RouterResponse, BoxError>,
    ) -> BoxService<RouterRequest, RouterResponse, BoxError> {
        ServiceBuilder::new()
            .map_future(move |f| {
                async move {
                    //Actually tracing-rs would be used.
                    println!("Before router planning service");
                    let r = f.await;
                    println!("After router planning service");
                    r
                }
            })
            .service(service)
            .boxed()
    }

    fn query_planning_service(
        &mut self,
        service: BoxService<RouterRequest, PlannedRequest, BoxError>,
    ) -> BoxService<RouterRequest, PlannedRequest, BoxError> {
        ServiceBuilder::new()
            .map_future(move |f| {
                async move {
                    //Actually tracing-rs would be used.
                    println!("Before query planning service");
                    let r = f.await;
                    println!("After query planning service");
                    r
                }
            })
            .service(service)
            .boxed()
    }

    fn execution_service(
        &mut self,
        service: BoxService<PlannedRequest, RouterResponse, BoxError>,
    ) -> BoxService<PlannedRequest, RouterResponse, BoxError> {
        ServiceBuilder::new()
            .map_future(move |f| {
                async move {
                    //Actually tracing-rs would be used.
                    println!("Before execution service");
                    let r = f.await;
                    println!("After execution service");
                    r
                }
            })
            .service(service)
            .boxed()
    }
}

#[tokio::test]
async fn custom_logging() -> Result<(), BoxError> {
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
    println!("{:?}", response);

    Ok(())
}
