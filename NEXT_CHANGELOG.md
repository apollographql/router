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

# [0.14.1] (unreleased) - 2022-mm-dd

## ❗ BREAKING ❗

### CORS: Deprecate newly-added `allow_any_header` option and return to previous behavior ([PR #1480](https://github.com/apollographql/router/pull/1480))

We've re-considered and reverted changes we shipped in the last release with regards to how we handle the [`Access-Control-Request-Headers`](https://developer.mozilla.org/en-US/docs/Web/HTTP/Headers/Access-Control-Request-Headers) *request* header and its corresponding [`Access-Control-Allow-Headers`](https://developer.mozilla.org/en-US/docs/Web/HTTP/Headers/Access-Control-Allow-Headers) response header.  We've reverted to the previous releases' behavior, including the removal of the recently-added `allow_any_header` option.

The previous default behavior was to **reflect** the client's `Access-Control-Request-Headers` request header values back in the `Access-Control-Allow-Headers` response header.  This previous behavior is in fact a common default behavior in other CORS libraries as well, including the [`cors`](https://npm.im/cors) Node.js package and we think it's worth keeping as it was previously, rather than requiring users to specify `allow_any_header` for the _majority_ of use cases.  We believe this to be a safe and secure default that is also more user-friendly.

It is not typically necessary to change this default behavior, but if you wish to allow a more specific set of headers, you can disable the default header reflection and specify a list of headers using the `allow_headers` option, which will allow only those headers in negotiating a response:

```yaml title="router.yaml"
server:
  cors:
    allow_any_origin: true
    # Including this `allow_headers` isn't typically necessary (can be removed) but
    # will *restrict* the permitted Access-Control-Allow-Headers response values.
    allow_headers:
      - Content-Type
      - Authorization
      - x-my-custom-header
```

By [@o0Ignition0o](https://github.com/o0ignition0o) in https://github.com/apollographql/router/pull/1480

### Reference-counting for the schema string given to plugins ([PR #???](https://github.com/apollographql/router/pull/))

The type of the `supergraph_sdl` field of the `apollo_router::plugin::PluginInit` struct
was changed from `String` to `Arc<String>`.
This reduces the number of copies of this string we keep in memory, as schemas can get large.

By [@SimonSapin](https://github.com/SimonSapin)

## 🚀 Features

## 🐛 Fixes

### Update span attributes to be compliant with the opentelemetry for GraphQL specs ([PR #1449](https://github.com/apollographql/router/pull/1449))

Change attribute name `query` to `graphql.document` and `operation_name` to `graphql.operation.name` in spans.

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/1449 

### Configuration handling enhancements ([PR #1454](https://github.com/apollographql/router/pull/1454))

Router config handling now:
* Allows completely empty configuration without error.
* Prevents unknown tags at the root of the configuration from being silently ignored.

By [@bryncooke](https://github.com/bryncooke) in https://github.com/apollographql/router/pull/1454


## 🛠 Maintenance

## 📚 Documentation


### CORS: Fix trailing slashes, and display defaults ([PR #1471](https://github.com/apollographql/router/pull/1471))

The CORS documentation now displays a valid `origins` configuration (without trailing slash!), and the full configuration section displays its default settings.


By [@o0Ignition0o](https://github.com/o0Ignition0o) in https://github.com/apollographql/router/pull/1471



### Add helm OCI example ([PR #1457](https://github.com/apollographql/router/pull/1457))

Update existing filesystem based example to illustrate how to do the same thing using our OCI stored helm chart.

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/1457
