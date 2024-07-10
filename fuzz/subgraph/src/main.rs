use std::time::Duration;

use actix_web::get;
use actix_web::post;
use actix_web::web;
use actix_web::web::Data;
use actix_web::App;
use actix_web::HttpResponse;
use actix_web::HttpServer;
use actix_web::Result;
use async_graphql::http::playground_source;
use async_graphql::http::GraphQLPlaygroundConfig;
use async_graphql::EmptySubscription;
use async_graphql::Schema;
use async_graphql_actix_web::GraphQLRequest;

use crate::model::Mutation;
use crate::model::Query;

mod model;

#[post("/")]
async fn index(
    schema: web::Data<Schema<Query, Mutation, EmptySubscription>>,
    mut req: GraphQLRequest,
) -> HttpResponse {
    //Zero out the random variable
    req.0.variables.remove(&async_graphql::Name::new("random"));
    println!("query: {}", req.0.query);

    let response = schema.execute(req.into_inner()).await;
    let response_json = serde_json::to_string(&response).unwrap();

    HttpResponse::Ok()
        .content_type("application/json")
        .body(response_json)
}

#[get("*")]
async fn index_playground() -> Result<HttpResponse> {
    Ok(HttpResponse::Ok()
        .content_type("text/html; charset=utf-8")
        .body(playground_source(
            GraphQLPlaygroundConfig::new("/").subscription_endpoint("/"),
        )))
}

#[tokio::main]
async fn main() -> std::io::Result<()> {
    env_logger::init();
    println!("about to listen to http://localhost:4005");

    HttpServer::new(move || {
        let schema = Schema::build(Query, Mutation, EmptySubscription).finish();
        App::new()
            .app_data(Data::new(schema))
            //.wrap(EnsureKeepAlive)
            //.wrap(DelayFor::default())
            .service(index)
            .service(index_playground)
    })
    .keep_alive(Duration::from_secs(75))
    .bind("0.0.0.0:4005")?
    .run()
    .await
}
