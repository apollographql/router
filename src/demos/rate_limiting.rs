use http::Request;
use std::time::Duration;

use tower::util::BoxService;
use tower::{BoxError, ServiceBuilder, ServiceExt};

use crate::{graphql, ApolloRouter, Extension, RouterRequest, RouterResponse, SubgraphRequest};

#[derive(Default)]
struct MyExtension;
impl Extension for MyExtension {
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
    let router = ApolloRouter::builder()
        .with_extension(MyExtension::default())
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
