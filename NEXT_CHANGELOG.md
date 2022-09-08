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

### Add warning logs for queries resulting in errors  ([Issue #1158](https://github.com/apollographql/router/issues/1158))

In particular, when queries:

 - contain wrong field requests
 - result in an error thrown in a subgraph

In both cases the router now logs a warning.

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/1730

## 🐛 Fixes
## 🛠 Maintenance

### Add errors vec in `QueryPlannerResponse` to handle errors in `query_planning_service` ([PR #1504](https://github.com/apollographql/router/pull/1504))

We changed `QueryPlannerResponse` to:

+ Add a `Vec<apollo_router::graphql::Error>`
+ Make the query plan optional, so that it is not present when the query planner encountered a fatal error. Such an error would be in the `Vec`

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/1504

## 📚 Documentation
