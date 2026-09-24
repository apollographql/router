### Native plugin configuration must be `Clone`, `Send`, `Sync` and `'static` ([PR #10271](https://github.com/apollographql/router/pull/10271))

The `Config` associated type of the `Plugin` and `PluginUnstable` traits now requires `Clone + Send + Sync + 'static`.

To migrate, add `Clone` to your configuration type's derives:

```rust
#[derive(Clone, Debug, Default, Deserialize, JsonSchema)]
struct Conf {
    name: String,
}
```

By [@BrynCooke](https://github.com/BrynCooke) in https://github.com/apollographql/router/pull/10271
