#![allow(unexpected_cfgs)]

pub mod error;
pub mod extensions;
pub mod json;
pub mod layers;
pub mod services;

#[cfg(test)]
pub mod test_utils;

use crate::layers::ServiceBuilderExt;
use crate::services::http_server;
use crate::services::query_execution;
use crate::services::query_parse;

use tower::{Service, ServiceBuilder};

pub use extensions::Extensions;

/// Builds a complete server-side transformation pipeline from HTTP requests to query execution
///
/// Example usage:
/// ```
/// use apollo_router_core::{server_pipeline, services::{query_execution, query_parse}};
/// use apollo_compiler::{Schema, validation::Valid};
/// use tower::{Service, service_fn};
///
/// let schema = Schema::parse_and_validate("type Query { hello: String }", "test.graphql").unwrap();
/// let parse_service = query_parse::QueryParseService::new(schema);
///
/// let execute_service = service_fn(|req: query_execution::Request| async move {
///     // Your query execution logic here
///     Ok::<_, std::convert::Infallible>(query_execution::Response {
///         extensions: req.extensions,
///         responses: Box::pin(futures::stream::empty())
///     })
/// });
///
/// // Note: This example is simplified - you would need a query planning service implementation
/// // let pipeline = server_pipeline(parse_service, plan_service, execute_service);
/// ```
pub fn server_pipeline<P, Pl, S>(
    query_parse_service: P,
    query_plan_service: Pl,
    execute_service: S,
) -> impl Service<http_server::Request, Response = http_server::Response>
where
    P: Service<query_parse::Request, Response = query_parse::Response> + Clone + Send + 'static,
    P::Future: Send + 'static,
    P::Error: Into<tower::BoxError>,
    Pl: Service<
            crate::services::query_plan::Request,
            Response = crate::services::query_plan::Response,
        > + Clone
        + Send
        + 'static,
    Pl::Future: Send + 'static,
    Pl::Error: Into<tower::BoxError>,
    S: Service<query_execution::Request, Response = query_execution::Response>
        + Clone
        + Send
        + 'static,
    S::Future: Send + 'static,
    S::Error: Into<tower::BoxError>,
{
    // Server-side request transformation:
    // HTTP Request → Bytes Request → JSON Request → Execution Request → ExecuteQuery Service
    ServiceBuilder::new()
        .http_to_bytes()
        .bytes_to_json()
        .prepare_query(query_parse_service, query_plan_service)
        .service(execute_service)
}



#[test]
fn test() {

    // PSUDO pipeline
    // let compute_job_pool = ComputeJobPool;
    // let query_parser_service = ServiceBuilder::new()
    //     .map_request()
    //     .telemetry()
    //     .automatic_persisted_query()
    //     .persisted_queries(manifest)
    //     .in_memory_cache() //Must not cache backpressure error
    //     .redis_cache()
    //     .map_err(BackPressure error mapping)
    //     .load_shed()
    //     .concurrancy_limit()
    //     .thread_pool(compute_job_pool)
    //     .service(QueryParser);
    //
    // let query_planner_service = ServiceBuilder::new()
    //     .telemetry()
    //     .in_memory_cache()
    //     .redis_cache()
    //     .auth_transform() //Get stuff from context
    //     .load_shed()
    //     .metrics()
    //     .concurrency_limit()
    //     .thread_pool(compute_job_pool)
    //     .service(QueryPlanner);
    //
    // let query_preparation_service = ServiceBuilder::new()
    //     .service(QueryPreparation(query_parser_service, query_planner_service))
    //
    //
    // let http_client_service = ServiceBuilder::new()
    //             .telemetry()
    //             .service(HttpClient);
    //         http_client_service
    //     };
    //
    // let graphql_fetch_service = ServiceBuilder::new()
    //     .load_shed()
    //     .rate_limit()
    //     .json_bytes()
    //     .bytes_body()
    //     .service(http_client_service);
    //
    // let rest_fetch_service = ServiceBuilder::new()
    //     .load_shed()
    //     .rate_limit()
    //     .json_bytes()
    //     .bytes_body()
    //     .service(http_client_service);
    //
    // let protobuf_fetch_service = ServiceBuilder::new()
    //     .json_proto()
    //     .proto_bytes()
    //     .service(http_client_service);
    //
    //
    // let fetch_service = ServiceBuilder::new()
    //     .service(FetchService(graphql_fetch_service, rest_fetch_service, protobuf_fetch_service))
    //
    // let execution_service = ServiceBuilder::new()
    //     .telemetry()
    //     .service(ExecutionService(fetch_service));
    //
    //
    //
    // ServiceBuilder::new() // HttpServer shape
    //     .telemetry()
    //     .error_metrics() // Confirm with Ross
    //     .map_future(|| convert overloaded)
    //     .load_shed()
    //     .concurrency_limit()
    //     .decompression()
    //     .http_stuff()
    //     .auth_extraction_and_validation() //Inject auth into context
    //     .body_bytes() // BytesServer shape
    //     .bytes_json() // JsonServer shape
    //     .maybe_log_out_request_response()
    //     .query_preparation(query_preparation_service) //Execution shape
    //     .service(query_execution_service);
}

//TODO error handling
