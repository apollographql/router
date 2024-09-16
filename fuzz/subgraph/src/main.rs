use async_graphql::EmptySubscription;
use async_graphql_axum::GraphQLRequest;
use async_graphql_axum::GraphQLResponse;
use axum::routing::post;
use axum::Extension;
use axum::Router;
use tower::ServiceBuilder;

use crate::model::Mutation;
use crate::model::Query;

mod model;

type Schema = async_graphql::Schema<Query, Mutation, EmptySubscription>;

async fn graphql_handler(schema: Extension<Schema>, mut req: GraphQLRequest) -> GraphQLResponse {
    //Zero out the random variable
    req.0.variables.remove(&async_graphql::Name::new("random"));
    println!("query: {}", req.0.query);
    schema.execute(req.into_inner()).await.into()
}

#[tokio::main]
async fn main() {
    env_logger::init();
    println!("about to listen to http://localhost:4005");

    let schema = Schema::build(Query, Mutation, EmptySubscription).finish();
    let router = Router::new()
        .route("/", post(graphql_handler))
        .layer(ServiceBuilder::new().layer(Extension(schema)));

    axum::Server::bind(&"0.0.0.0:4005".parse().expect("Fixed address is valid"))
        .serve(router.into_make_service())
        .await
        .expect("Server failed to start")
}
