use http::{Request, Response, Uri};
use tower::{BoxError, ServiceBuilder};

use crate::services::graphql_subgraph_service::GraphQlSubgraphService;
use crate::services::rest_subgraph_service::RestSubgraphService;
use crate::{graphql, ApolloRouter, RouterResponse, ServiceBuilderExt, SubgraphRequest};

#[tokio::test]
async fn call_external_service() -> Result<(), BoxError> {
    let client = reqwest::Client::default();

    let router = ApolloRouter::builder()
        .with_subgraph_service(
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
        .with_subgraph_service(
            "authors",
            ServiceBuilder::new().propagate_all_headers().service(
                GraphQlSubgraphService::builder()
                    .url(Uri::from_static("http://custom"))
                    .build(),
            ),
        )
        .with_subgraph_service(
            "rest-service",
            ServiceBuilder::new().propagate_header("A").service(
                RestSubgraphService::builder()
                    .url(Uri::from_static("http://custom"))
                    .build(),
            ),
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
