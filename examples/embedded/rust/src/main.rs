use apollo_router::services::supergraph;
use apollo_router::TestHarness;
use tower::ServiceExt;

#[tokio::main]
async fn main() -> Result<(), tower::BoxError> {
    // TestHarness creates a GraphQL pipeline to process queries against a supergraph Schema
    let router = TestHarness::builder()
        .schema(include_str!("../../../graphql/supergraph.graphql"))
        .with_subgraph_network_requests()
        .build()
        .await?;

    // ...then create a GraphQL request...
    let request = supergraph::Request::fake_builder()
        .query(r#"query Query { me { name } }"#)
        .build()
        .expect("expecting valid request");

    // ... and run it against the router service!
    let res = router
        .oneshot(request)
        .await?
        .next_response()
        .await
        .unwrap();

    // {
    //   "data": {
    //     "me": {
    //       "name": "Ada Lovelace"
    //     }
    //   }
    // }
    println!("{}", serde_json::to_string_pretty(&res)?);
    Ok(())
}
