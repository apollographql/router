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
### Update `apollo-parser` to v0.2.11

Fixes error creation for missing selection sets in named operation definitions.

By [@lrlna](https://github.com/lrlna) in https://github.com/apollographql/router/pull/1841

### Fix router scaffold version ([Issue #1836](https://github.com/apollographql/router/issues/1836))

Add `v` prefix to the package version emitted in our [scaffold tooling](https://www.apollographql.com/docs/router/customizations/custom-binary/) when a published version of the crate is available.  This results in packages depending (appropriately, we would claim!) on our published Cargo crates, rather than Git references to the repository.

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/1838

### Fixed `extraVolumeMounts` in Helm charts ([Issue #1824](https://github.com/apollographql/router/issues/1824))

Correct a case in our Helm charts where `extraVolumeMounts` was not be being filled into the deployment template correctly.

By [@LockedThread](https://github.com/LockedThread) in https://github.com/apollographql/router/pull/1831

### Do not fill in a skeleton object when canceling a subgraph request ([Issue #1819](https://github.com/apollographql/router/issues/1819))

Given a supergraph with multiple subgraphs `USER` and `ORGA`, like [this example supergraph](https://github.com/apollographql/router/blob/d0a02525c670e4317586100a31fdbdcd95c6ef07/apollo-router/src/services/supergraph_service.rs#L586-L623), if a query spans multiple subgraphs, like this:

```graphql
query {
  currentUser { # USER subgraph
    activeOrganization { # ORGA subgraph
      id
      creatorUser {
        name
      }
    }
  }
}
```

...when the `USER` subgraph returns `{"currentUser": { "activeOrganization": null }}`, then the request to the `ORGA` subgraph
should be _cancelled_ and no data should be generated.   This was not occurring since the query planner was incorrectly creating an object at the target path.  This is now corrected.

This fix also improves the internal usage of mocked subgraphs with `TestHarness`.

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/1819

### Default conditional `@defer` condition to `true` ([Issue #1820](https://github.com/apollographql/router/issues/1820))

According to recent updates in the `@defer` specification, defer conditions must default to `true`.  This corrects a bug where that default value wasn't being initialized properly.

By [@o0Ignition0o](https://github.com/o0Ignition0o) in https://github.com/apollographql/router/pull/1832

### Support query plans with empty primary subselections ([Issue #1778](https://github.com/apollographql/router/issues/1778))

When a query with `@defer` would result in an empty primary response, the router was returning
an error in interpreting the query plan. It is now using the query plan properly, and detects
more precisely queries containing `@defer`.

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/1778

## 🛠 Maintenance

### Add more compilation gates to hide noisy warnings ([PR #1830](https://github.com/apollographql/router/pull/1830))

Add more gates (for the `console` feature introduced in [PR #1632](https://github.com/apollographql/router/pull/1632)) to not emit compiler warnings when using the `--all-features` flag.  (See original PR for more details on the flag usage.)

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/1830

### Remove potential panics from query plan execution ([PR #1842](https://github.com/apollographql/router/pull/1842))

Some remaining parts of the query plan execution code were using `expect()`, `unwrap()` and `panic()` to guard against assumptions
about data.  These conditions have been replaced with errors which will returned in the response preventing the possibility of panics in these code paths.

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/1842

## 📚 Documentation
