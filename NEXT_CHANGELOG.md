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
### The `apollo-router-core` crate has been merged into `apollo-router`

To upgrade, remove any dependency on the former in `Cargo.toml` files (keeping only the latter), and change imports like so:

```diff
- use apollo_router_core::prelude::*;
+ use apollo_router::prelude::*;
```

## 🚀 Features
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

### Allow to set a custom health check path ([PR #1164](https://github.com/apollographql/router/pull/1164))
Added the possibility to set a custom health check path
```yaml
server:
  # Default is /.well-known/apollo/server-health
  health_check_path: /health
```

By [@jcaromiq](https://github.com/jcaromiq) in https://github.com/apollographql/router/pull/1164

## 🐛 Fixes ( :bug: )

### Fix CORS configuration to eliminate runtime panic on mis-configuration ([PR #XXXX](https://github.com/apollographql/router/pull/XXXX))
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

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/XXXX

## 🛠 Maintenance ( :hammer_and_wrench: )

### Fix a flappy test to test custom health check path ([PR #1176](https://github.com/apollographql/router/pull/1176))
Force the creation of `SocketAddr` to use a new unused port.

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/1176
