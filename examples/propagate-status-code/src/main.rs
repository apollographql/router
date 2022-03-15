use anyhow::Result;

// adding the module to your main.rs file
// will automatically register it to the router plugin registry.
//
// you can use the plugin by adding it to `config.yml`
mod propagate_status_code;

// `cargo run -- -s ../graphql/supergraph.graphql -c ./config.router.yml`
fn main() -> Result<()> {
    apollo_router::main()
}
