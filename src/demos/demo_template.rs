use crate::{graphql, ApolloRouter, Extension};
use http::Request;
use tower::BoxError;

#[derive(Default)]
struct MyExtension;
impl Extension for MyExtension {}

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
