use std::sync::Arc;

use apollo_compiler::ApolloCompiler;
use apollo_parser::SyntaxTree;
use apollo_router::plugin::Plugin;
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
    // XXX: Uncomment to fail
    // ctx: Option<ApolloCompiler>,
    // XXX: Must store schema as a string until apollo-rs
    // becomes Send/Sync safe
    schema: Option<String>,
}

#[async_trait::async_trait]
impl Plugin for Schema {
    // Config is a unit, and `Schema` derives default.
    type Config = ();

    async fn new(_configuration: Self::Config) -> Result<Self, BoxError> {
        Ok(Self::default())
    }

    // This function is invoked whenever a new compiler context is available
    // NB: Until apollo-rs is Send/Sync safe we can't store the supplied
    // ctx. For now, we convert the context first to an AST, then to a
    // string and finally we store that string for later use.
    fn schema_update(&mut self, ctx: ApolloCompiler) {
        // Obtain the AST from our compiler context
        let mut ast = ctx.parse();
        // Get our own mutable reference to the AST
        let my_ast = Arc::<SyntaxTree>::make_mut(&mut ast);
        // Need an owned AST, so clone our mutable reference
        let text = my_ast.clone().document().to_string();
        // XXX: Uncomment to fail
        // self.ctx = ctx;
        // Store the re-constructed string representation of our schema
        self.schema = Some(text);
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
                // If we have a schema
                if let Some(schema) = &schema {
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
