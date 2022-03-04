use apollo_router_core::{
    register_plugin, Plugin, RouterRequest, RouterResponse, SubgraphRequest, SubgraphResponse,
};
use http::StatusCode;
use tower::{util::BoxService, BoxError, ServiceBuilder, ServiceExt};

#[derive(Default)]
// Global state for our plugin would live here.
// We don't need any in this example
struct ContextData {}

impl Plugin for ContextData {
    // We either forbid anonymous operations,
    // Or we don't. This is the reason why we don't need
    // to deserialize any configuration from a .yml file.
    //
    // Config is a unit, and `ForbidAnonymousOperation` derives default.
    type Config = ();

    fn new(_configuration: Self::Config) -> Result<Self, BoxError> {
        Ok(Self::default())
    }

    // Forbidding anonymous operations can happen at the very beginning of our GraphQL request lifecycle.
    // We will thus put the logic it in the `router_service` section of our plugin.
    fn router_service(
        &mut self,
        service: BoxService<RouterRequest, RouterResponse, BoxError>,
    ) -> BoxService<RouterRequest, RouterResponse, BoxError> {
        // `ServiceBuilder` provides us with a `checkpoint` method.
        //
        // This method allows us to return Step::Continue(request) if we want to let the request through,
        // or Step::Return(response) with a crafted response if we don't want the request to go through.
        ServiceBuilder::new()
            .map_request(|req: RouterRequest| {
                // Populate a value in context for use later.
                if let Err(e) = req.context.insert("incoming_data", "world!".to_string()) {
                    // This can only happen if the value could not be serialized.
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
register_plugin!("com.example", "context_data", ContextData);
