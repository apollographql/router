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
## 🚀 Features
## 🐛 Fixes

### Fixed extraVolumeMounts ([Issue #1824](https://github.com/apollographql/router/issues/1824))

Fixed extraVolumeMounts not be being read into the deployment template correctly.

By [@LockedThread](https://github.com/LockedThread) in https://github.com/apollographql/router/pull/1831

### Do not fill in a skeleton object when canceling a subgraph request ([Issue #1819](https://github.com/apollographql/router/issues/1819))

in a query spanning multiple subgraphs like this:

query {
  currentUser {
    activeOrganization {
      id
      creatorUser {
        name
      }
    }
  }
}
if the user subgraph returns {"currentUser": { "activeOrganization": null }}, then the request to the organization subgraph
is cancelled, and no data should be generated, but the query planner was wrongly creating an object at the target path.

This PR also improves the usage of mocked subgraphs with `TestHarness`.

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/1819

### Defer: default defer condition to true ([Issue #1820](https://github.com/apollographql/router/issues/1820))

According to the defer specification, defer conditions are mandatory and default to true.
We fixed a bug where the default value wasn't initialized properly.

By [@o0Ignition0o](https://github.com/o0Ignition0o) in https://github.com/apollographql/router/pull/1832

### Support query plans with empty primary subselections ([Issue #1778](https://github.com/apollographql/router/issues/1778))

When a query with `@defer` would result in an empty primary response, the router was returning
an error in interpreting the query plan. It is now using the query plan properly, and detects
more precisely queries containing `@defer`.

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/1778

### Move response formatting to the execution service ([Issue #1771](https://github.com/apollographql/router/issues/1771))

The response formatting process, where response data is filtered according to deferred responses subselections
and the API schema, was executed in the supergraph service. This is a bit late, because it results in the
execution service returning a stream of invalid responses, so the execution plugins work on invalid data.

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/1771

## 🛠 Maintenance

### Add more compilation gates to delete useless warnings ([PR #1830](https://github.com/apollographql/router/pull/1830))

Add more gates (for `console` feature) to not have warnings when using `--all-features`.

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/1830

## 📚 Documentation
