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

## Example section entry format

### Headline ([Issue #ISSUE_NUMBER](https://github.com/apollographql/router/issues/ISSUE_NUMBER))

Description! And a link to a [reference](http://url)

By [@USERNAME](https://github.com/USERNAME) in https://github.com/apollographql/router/pull/PULL_NUMBER
-->

# [x.x.x] (unreleased) - 2022-mm-dd

## ❗ BREAKING ❗

### Bind the Sandbox on the same endpoint as the Supergraph [#1785](https://github.com/apollographql/router/issues/1785)

We have rolled back an addition that we released in yesteday’s `v1.0.0-rc.0` which allowed Sandbox to be on a custom listener address.
In retrospect, we believe it was premature to make this change without considering the broader impact of this change which touches on CORS and some developer experiences bits.
We would like more time to make sure we provide you with the best experience before we attempt to make the change again.

Sandbox will continue to be on the same listener address as the GraphQL listener.

If you have updated your configuration for `v1.0.0-rc.0` and enabled the sandbox here is a diff of what has changed:

```diff
sandbox:
-  listen: 127.0.0.1:4000
-  path: /
  enabled: true
# make sure homepage is disabled!
homepage:
  enabled: false
# do not forget to enable introspection,
# otherwise the sandbox won't work!
supergraph:
  introspection: true
```

Note this means you can either enable the Homepage, or the Sandbox, but not both.

By [@o0Ignition0o](https://github.com/o0Ignition0o) in https://github.com/apollographql/router/pull/1796


## 🚀 Features

### Automatically check "Return Query Plans from Router" checkbox in Sandbox ([Issue #1803](https://github.com/apollographql/router/issues/1803))

When loading Sandbox, we now automatically configure it to toggle the "Request query plans from Router" checkbox to the enabled position which requests query plans from the Apollo Router when executing operations.  These query plans are displayed in the Sandbox interface and can be seen by selecting "Query Plan Preview" from the drop-down above the panel on the right side of the interface.

By [@abernix](https://github.com/abernix) in https://github.com/apollographql/router/pull/1804

## 🐛 Fixes

### Fix dev mode when you don't specify a configuration file ([Issue #1801](https://github.com/apollographql/router/issues/1801)) ([Issue #1802](https://github.com/apollographql/router/issues/1802))

Previously the dev mode was ignored if you ran the router without a configuration file.

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/1808

### Respect supergraph path for kubernetes deployment probes ([Issue #1787](https://github.com/apollographql/router/issues/1787))

For cases where you configured the `supergraph.path` for the router when using the helm chart, the liveness 
and readiness probes continued to use the default path of `/` and so the start failed.

By [@damienpontifex](https://github.com/damienpontifex) in https://github.com/apollographql/router/pull/1788


## 🛠 Maintenance

### Update apollo-router-scaffold to use the published router crate [PR #1782](https://github.com/apollographql/router/pull/1782)

Now that apollo-router version "1.0.0-rc.0" is released on [crates.io](https://crates.io/crates/apollo-router), we can update scaffold to it relies on the published crate instead of the git tag.

By [@o0Ignition0o](https://github.com/o0Ignition0o) in https://github.com/apollographql/router/pull/1782

### Refactor Configuration validation [#1791](https://github.com/apollographql/router/issues/1791)

Instantiating `Configuration`s is now fallible, because it will run consistency checks on top of the already run structure checks.

By [@o0Ignition0o](https://github.com/o0Ignition0o) in https://github.com/apollographql/router/pull/1794

## 📚 Documentation

### Add rustdoc documentation to varius modules ([Issue #799](https://github.com/apollographql/router/issues/799))

Adds documentation for:

apollo-router/src/layers/instrument.rs
apollo-router/src/layers/map_first_graphql_response.rs
apollo-router/src/layers/map_future_with_request_data.rs
apollo-router/src/layers/sync_checkpoint.rs
apollo-router/src/plugin/serde.rs
apollo-router/src/tracer.rs

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/1792
