use std::sync::Arc;

use apollo_compiler::ApolloCompiler;
use apollo_router::plugin::Plugin;
use apollo_router::plugin::PluginInit;
use apollo_router::register_plugin;
use apollo_router::services::supergraph;
use tower::BoxError;
use tower::ServiceBuilder;
use tower::ServiceExt;

#[derive(Default)]
// Global state for our plugin would live here.
// We keep our supergraph sdl here as a string.
struct SupergraphSDL {
    supergraph_sdl: Arc<String>,
}

#[async_trait::async_trait]
impl Plugin for SupergraphSDL {
    // Config is a unit, and `SupergraphSDL` derives default.
    type Config = ();

    async fn new(init: PluginInit<Self::Config>) -> Result<Self, BoxError> {
        Ok(SupergraphSDL {
            supergraph_sdl: init.supergraph_sdl,
        })
    }

    fn supergraph_service(&self, service: supergraph::BoxService) -> supergraph::BoxService {
        // Clone our supergraph_sdl for use in map_request
        let supergraph_sdl = self.supergraph_sdl.clone();
        // `ServiceBuilder` provides us with `map_request` and `map_response` methods.
        //
        // These allow basic interception and transformation of request and response messages.
        ServiceBuilder::new()
            .map_request(move |req: supergraph::Request| {
                // If we have a query
                if let Some(query) = &req.supergraph_request.body().query {
                    // Compile our supergraph_sdl and query
                    let input = format!("{}\n{}", supergraph_sdl, query);
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
register_plugin!("example", "supergraph_sdl", SupergraphSDL);
