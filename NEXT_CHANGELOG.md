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


### Fix input validation rules ([PR #1211](https://github.com/apollographql/router/pull/1211))
The graphql specification provides two sets of coercion / validation rules, depending on whether we're dealing with inputs or outputs.
The spec we were following for query validation used the output coercion rules; which don't match the spec.
This is a breaking change since slightly invalid input might have validated before, and don't anymore.

By [@o0Ignition0o](https://github.com/o0Ignition0o) in https://github.com/apollographql/router/pull/1211

## 🚀 Features
### Add trace logs for parsing recursion consumption ([PR #1222](https://github.com/apollographql/router/pull/1222))
Apollo Parser now includes recursion limits which can be examined after parse execution. The router logs these
out at trace level. You can see them in your logs by searching for "recursion_limit". For example, if json logging,
and using `jq` to filter the output:
```
router -s ../graphql/supergraph.graphql -c ./router.yaml --log trace | jq -c '. | select(.fields.message == "recursion limit data")'        
{"timestamp":"2022-06-10T15:01:02.213447Z","level":"TRACE","fields":{"message":"recursion limit data","recursion_limit":"recursion limit: 4096, high: 0"},"target":"apollo_router::spec::schema"}
{"timestamp":"2022-06-10T15:01:02.261092Z","level":"TRACE","fields":{"message":"recursion limit data","recursion_limit":"recursion limit: 4096, high: 0"},"target":"apollo_router::spec::schema"}
{"timestamp":"2022-06-10T15:01:07.642977Z","level":"TRACE","fields":{"message":"recursion limit data","recursion_limit":"recursion limit: 4096, high: 4"},"target":"apollo_router::spec::query"}
```
This is indicating that the maximum recursion limit is 4096 and that the query we processed caused us to recurse 4 times.

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/1222

### Helm chart now has the option to use an existing Secret for API Key [PR #1196](https://github.com/apollographql/router/pull/1196)
This change allows the use an already existing Secret for the graph API Key.

To use it, update your values.yaml or specify the value on your helm install command line.

e.g.: helm install --set router.managedFederation.existingSecret="my-secret-name" <etc...>

By [@pellizzetti](https://github.com/pellizzetti) in https://github.com/apollographql/router/pull/1196

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

### Pin clap dependency in Cargo.toml ([PR #1232](https://github.com/apollographql/router/pull/1232))
A minor release of Clap occured yesterday; which introduced a breaking change.

This might lead cargo scaffold users to hit a panic a runtime when the router tries to parse env variables and arguments.

This patch Pins the clap dependency to the version that was available before the release, until the root cause is found and fixed.

By [@o0Ignition0o](https://github.com/o0Ignition0o) in https://github.com/apollographql/router/pull/1232

### Display better error message when on subgraph fetch errors ([PR #1201](https://github.com/apollographql/router/pull/1201))
Show a helpful error message when a subgraph does not return JSON or bad status code

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/1201

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
