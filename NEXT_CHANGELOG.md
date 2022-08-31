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

### Preserve Plugin response Vary headers([PR #1660](https://github.com/apollographql/router/issues/1297))

It is now possible to set a `Vary` header in a client response from a plugin.

Note: This is a breaking change because the prior behaviour provided three default Vary headers and we've had to drop those to enable this change. If, after all plugin processing, there is no Vary header, the router will add one with a value of "origin".

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/1660

### Fix the supported defer specification version to 20220824 ([PR #1652](https://github.com/apollographql/router/issues/1652))

Since the router will ship before the `@defer` specification is done, we
add a parameter to the `Accept` and `Content-Type` headers to indicate
which specification version is accepted.

The specification is fixed to [graphql/graphql-spec@01d7b98](https://github.com/graphql/graphql-spec/commit/01d7b98f04810c9a9db4c0e53d3c4d54dbf10b82)

The router will now return a response with the status code `406 Not Acceptable` if the `Accept` header does not match.

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/1652

## 🚀 Features

### router now provides TraceId ([PR #XXXX](https://github.com/apollographql/router/issues/1536))

If you need a reliable way to link together the various stages of pipeline processing, you can now use

```
apollo_router::tracer::TraceId
```

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/XXXX

## 🐛 Fixes

### Update our helm documentation to illustrate how to use our registry ([#1643](https://github.com/apollographql/router/issues/1643))

The helm chart never used to have a registry, so our docs were really just placeholders. I've updated them to reflect the fact that we now store the chart in our OCI registry.

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/1649

### Update router-bridge to `query-planner` v2.1.0 ([PR #1650](https://github.com/apollographql/router/pull/1650))

The 2.1.0 release of the query planner comes with fixes to fragment interpretation and reduced memory usage.

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/1650

## 🛠 Maintenance

### Remove cache layer ([PR #1647](https://github.com/apollographql/router/pull/1647))

We removed ServiceBuilderExt::cache in 0.16.0. That was the only consumer of
the cache layer. This completes the removal by deleting the cache layer.

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/1647

### Refactor `SupergraphService` ([PR #1615](https://github.com/apollographql/router/issues/1615))

The `SupergraphService` code became too complex, so much that `rustfmt` could not modify it anymore.
This breaks up the code in more manageable functions.

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/1615


## 📚 Documentation
