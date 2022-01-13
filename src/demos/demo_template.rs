use crate::{graphql, ApolloRouter, Plugin};
use http::Request;
use tower::BoxError;

#[derive(Default)]
struct MyPlugin;
impl Plugin for MyPlugin {}

#[tokio::test]
async fn demo() -> Result<(), BoxError> {
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
