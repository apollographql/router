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
## 🐛 Fixes

## Example section entry format

### **Headline** ([Issue #ISSUE_NUMBER](https://github.com/apollographql/router/issues/ISSUE_NUMBER))

Description! And a link to a [reference](http://url)

By [@USERNAME](https://github.com/USERNAME) in https://github.com/apollographql/router/pull/PULL_NUMBER
-->

# [0.9.5] (unreleased) - 2022-mm-dd
## ❗ BREAKING ❗

### Rename `experimental.traffic_shaping` to `apollo.traffic_shaping` [PR #1229](https://github.com/apollographql/router/pull/1229)
You will need to update your YAML configuration file to use the correct name for `traffic_shaping` plugin.

```diff
- plugins:
-   experimental.traffic_shaping:
-     variables_deduplication: true # Enable the variables deduplication optimization
-     all:
-       query_deduplication: true # Enable query deduplication for all subgraphs.
-     subgraphs:
-       products:
-         query_deduplication: false # Disable query deduplication for products.
+ traffic_shaping:
+   variables_deduplication: true # Enable the variables deduplication optimization
+   all:
+     query_deduplication: true # Enable query deduplication for all subgraphs.
+   subgraphs:
+     products:
+       query_deduplication: false # Disable query deduplication for products.
```

## 🚀 Features

### Add support of compression [PR #1229](https://github.com/apollographql/router/pull/1229)
Add support of request and response compression for the router and all subgraphs. The router is now able to handle `Content-Encoding` and `Accept-Encoding` headers properly. Supported algorithms are `gzip`, `br`, `deflate`.
You can also enable compression on subgraphs requests and responses by updating the `traffic_shaping` configuration:

```yaml
traffic_shaping:
  all:
    compression: br # Enable brotli compression for all subgraphs
  subgraphs:
    products:
      compression: gzip # Enable gzip compression only for subgraph products
```

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/1229


### Add support of multiple uplink URLs [PR #1210](https://github.com/apollographql/router/pull/1210)
Add support of multiple uplink URLs with a comma-separated list in `APOLLO_UPLINK_ENDPOINTS` and for `--apollo-uplink-endpoints`

Example: 
```bash
export APOLLO_UPLINK_ENDPOINTS="https://aws.uplink.api.apollographql.com/, https://uplink.api.apollographql.com/"
```

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/1210

## 🐛 Fixes

### Support introspection object types ([PR #1240](https://github.com/apollographql/router/pull/1240))

Introspection queries can use a set of object types defined in the specification. The query parsing code was not recognizing them,
resulting in some introspection queries not working.

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/1240

### Create the ExecutionResponse after the primary response was generated ([PR #1260](https://github.com/apollographql/router/pull/1260))

The `@defer` preliminary work has a surprising side effect: when using methods like `RouterResponse::map_response`, they are
executed before the subgraph responses are received, because they work on the stream of responses.
This PR goes back to the previous behaviour by awaiting the primary response before creating the ExecutionResponse.

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/1260

## 🛠 Maintenance
## 📚 Documentation

### Update README link to the configuration file  ([PR #1208](https://github.com/apollographql/router/pull/1208))

As the structure of the documentation has changed, the link should point to the `YAML config file` section of the overview.

By [@gscheibel](https://github.com/gscheibel in https://github.com/apollographql/router/pull/1208

