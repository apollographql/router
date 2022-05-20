# Changelog for the next release

All notable changes to Router will be documented in this file.

This project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

<!-- <THIS IS AN EXAMPLE, DO NOT REMOVE>

# [x.x.x] (unreleased) - 2022-mm-dd
> Important: X breaking changes below, indicated by **❗ BREAKING ❗**
## ❗ BREAKING ❗
## 🚀 Features ( :rocket: )
## 🐛 Fixes ( :bug: )
## 🛠 Maintenance ( :hammer_and_wrench: )
## 📚 Documentation ( :books: )
## 🐛 Fixes ( :bug: )

## Example section entry format

### **Headline** ([PR #PR_NUMBER](https://github.com/apollographql/router/pull/PR_NUMBER))

Description! And a link to a [reference](http://url)
-->

# [0.9.2] (unreleased) - 2022-mm-dd

## ❗ BREAKING ❗

### Simplify Context::upsert() [PR #1073](https://github.com/apollographql/router/pull/1073)
Removes the `default` parameter and requires inserted values to implement `Default`.

## 🚀 Features

### DIY docker images [PR #1106](https://github.com/apollographql/router/pull/1106)
The build_docker_image.sh script is now provided as a working example of how to build docker images from our GH release tarballs or from a commit hash/tag against the router repo.

## 🐛 Fixes

### Return top `__typename` field when it's not an introspection query [PR #1102](https://github.com/apollographql/router/pull/1102)
When `__typename` is used at the top of the query in combination with other fields it was not returned in the output.

### Fix the installation and releasing script for Windows [PR #1098](https://github.com/apollographql/router/pull/1098)
Do not put .exe for Windows in the name of the tarball when releasing new version

### Aggregate usage reports in streaming and set the timeout to 5 seconds [PR #1066](https://github.com/apollographql/router/pull/1066)
The metrics plugin was allocating chunks of usage reports to aggregate them right after, this was replaced by a streaming loop. The interval for sending the reports to spaceport was reduced from 10s to 5s.

### Put back the ability to use environment variable expansion for telemetry endpoints [PR #1092](https://github.com/apollographql/router/pull/1092)
Adds the ability to use environment variable expansion for the configuration of agent/collector endpoint for Jaeger, OTLP, Datadog.

### Fix the introspection query detection [PR #1100](https://github.com/apollographql/router/pull/1100)
Fix the introspection query detection, for example if you only have `__typename` in the query then it's an introspection query, if it's used with other fields (not prefixed by `__`) then it's not an introspection query.

## 🛠 Maintenance

### Add well known query to `PluginTestHarness` [PR #1114](https://github.com/apollographql/router/pull/1114)
Add `call_canned` on `PluginTestHarness`. It performs a well known query that will generate a valid response.


### Remove the batching and timeout from spaceport  [PR #1080](https://github.com/apollographql/router/pull/1080)
apollo-router is already handling report aggregation and sends the
report every 5s. Now spaceport will put the incoming reports in a
bounded queue and send them in order, with backpressure.

## 📚 Documentation
### Add CORS documentation ([PR #1044](https://github.com/apollographql/router/pull/1044))
We've updated the CORS documentation to reflect the recent [CORS and CSRF](https://github.com/apollographql/router/pull/1006) updates.

## 🐛 Fixes
