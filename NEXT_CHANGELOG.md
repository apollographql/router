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

### Exit the router after logging panic details ([PR #1602](https://github.com/apollographql/router/pull/1602))

If the router panics, it can leave the router in an unuseable state.

Terminating after logging the panic details is the best choice here.

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/1602

### Remove `activate()` from the plugin API ([PR #1569](https://github.com/apollographql/router/pull/1569))

Recent changes to configuration reloading means that the only known consumer of this API, telemetry, is no longer using it.

Let's remove it since it's simple to add back if later required.

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/1569

### Rename TestHarness methods ([PR #1579](https://github.com/apollographql/router/pull/1579))

Some methods of `apollo_router::TestHarness` were renamed:

* `extra_supergraph_plugin` → `supergraph_hook`
* `extra_execution_plugin` → `execution_hook`
* `extra_subgraph_plugin` → `subgraph_hook`

By [@SimonSapin](https://github.com/SimonSapin) in https://github.com/apollographql/router/pull/1579

### `Request` and `Response` types from `apollo_router::http_ext` are private ([Issue #1589](https://github.com/apollographql/router/issues/1589))

These types were wrappers around the `Request` and `Response` types from the `http` crate.
Now the latter are used directly instead.

By [@SimonSapin](https://github.com/SimonSapin) in https://github.com/apollographql/router/pull/1589

### Changes to `IntoHeaderName` and `IntoHeaderValue` ([PR #1607](https://github.com/apollographql/router/pull/1607))

Note: these types are typically not use directly, so we expect most user code to require no change.

* Move from `apollo_router::http_ext` to `apollo_router::services`
* Rename to `TryIntoHeaderName` and `TryIntoHeaderValue`
* Make contents opaque
* Replace generic `From<T: Display>` conversion with multiple specific conversions
  that are implemented by `http::headers::Header{Name,Value}`.

By [@SimonSapin](https://github.com/SimonSapin) in https://github.com/apollographql/router/pull/1607

### QueryPlan::usage_reporting and QueryPlannerContent are private ([Issue #1556](https://github.com/apollographql/router/issues/1556))

These items have been removed from the public API of `apollo_router::services::execution`.

By [@SimonSapin](https://github.com/SimonSapin) in https://github.com/apollographql/router/pull/1568

## 🚀 Features

### instrument the rhai plugin with a tracing span ([PR #1598](https://github.com/apollographql/router/pull/1598))

If you have an active rhai script in your router, you will now see a "rhai plugin" tracing span.

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/1598

## 🐛 Fixes

### Only send one report for a response with deferred responses ([PR #1576](https://github.com/apollographql/router/issues/1576))

The router was sending one report per response (even deferred ones), while Studio was expecting one report for the entire
response. The router now sends one report, that measures the latency of the entire operation.

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/1576

### Include formatted query plan when exposing the query plan ([#1557](https://github.com/apollographql/router/issues/1557))

Move the location of the `text` field when exposing the query plan and fill it with a formatted query plan.

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/1557

### Fix typo on HTTP errors from subgraph ([#1593](https://github.com/apollographql/router/pull/1593))

Remove the closed parenthesis at the end of error messages resulting from HTTP errors from subgraphs.

By [@nmoutschen](https://github.com/nmoutschen) in https://github.com/apollographql/router/pull/1593

### Only send one report for a response with deferred responses ([PR #1596](https://github.com/apollographql/router/issues/1596))

deferred responses come as multipart elements, send as individual HTTP response chunks. When a client receives one chunk,
it should contain the next delimiter, so the client knows that the response can be processed, instead of waiting for the
next chunk to see the delimiter.

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/1596

## 🛠 Maintenance


### Re-organize our release steps checklist ([PR #1605](https://github.com/apollographql/router/pull/1605))

We've got a lot of manual steps we need to do in order to release the Router binarys, but we can at least organize them meaningfuly for ourselves to follow!  This is only a Router-team concern today!

By [@abernix](https://github.com/abernix) in https://github.com/apollographql/router/pull/1605)

## 📚 Documentation
