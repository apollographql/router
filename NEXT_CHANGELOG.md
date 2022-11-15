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

### Add support for returning different HTTP status codes to rhai engine ([Issue #2023](https://github.com/apollographql/router/issues/2023))

This feature now makes it possible to return different HTTP status codes when raising an exception in Rhai. You do this by providing an objectmap with two keys: status and message.

```
    throw #{
        status: 403,
        message: "I have raised a 403"
    };
```

This would short-circuit request/response processing and set an HTTP status code of 403 in the client response and also set the error message.

It is still possible to return errors as per the current method:

```
    throw "I have raised an error";
```
This will have a 500 HTTP status code with the specified message.

It is not currently possible to return a 200 "error". If you try, it will be implicitly converted into a 500 error.

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/2097

### Add support for urlencode/decode to rhai engine ([Issue #2052](https://github.com/apollographql/router/issues/2052))

Two new functions, `urlencode()` and `urldecode()` may now be used to urlencode/decode strings.

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/2053

### **Experimental** 🥼 External cache storage in Redis ([PR #2024](https://github.com/apollographql/router/pull/2024))

implement caching in external storage for query plans, introspection and APQ. This is done as a multi level cache, first in
memory with LRU then with a redis cluster backend. Since it is still experimental, it is opt-in through a Cargo feature.

By [@garypen](https://github.com/garypen) and [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/2024

### Add Query Plan access to ExecutionRequest ([PR #2081](https://github.com/apollographql/router/pull/2081))

You can now access the query plan from an execution request:

```
request.query_plan
```

`request.context` also now supports the rhai `in` keyword.

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/2081

## 🐛 Fixes

### Move the nullifying error messages to extension ([Issue #2071](https://github.com/apollographql/router/issues/2071))

The Router was generating error messages when triggering nullability rules (when a non nullable field is null,
it will nullify the parent object). Adding those messages in the list of errors was potentially redundant
(subgraph can already add an error message indicating why a field is null) and could be treated as a failure by
clients, while nullifying fields is a part of normal operation. We still add the messages in extensions so
clients can easily debug why parts of the response were removed

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/2077

### Fix `Float` input-type coercion for default values with values larger than 32-bits ([Issue #2087](https://github.com/apollographql/router/issues/2087))

A regression has been fixed which caused the Router to reject integers larger than 32-bits used as the default values on `Float` fields in input types.

In other words, the following will once again work as expected:

```graphql
input MyInputType {
    a_float_input: Float = 9876543210
}
```

By [@o0Ignition0o](https://github.com/o0Ignition0o) in https://github.com/apollographql/router/pull/2090

### Assume `Accept: application/json` when no `Accept` header is present [Issue #1990](https://github.com/apollographql/router/issues/1990))

the `Accept` header means `*/*` when it is absent.

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/2078

### Missing `@skip` and `@include` implementation for root operations ([Issue #2072](https://github.com/apollographql/router/issues/2072))

`@skip` and `@include` were not implemented for inline fragments and fragment spreads on top level operations.

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/2096

## 🛠 Maintenance

### Use `debian:bullseye-slim` as our base Docker image ([PR #2085](https://github.com/apollographql/router/pull/2085))

A while ago, when we added compression support to the router, we discovered that the Distroless base-images we were using didn't ship with a copy of `libz.so.1`. We addressed that problem by copying in a version of the library from the Distroless image (Java) which does ship it. While that worked, we found challenges in adding support for both `aarch64` and `amd64` Docker images that would make it less than ideal to continue using those Distroless images.

Rather than persist with this complexity, we've concluded that it would be better to just use a base image which ships with `libz.so.1`, hence the change to `debian:bullseye-slim`.  Those images are still quite minimal and the resulting images are similar in size.

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/2085

### Update `apollo-parser` to `v0.3.2` ([PR #TODO](https://github.com/apollographql/router/pull/TODO))

This updates our dependency on our `apollo-parser` package which brings a few improvements, including more defensive parsing of some operations.  See its CHANGELOG in [the `apollo-rs` repository](https://github.com/apollographql/apollo-rs/blob/main/crates/apollo-parser/CHANGELOG.md#032---2022-11-15) for more details.

By [@abernix](https://github.com/abernix) in https://github.com/apollographql/router/pull/TODO

## 📚 Documentation

### Fix example `helm show values` command ([PR #2088](https://github.com/apollographql/router/pull/2088))

The `helm show vaues` command needs to use the correct Helm chart reference `oci://ghcr.io/apollographql/helm-charts/router`.

By [@col](https://github.com/col) in https://github.com/apollographql/router/pull/2088
