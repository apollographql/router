use apollo_router;
extern crate custom_plugin;

fn main() -> Result<(), anyhow::Error> {
    custom_plugin::plugin_sanity_check();
    apollo_router::main()
}