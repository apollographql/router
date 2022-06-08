# Changelog for the next release

All notable changes to Router will be documented in this file.

This project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

<!-- <THIS IS AN EXAMPLE, DO NOT REMOVE>

# [x.x.x] (unreleased) - 2022-mm-dd
> Important: X breaking changes below, indicated by **❗ BREAKING ❗**
## ❗ BREAKING ❗
## 🚀 Features ( :rocket: )
## 🐛 Fixes ( :bug: )
## 🛠 Maintenance ( :hammer_and_wrench: )
## 📚 Documentation ( :books: )
## 🐛 Fixes ( :bug: )

## Example section entry format

### **Headline** ([Issue #ISSUE_NUMBER](https://github.com/apollographql/router/issues/ISSUE_NUMBER))

Description! And a link to a [reference](http://url)

By [@USERNAME](https://github.com/USERNAME) in https://github.com/apollographql/router/pull/PULL_NUMBER
-->

# [0.9.4] (unreleased) - 2022-mm-dd

## ❗ BREAKING ❗
### The `apollo-router-core` crate has been merged into `apollo-router` ([PR](https://github.com/apollographql/router/pull/1189))

To upgrade, remove any dependency on the former in `Cargo.toml` files (keeping only the latter), and change imports like so:

```diff
- use apollo_router_core::prelude::*;
+ use apollo_router::prelude::*;
```

By [@SimonSapin](https://github.com/SimonSapin) in https://github.com/apollographql/router/pull/1189

## 🚀 Features
### Add iterators to Context ([PR #1202](https://github.com/apollographql/router/pull/1202))
Context can now be iterated over, with two new methods:
 - iter()
 - iter_mut()

The implementation leans heavily on the underlying entries [DashMap](https://docs.rs/dashmap/5.3.4/dashmap/struct.DashMap.html#method.iter), so the documentation there will be helpful.

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/1202

### Add an experimental optimization to deduplicate variables in query planner [PR #872](https://github.com/apollographql/router/pull/872)
Get rid of duplicated variables in requests and responses of the query planner. This optimization is disabled by default, if you want to enable it you just need override your configuration:

```yaml title="router.yaml"
plugins:
  experimental.traffic_shaping:
    variables_deduplication: true # Enable the variables deduplication optimization
```

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/872

### Add more customizable metrics ([PR #1159](https://github.com/apollographql/router/pull/1159))
Added the ability to add custom attributes/labels on metrics via the configuration file.
Example:
```yaml
telemetry:
  metrics:
    common:
      attributes:
        static:
          - name: "version"
            value: "v1.0.0"
        from_headers:
          - named: "content-type"
            rename: "payload_type"
            default: "application/json"
          - named: "x-custom-header-to-add"
```

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/1159

### Allow to set a custom health check path ([PR #1164](https://github.com/apollographql/router/pull/1164))
Added the possibility to set a custom health check path
```yaml
server:
  # Default is /.well-known/apollo/server-health
  health_check_path: /health
```

By [@jcaromiq](https://github.com/jcaromiq) in https://github.com/apollographql/router/pull/1164

## 🐛 Fixes ( :bug: )

### Fix CORS configuration to eliminate runtime panic on mis-configuration ([PR #1197](https://github.com/apollographql/router/pull/1197))
Previously, it was possible to specify a CORS configuration which was syntactically valid, but which could not be enforced at runtime:
Example:
```yaml
server:
  cors:
    allow_any_origin: true
    allow_credentials: true
```
Such a configuration would result in a runtime panic. The router will now detect this kind of mis-configuration and report the error
without panick-ing.

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/1197

## 🛠 Maintenance ( :hammer_and_wrench: )

### Fix a flappy test to test custom health check path ([PR #1176](https://github.com/apollographql/router/pull/1176))
Force the creation of `SocketAddr` to use a new unused port.

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/1176

### Add static skip/include directive support ([PR #1185](https://github.com/apollographql/router/pull/1185))
+ Rewrite the InlineFragment implementation
+ Small optimization: add support of static check for `@include` and `@skip` directives

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/1185

### Update buildstructor to 0.3 ([PR #1207](https://github.com/apollographql/router/pull/1207))

Update buildstructor to 0.3.
By [@bryncooke](https://github.com/bryncooke) in https://github.com/apollographql/router/pull/1207
