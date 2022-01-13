use crate::{graphql, ApolloRouter, Extension, RouterResponse, SubgraphRequest};
use http::{Request, Uri};
use tower::util::BoxService;
use tower::ServiceExt;
use tower::{BoxError, ServiceBuilder};
#[derive(Default)]
struct MyExtension;
impl Extension for MyExtension {
    fn subgraph_service(
        &mut self,
        name: &str,
        service: BoxService<SubgraphRequest, RouterResponse, BoxError>,
    ) -> BoxService<SubgraphRequest, RouterResponse, BoxError> {
        if name == "books" {
            return ServiceBuilder::new()
                .map_request(|mut r: SubgraphRequest| {
                    r.url_override = Some(Uri::from_static("http://overridden"));
                    r
                })
                .service(service)
                .boxed();
        }
        service
    }
}

#[tokio::test]
async fn demo() -> Result<(), BoxError> {
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
