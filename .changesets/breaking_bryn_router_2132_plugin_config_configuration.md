### Native plugin configuration must use apollo-configuration

The `Config` associated type of the `Plugin` and `PluginUnstable` traits now requires `apollo_configuration::Configuration + Clone + Send + Sync + 'static`, in place of `JsonSchema + DeserializeOwned`. Each plugin's configuration is parsed and validated with the router's configuration. A plugin's validation rules run at parse time, so a value they reject fails startup or reload with an error at the plugin's section of the file. Every invalid plugin is reported.

To migrate, add `apollo-configuration` as a dependency (the same version as the router) and declare your configuration with its `#[configuration]` attribute:

```rust
#[apollo_configuration::configuration(validate = validate_conf)]
struct Conf {
    #[config(required)]
    name: String,
}

fn validate_conf(conf: &Conf, mut errors: apollo_configuration::ErrorCollector<'_>) {
    if conf.name.is_empty() {
        errors.nest("name").report_simple("name must not be empty");
    }
}
```

The `validate` argument is optional. Where the attribute can't express a type, such as a tuple struct, implement `Validate` and `Configuration` by hand; the router re-exports the crate as `apollo_router::plugin::apollo_configuration`. A section that is a bare `true` or `false` can use `apollo_router::plugin::Enabled`. `type Config = ()` keeps working.

By [@BrynCooke](https://github.com/BrynCooke)
