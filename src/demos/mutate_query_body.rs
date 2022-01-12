use crate::{
    graphql, ApolloRouter, Extension, PlannedRequest, RouterRequest, RouterResponse,
    SubgraphRequest,
};
use http::Request;
use tower::util::BoxService;
use tower::{BoxError, ServiceBuilder, ServiceExt};

#[derive(Default)]
struct MyExtension;
impl Extension for MyExtension {
    fn subgraph_service(
        &mut self,
        _name: &str,
        service: BoxService<SubgraphRequest, RouterResponse, BoxError>,
    ) -> BoxService<SubgraphRequest, RouterResponse, BoxError> {
        ServiceBuilder::new()
            .map_request(|mut r: SubgraphRequest| {
                //Do something, e.g. censor data. In our case we UPPERCASE everything.
                r.subgraph_request.body_mut().body = r.subgraph_request.body().body.to_uppercase();
                r
            })
            .service(service)
            .boxed()
    }
}

#[tokio::test]
async fn mutate_query_body() -> Result<(), BoxError> {
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
