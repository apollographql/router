use http::{Request, Response};

use tower::util::BoxService;
use tower::{BoxError, ServiceBuilder, ServiceExt};

use crate::{graphql, ApolloRouter, Extension, RouterResponse, SubgraphRequest};

#[tokio::test]
async fn call_external_service() -> Result<(), BoxError> {
    let client = reqwest::Client::default();

    let router = ApolloRouter::builder()
        .with_service(
            "books",
            ServiceBuilder::new().service_fn(move |req: SubgraphRequest| {
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
            }),
        )
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
