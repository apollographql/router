use apollo_router_rs::ApolloRouter;
use tower::BoxError;
#[tokio::main]
async fn main() -> Result<(), BoxError> {
    let router = ApolloRouter::builder().build();
    router.start().await;
    Ok(())
}
