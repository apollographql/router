use apollo_compiler::ApolloCompiler;
use apollo_router::plugin::Plugin;
use apollo_router::plugin::PluginInitialise;
use apollo_router::register_plugin;
use apollo_router::services::RouterRequest;
use apollo_router::services::RouterResponse;
use tower::util::BoxService;
use tower::BoxError;
use tower::ServiceBuilder;
use tower::ServiceExt;

#[derive(Default)]
// Global state for our plugin would live here.
// We (optionally) keep our schema here as a string.
struct Schema {
    schema: String,
}

#[async_trait::async_trait]
impl Plugin for Schema {
    // Config is a unit, and `Schema` derives default.
    type Config = ();

    async fn new(init: PluginInitialise<Self::Config>) -> Result<Self, BoxError> {
        Ok(Schema {
            schema: init.schema,
        })
    }

    fn router_service(
        &self,
        service: BoxService<RouterRequest, RouterResponse, BoxError>,
    ) -> BoxService<RouterRequest, RouterResponse, BoxError> {
        // Clone our schema for use in map_request
        let schema = self.schema.clone();
        // `ServiceBuilder` provides us with `map_request` and `map_response` methods.
        //
        // These allow basic interception and transformation of request and response messages.
        ServiceBuilder::new()
            .map_request(move |req: RouterRequest| {
                // If we have a query
                if let Some(query) = &req.originating_request.body().query {
                    // Compile our schema and query
                    let input = format!("{}\n{}", schema, query);
                    let ctx = ApolloCompiler::new(&input);
                    // Do we have any diagnostics we'd like to print?
                    let diagnostics = ctx.validate();
                    for diagnostic in diagnostics {
                        tracing::warn!(%diagnostic, "compiler diagnostics");
                    }
                    // TODO: Whatever else we want to do with our compiler context
                }
                req
            })
            .service(service)
            .boxed()
    }
}

// This macro allows us to use it in our plugin registry!
// register_plugin takes a group name, and a plugin name.
//
// In order to keep the plugin names consistent,
// we use using the `Reverse domain name notation`
register_plugin!("example", "schema", Schema);
