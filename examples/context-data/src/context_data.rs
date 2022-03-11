use apollo_router_core::{
    register_plugin, Plugin, RouterRequest, RouterResponse, SubgraphRequest, SubgraphResponse,
};
use http::StatusCode;
use tower::{util::BoxService, BoxError, ServiceBuilder, ServiceExt};

#[derive(Default)]
// Global state for our plugin would live here.
// We don't need any in this example
struct ContextData {}

// Passing information via context is useful for storing things like authentication data or other
// collecting cache control information.
// Services are structured in a hierarchy:
// ```
// Router Service +-> Query Planning Service
//                |-> Execution Service +------> Subgraph Service
//                                      |------> Subgraph Service
//                                      |------> Subgraph Service
//                                      |------> ........
// ```
//
// For each request a single instance of context is created and passed to all services.
//
// In this example we:
// 1. Place some information in context at the incoming request of the router service. (world!)
// 2. Pick up and print it out at subgraph request. (Hello world!)
// 3. For each subgraph response merge the some information into the context. (response_count)
// 4. Pick up and print it out at router response. (response_count)
//
impl Plugin for ContextData {
    // Config is a unit, and `ContextData` derives default.
    type Config = ();

    fn new(_configuration: Self::Config) -> Result<Self, BoxError> {
        Ok(Self::default())
    }

    fn router_service(
        &mut self,
        service: BoxService<RouterRequest, RouterResponse, BoxError>,
    ) -> BoxService<RouterRequest, RouterResponse, BoxError> {
        // `ServiceBuilder` provides us with `map_request` and `map_response` methods.
        //
        // These allow basic interception and transformation of request and response messages.
        ServiceBuilder::new()
            .map_request(|req: RouterRequest| {
                // Populate a value in context for use later.
                // Context values must be serializable to serde_json::Value.
                if let Err(e) = req.context.insert("incoming_data", "world!".to_string()) {
                    // This can only happen if the value could not be serialized.
                    // In this case we will never fail because we are storing a string which we
                    // know can be stored as Json.
                    tracing::info!("Failed to set context data {}", e);
                }
                req
            })
            .service(service)
            .map_response(|resp| {
                // Pick up a value from the context on the response.
                if let Ok(Some(data)) = resp.context.get::<_, u64>("response_count") {
                    tracing::info!("Subrequest count {}", data);
                }
                resp
            })
            .boxed()
    }

    fn subgraph_service(
        &mut self,
        _name: &str,
        service: BoxService<SubgraphRequest, SubgraphResponse, BoxError>,
    ) -> BoxService<SubgraphRequest, SubgraphResponse, BoxError> {
        ServiceBuilder::new()
            .map_request(|req: SubgraphRequest| {
                // Pick up a value from the context that was populated earlier.
                if let Ok(Some(data)) = req.context.get::<_, String>("incoming_data") {
                    tracing::info!("Hello {}", data); // Hello world!
                }
                req
            })
            .service(service)
            .map_response(|mut resp: SubgraphResponse| {
                // A single context is created for the entire request.
                // We use upsert because there may be multiple downstream subgraph requests.
                // Upserts are guaranteed to be applied serially.
                match &resp.context.upsert("response_count", |v| v + 1, || 0) {
                    Ok(_) => (),
                    Err(_) => {
                        // This code will never be executed because we know that an integer can be
                        // stored as a serde_json::Value.
                        *resp.response.status_mut() = StatusCode::INTERNAL_SERVER_ERROR;
                    }
                }
                resp
            })
            .boxed()
    }
}

// This macro allows us to use it in our plugin registry!
// register_plugin takes a group name, and a plugin name.
//
// In order to keep the plugin names consistent,
// we use using the `Reverse domain name notation`
register_plugin!("example", "context_data", ContextData);
