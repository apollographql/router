use std::str::FromStr;

use http::header::HeaderName;
use http::{HeaderValue, Request};

use tower::util::BoxService;
use tower::{BoxError, ServiceBuilder, ServiceExt};

use crate::{graphql, ApolloRouter, Extension, RouterResponse, ServiceBuilderExt, SubgraphRequest};

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
                .propagate_header("A") //Propagate using our helper
                .propagate_or_default_header("B", HeaderValue::from(2))
                .map_request(|mut r: SubgraphRequest| {
                    // Demonstrate some manual propagation that could contain fancy logic
                    if let Some(value) = r
                        .request
                        .headers()
                        .get(HeaderName::from_str("SomeHeader").unwrap())
                    {
                        r.subgraph_request.headers_mut().insert("B", value.clone());
                    }
                    r
                })
                .service(service)
                .boxed();
        } else if name == "authors" {
            return ServiceBuilder::new()
                // Some other operations that you can do on headers easily
                .propagate_all_headers()
                .remove_header("C")
                .insert_header("D", HeaderValue::from(5))
                .propagate_cookies() //Propagate using our helper
                .service(service)
                .boxed();
        }

        // If it's any other service just go for the defaults
        service
    }
}

#[tokio::test]
async fn header_propagation() -> Result<(), BoxError> {
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
