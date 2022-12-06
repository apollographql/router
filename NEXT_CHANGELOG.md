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
## 🥼 Experimental

## Example section entry format

### Headline ([Issue #ISSUE_NUMBER](https://github.com/apollographql/router/issues/ISSUE_NUMBER))

Description! And a link to a [reference](http://url)

By [@USERNAME](https://github.com/USERNAME) in https://github.com/apollographql/router/pull/PULL_NUMBER
-->

# [x.x.x] (unreleased) - 2022-mm-dd

### Filter nullified deferred responses ([Issue #2213](https://github.com/apollographql/router/issues/2168))

[`@defer` spec updates](https://github.com/graphql/graphql-spec/compare/01d7b98f04810c9a9db4c0e53d3c4d54dbf10b82...f58632f496577642221c69809c32dd46b5398bd7#diff-0f02d73330245629f776bb875e5ca2b30978a716732abca136afdd028d5cd33cR448-R470)
mandate that a deferred response should not be sent if its path points to an element of the response that was nullified
in a previous payload.

By [@Geal](https://github.com/geal) in https://github.com/apollographql/router/pull/2184


## 🛠 Maintenance

### it_rate_limit_subgraph_requests fixed ([Issue #2213](https://github.com/apollographql/router/issues/2213))

This test was failing frequently due to it being a timing test being run in a single threaded tokio runtime. 

By [@bryncooke](https://github.com/bryncooke) in https://github.com/apollographql/router/pull/2218
