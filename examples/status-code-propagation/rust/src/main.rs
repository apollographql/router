use anyhow::Result;

// adding the module to your main.rs file
// will automatically register it to the router plugin registry.
//
// you can use the plugin by adding it to `router.yaml`
mod propagate_status_code;

// `cargo run -- -s ../../graphql/supergraph.graphql -c ./router.yaml`
fn main() -> Result<()> {
    apollo_router::main()
}
