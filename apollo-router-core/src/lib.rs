use apollo_federation::sources::connect::validation::Code::HttpHeaderNameCollision;
use tower::{Service, ServiceBuilder};

mod extensions;
mod json;
mod layers;
mod services;

pub use extensions::Extensions;

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
