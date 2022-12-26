# Changelog for the next release

All notable changes to Router will be documented in this file.

This project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

<!-- <THIS IS AN EXAMPLE, DO NOT REMOVE>

# [x.x.x] (unreleased) - 2022-mm-dd
> Important: X breaking changes below, indicated by **❗ BREAKING ❗**
## ❗ BREAKING ❗
## 🚀 Features
## 🐛 Fixes
## 🛠 Maintenance
## 📚 Documentation
## 🥼 Experimental

## Example section entry format

### Headline ([Issue #ISSUE_NUMBER](https://github.com/apollographql/router/issues/ISSUE_NUMBER))

Description! And a link to a [reference](http://url)

By [@USERNAME](https://github.com/USERNAME) in https://github.com/apollographql/router/pull/PULL_NUMBER
-->

# [x.x.x] (unreleased) - 2022-mm-dd

## 🛠 Maintenance

### Add access to global configuration in plugins via `PluginInit` ([PR #2279](https://github.com/apollographql/router/pull/2279))

You can access to the global router configuration in your plugins via the `PluginInit` struct in the `new` method of the `Plugin` trait.
Example:

```rust
async fn new(init: PluginInit<Self::Config>) -> Result<Self, BoxError> {
    dbg!(init.router_config); // Access to the global router configuration
    Ok(Self {})
}
```

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/2279