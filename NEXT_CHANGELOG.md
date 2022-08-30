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

### Fix the supported defer specification version to 20220824 ([PR #1652](https://github.com/apollographql/router/issues/1652))

Since the router will ship before the @defer specification is done, we
add a parameter to the Accept and Content-Type headers to indicate
which specification version is accepted.

The specification is fixed to [graphql/graphql-spec@01d7b98](https://github.com/graphql/graphql-spec/commit/01d7b98f04810c9a9db4c0e53d3c4d54dbf10b82)

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/1652

## 🚀 Features
## 🐛 Fixes

### Update our helm documentation to illustrate how to use our registry ([PR #1649](https://github.com/apollographql/router/issues/1649))

The helm chart never used to have a registry, so our docs were really just placeholders. I've updated them to reflect the fact that we now store the chart in our OCI registry.

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/1649

## 🛠 Maintenance

### Remove cache layer ([PR #1647](https://github.com/apollographql/router/issues/1647))

We removed ServiceBuilderExt::cache in 0.16.0. That was the only consumer of
the cache layer. This completes the removal by deleting the cache layer.

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/1647

### Refactor `SupergraphService` ([PR #1615](https://github.com/apollographql/router/issues/1615))

The `SupergrapHService` code became too complex, so much that `rsutfmt` could not modify it anymore.
This breaks up the code in more manageable functions.

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/1615


## 📚 Documentation
