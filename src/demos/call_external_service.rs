use http::{Request, Response};

use tower::util::BoxService;
use tower::{BoxError, ServiceBuilder, ServiceExt};

use crate::{graphql, ApolloRouter, Extension, RouterResponse, SubgraphRequest};

#[derive(Default)]
struct MyExtension {
    client: reqwest::Client,
}
impl Extension for MyExtension {
    fn subgraph_service(
        &mut self,
        name: &str,
        service: BoxService<SubgraphRequest, RouterResponse, BoxError>,
    ) -> BoxService<SubgraphRequest, RouterResponse, BoxError> {
        if name == "books" {
            let client = self.client.clone();
            // Here we provide a completely new implementation for the service
            // This could use a special client to a bespoke service or something else completely
            return ServiceBuilder::new()
                .service_fn(move |req: SubgraphRequest| {
                    let client = client.clone();
                    async move {
                        let response = client.get("http://apollographql.com").send().await;
                        Ok(RouterResponse {
                            request: req.request,
                            response: Response::new(graphql::Response {
                                body: response.unwrap().text().await.unwrap(),
                            }),
                            context: Default::default(),
                        })
                    }
                })
                .boxed();
        }
        service
    }
}

#[tokio::test]
async fn call_external_service() -> Result<(), BoxError> {
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
