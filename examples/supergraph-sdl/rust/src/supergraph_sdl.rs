use std::sync::Arc;

use apollo_compiler::ExecutableDocument;
use apollo_compiler::Schema;
use apollo_router::plugin::Plugin;
use apollo_router::plugin::PluginInit;
use apollo_router::register_plugin;
use apollo_router::services::supergraph;
use tower::BoxError;
use tower::ServiceBuilder;
use tower::ServiceExt;

// Global state for our plugin would live here.
// We keep our parsed supergraph schema in a reference-counted pointer
struct SupergraphSDL {
    schema: Arc<Schema>,
}

#[async_trait::async_trait]
impl Plugin for SupergraphSDL {
    // Config is a unit, and `SupergraphSDL` derives default.
    type Config = ();

    async fn new(init: PluginInit<Self::Config>) -> Result<Self, BoxError> {
        Ok(SupergraphSDL {
            schema: Arc::new(Schema::parse(&*init.supergraph_sdl, "schema.graphql")),
        })
    }

    fn supergraph_service(&self, service: supergraph::BoxService) -> supergraph::BoxService {
        // Clone our parsed schema for use in map_request
        let schema = self.schema.clone();
        // `ServiceBuilder` provides us with `map_request` and `map_response` methods.
        //
        // These allow basic interception and transformation of request and response messages.
        ServiceBuilder::new()
            .map_request(move |req: supergraph::Request| {
                // If we have a query
                if let Some(query) = &req.supergraph_request.body().query {
                    // Parse our query against the schema
                    let doc = ExecutableDocument::parse(&schema, query, "query.graphql");
                    // Do we have any diagnostics we'd like to print?
                    if let Err(diagnostics) = doc.validate(&schema) {
                        let diagnostics = diagnostics.to_string_no_color();
                        tracing::warn!(%diagnostics, "validation diagnostics");
                    }
                    // TODO: Whatever else we want to do with our parsed schema and document
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
