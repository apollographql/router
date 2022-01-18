use std::time::Duration;

#[cfg(test)]
use http::Request;
use tower::util::BoxService;
use tower::{BoxError, ServiceBuilder, ServiceExt};

#[cfg(test)]
use crate::{graphql, ApolloRouter};
use crate::{Plugin, RouterResponse, SubgraphRequest};

#[derive(Default)]
struct MyPlugin;
impl Plugin for MyPlugin {
    fn subgraph_service(
        &mut self,
        name: &str,
        service: BoxService<SubgraphRequest, RouterResponse, BoxError>,
    ) -> BoxService<SubgraphRequest, RouterResponse, BoxError> {
        if name == "books" {
            //Rate limit the books subgraph service to 1 request per second
            //If rate limit is exceeded then this service will response immediately rather than timing out.
            return ServiceBuilder::new()
                .load_shed()
                .rate_limit(2, Duration::from_secs(1))
                .service(service)
                .boxed();
        }
        service
    }
}

#[tokio::test]
async fn load_shedding() -> Result<(), BoxError> {
    let router = ApolloRouter::builder()
        .with_plugin(MyPlugin::default())
        .build();

    // first call should succeed
    let res = router
        .call(
            Request::builder()
                .header("A", "HEADER_A")
                .body(graphql::Request {
                    body: "Hello1".to_string(),
                })
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(
        // body() means http response body, .body is the graphql response body
        res.body().body,
        r#"{"req1: Hello1 World from http://books/", "req2: Hello1 World from http://books/"}"#
    );

    println!("{:?}", res);

    // second call will overload the subgraph
    let err = router
        .call(
            Request::builder()
                .header("A", "HEADER_A")
                .body(graphql::Request {
                    body: "Hello1".to_string(),
                })
                .unwrap(),
        )
        .await
        .unwrap_err();

    assert_eq!("service overloaded", format!("{}", err));

    Ok(())
}
