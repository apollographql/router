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

> **Note**
> We're almost to 1.0! We've got a couple relatively small breaking changes to the configuration for this release (none to the API) that should be relatively easy to adapt to and a number of bug fixes and usability improvements.

## ❗ BREAKING ❗

### Change `headers` propagation configuration ([PR #1795](https://github.com/apollographql/router/pull/1795))

While it wasn't necessary today, we want to avoid a necessary breaking change in the future by proactively making room for up-and-coming work.  We've therefore introduced another level into the `headers` configuration with a `request` object, to allow for a `response` (see [Issue #1284](https://github.com/apollographql/router/issues/1284)) to be an _additive_ feature after 1.0.

A rough look at this should just be a matter of adding in `request` and indenting everything that was inside it:

```patch
headers:
    all:
+     request:
          - remove:
              named: "test"
```

The good news is that we'll have `response` in the future!  For a full set of examples, please see the [header propagation documentation](https://www.apollographql.com/docs/router/configuration/header-propagation/).

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/1795

### Bind the Sandbox on the same endpoint as the Supergraph, again ([Issue #1785](https://github.com/apollographql/router/issues/1785))

We have rolled back an addition that we released in this week's `v1.0.0-rc.0` which allowed Sandbox (an HTML page that makes requests to the `supergraph` endpoint) to be on a custom socket.  In retrospect, we believe it was premature to make this change without considering the broader impact of this change which ultimately touches on CORS and some developer experiences bits.  Practically speaking, we may not want to introduce this because it complicates the model in a number of ways.

For the foreseeable future, Sandbox will continue to be on the same listener address as the `supergraph` listener.

It's unlikely anyone has really leaned into this much already, but if you've already re-configured `sandbox` or `homepage` to be on a custom `listen`-er and/or `path` in `1.0.0-rc.0`, here is a diff of what you should remove:

```diff
sandbox:
-  listen: 127.0.0.1:4000
-  path: /
  enabled: false
homepage:
-  listen: 127.0.0.1:4000
-  path: /
  enabled: true
```

Note this means you can either enable the `homepage`, or the `sandbox`, but not both.

By [@o0Ignition0o](https://github.com/o0Ignition0o) in https://github.com/apollographql/router/pull/1796

## 🚀 Features

### Automatically check "Return Query Plans from Router" checkbox in Sandbox ([Issue #1803](https://github.com/apollographql/router/issues/1803))

When loading Sandbox, we now automatically configure it to toggle the "Request query plans from Router" checkbox to the enabled position which requests query plans from the Apollo Router when executing operations.  These query plans are displayed in the Sandbox interface and can be seen by selecting "Query Plan Preview" from the drop-down above the panel on the right side of the interface.

By [@abernix](https://github.com/abernix) in https://github.com/apollographql/router/pull/1804

## 🐛 Fixes

### Fix `--dev` mode when no configuration file is specified ([Issue #1801](https://github.com/apollographql/router/issues/1801)) ([Issue #1802](https://github.com/apollographql/router/issues/1802))

We've reconciled an issue where the `--dev` mode flag was being ignored when running the router without a configuration file.  (While many use cases do require a configuration file, the Router actually doesn't _need_ a confguration in many cases!)

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/1808

### Respect `supergraph`'s `path` for Kubernetes deployment probes ([Issue #1787](https://github.com/apollographql/router/issues/1787))

If you've configured the `supergraph`'s `path` property using the Helm chart, the liveness
and readiness probes now utilize these correctly.  This fixes a bug where they continued to use the _default_ path of `/` and resulted in a startup failure.

By [@damienpontifex](https://github.com/damienpontifex) in https://github.com/apollographql/router/pull/1788

### Get variable default values from the query for query plan condition nodes ([PR #1640](https://github.com/apollographql/router/issues/1640))

The query plan condition nodes, generated by the `if` argument of the  `@defer` directive, were
not using the default value of the variable passed in as an argument.

This _also_ fixes _default value_ validations for non-`@defer`'d queries.

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/1640

### Correctly hot-reload when changing the `supergraph`'s `listen` socket ([Issue #1814](https://github.com/apollographql/router/issues/1814))

If you change the `supergraph`'s `listen` socket while in `--hot-reload` mode, the Router will now correctly pickup the change and bind to the new socket.

By [@o0Ignition0o](https://github.com/o0Ignition0o) in https://github.com/apollographql/router/pull/1815

## 🛠 Maintenance

### Update `apollo-router-scaffold` to use the published `apollo-router` crate [PR #1782](https://github.com/apollographql/router/pull/1782)

Now that `apollo-router` is released on [crates.io](https://crates.io/crates/apollo-router), we have updated the project scaffold to rely on the published crate instead of Git tags.

By [@o0Ignition0o](https://github.com/o0Ignition0o) in https://github.com/apollographql/router/pull/1782

### Refactor `Configuration` validation [Issue #1791](https://github.com/apollographql/router/issues/1791)

Instantiating `Configuration`s is now fallible, because it will run consistency checks on top of the already run structure checks.

By [@o0Ignition0o](https://github.com/o0Ignition0o) in https://github.com/apollographql/router/pull/1794

### Refactor response-formatting tests [#1798](https://github.com/apollographql/router/issues/1798)

Rewrite the response-formatting tests to use a builder pattern instead of macros and move the tests to a separate file.

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/1798

## 📚 Documentation

### Add `rustdoc` documentation to various modules ([Issue #799](https://github.com/apollographql/router/issues/799))

Adds documentation for:

- `apollo-router/src/layers/instrument.rs`
- `apollo-router/src/layers/map_first_graphql_response.rs`
- `apollo-router/src/layers/map_future_with_request_data.rs`
- `apollo-router/src/layers/sync_checkpoint.rs`
- `apollo-router/src/plugin/serde.rs`
- `apollo-router/src/tracer.rs`

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/1792

### Fixed `docs.rs` publishing error from our last release

During our last release we discovered for the first time that our documentation wasn't able to compile on the [docs.rs](https://docs.rs) website, leaving our documentation in a [failed state](https://docs.rs/crate/apollo-router/1.0.0-rc.0/builds/629200).

While we've reconciled _that particular problem_, we're now being affected by [this](https://docs.rs/crate/router-bridge/0.1.7/builds/629895) internal compiler errors (ICE) that [is affecting](https://github.com/rust-lang/rust/issues/101844) anyone using `1.65.0-nightly` builds circa today.  Since docs.rs uses `nightly` for all builds, this means it'll be a few more days before we're published there.

With thanks to [@SimonSapin](https://github.com/SimonSapin) in https://github.com/apollographql/federation-rs/pull/185
