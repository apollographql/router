# Changelog for the next release

All notable changes to Router will be documented in this file.

This project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

<!-- <THIS IS AN EXAMPLE, DO NOT REMOVE>

# [x.x.x] (unreleased) - 2022-mm-dd
> Important: X breaking changes below, indicated by **❗ BREAKING ❗**
## ❗ BREAKING ❗
## 🚀 Features
## 🐛 Fixes
## 📃 Configuration
## 🛠 Maintenance
## 📚 Documentation
## 🥼 Experimental

## Example section entry format

### Headline ([Issue #ISSUE_NUMBER](https://github.com/apollographql/router/issues/ISSUE_NUMBER))

Description! And a link to a [reference](http://url)

By [@USERNAME](https://github.com/USERNAME) in https://github.com/apollographql/router/pull/PULL_NUMBER
-->

# [1.8.1] (unreleased) - 2022-mm-dd

## 🚀 Features

### Add support for `base64encode()` / `base64decode()` in Rhai ([Issue #2025](https://github.com/apollographql/router/issues/2025))

Two new functions, `base64encode()` and `base64decode()` may now be used to Base64-encode or Base64-decode strings, respectively.

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/2394

### Anonymous product usage analytics ([Issue #2124](https://github.com/apollographql/router/issues/2124), [Issue #2397](https://github.com/apollographql/router/issues/2397))

Following up on https://github.com/apollographql/router/pull/1630, the Router transmits anonymous usage telemetry about configurable feature usage which helps guide Router product development.  No information is transmitted in our usage collection that includes any request-specific information.  Knowing what features and configuration our users are depending on allows us to evaluate opportunity to reduce complexity and remain diligent about the surface area of the Router.  The privacy of your and your user's data is of critical importantance to the core Router team and we handle it in accordance with our [privacy policy](https://www.apollographql.com/docs/router/privacy/), which clearly states which data we collect and transmit and offers information on how to opt-out.
Note that strings are output as `<redacted>` so that we do not leak confidential or sensitive information.
Boolean and numerics are output.

For example:
```json
{
   "session_id": "fbe09da3-ebdb-4863-8086-feb97464b8d7", // Randomly generated at Router startup.
   "version": "1.4.0", // The version of the router
   "os": "linux",
   "ci": null, // If CI is detected then this will name the CI vendor
   "usage": {
     "configuration.headers.all.request.propagate.named.<redacted>": 3,
     "configuration.headers.all.request.propagate.default.<redacted>": 1,
     "configuration.headers.all.request.len": 3
     "configuration.headers.subgraphs.<redacted>.request.propagate.named.<redacted>": 2,
     "configuration.headers.subgraphs.<redacted>.request.len": 2,
     "configuration.headers.subgraphs.len": 1,
     "configuration.homepage.enabled.true": 1,
     "args.config-path.redacted": 1,
     "args.hot-reload.true": 1,
     //Many more keys. This is dynamic and will change over time.
     //More...
     //More...
     //More...
   }
 }
```

Users can disable the sending this data by using the command line flag `--anonymous-telemetry-disabled` or setting the environment variable `APOLLO_TELEMETRY_DISABLED=true`

By [@bryncooke](https://github.com/bryncooke) in https://github.com/apollographql/router/pull/2173, https://github.com/apollographql/router/issues/2398


## 🐛 Fixes

### Specify content type to `application/json` on requests with content-type/accept header missmatch ([Issue #2334](https://github.com/apollographql/router/issues/2334))

When receiving requests with invalid content-type/accept header missmatch (e.g multipart requests) , it now specifies the right `content-type` header.

By [@Meemaw](https://github.com/Meemaw) in https://github.com/apollographql/router/pull/2370


## 🛠 Maintenance

### Remove unused factory traits ([Issue #2180](https://github.com/apollographql/router/pull/2372))

Building the execution and subgraph services had to go through a factory trait before, which is not
needed anymore since there is only one useful implementation.

By [@Geal](https://github.com/geal) in https://github.com/apollographql/router/pull/2372

### Optimize header propagation plugin's regex matching ([PR #2391](https://github.com/apollographql/router/pull/2389))

We've changed the plugin to reduce the chances of generating memory allocations when applying regex-based header propagation rules.

By [@o0Ignition0o](https://github.com/o0Ignition0o) in https://github.com/apollographql/router/pull/2389
