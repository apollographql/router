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

### Rhai plugin `request.sub_headers` renamed to `request.subgraph.headers` [PR #1261](https://github.com/apollographql/router/pull/1261)

Rhai scripts previously supported the `request.sub_headers` attribute so that subgraph request headers could be
accessed. This is now replaced with an extended interface for subgraph requests:

```
request.subgraph.headers
request.subgraph.body.query
request.subgraph.body.operation_name
request.subgraph.body.variables
request.subgraph.body.extensions
request.subgraph.uri.host
request.subgraph.uri.path
```

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/1261

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

### Add support for adding extra enviromental variables and volumes to helm chart [PR #1245](https://github.com/apollographql/router/pull/1245)

The following example will allow you to mount your supergraph.yaml into the helm deployment using a configmap with a key of supergraph.yaml. Using [Kustomize](https://kustomize.io/) to generate your configmap from your supergraph.yaml is suggested.

Example:
```yaml
extraEnvVars:
  - name: APOLLO_ROUTER_SUPERGRAPH_PATH
    value: /etc/apollo/supergraph.yaml
    # sets router log level to debug
  - name: APOLLO_ROUTER_LOG
    value: debug
extraEnvVarsCM: ''
extraEnvVarsSecret: ''

extraVolumes:
  - name: supergraph-volume
    configMap:
      name: some-configmap 
extraVolumeMounts: 
  - name: supergraph-volume
    mountPath: /etc/apollo
```

By [@LockedThread](https://github.com/LockedThread) in https://github.com/apollographql/router/pull/1245

## 🐛 Fixes

### Deny unknown fields on configuration [PR #1278](https://github.com/apollographql/router/pull/1278)
Do not silently skip some bad configuration, now if you add an unknown configuration field at the root of your configuration file it will return an error.

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/1278

### Support introspection object types ([PR #1240](https://github.com/apollographql/router/pull/1240))


### Update the scaffold template so it works with streams ([#1247](https://github.com/apollographql/router/issues/1247))

Release v0.9.4 changed the way we deal with Response objects, which can now be streams.
This Pull request updates the scaffold template so it generates plugins that are compatible with the new Plugin API.

By [@o0Ignition0o](https://github.com/o0Ignition0o) in https://github.com/apollographql/router/pull/1248


Introspection queries can use a set of object types defined in the specification. The query parsing code was not recognizing them,
resulting in some introspection queries not working.

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/1240

### Create the ExecutionResponse after the primary response was generated ([PR #1260](https://github.com/apollographql/router/pull/1260))

The `@defer` preliminary work has a surprising side effect: when using methods like `RouterResponse::map_response`, they are
executed before the subgraph responses are received, because they work on the stream of responses.
This PR goes back to the previous behaviour by awaiting the primary response before creating the ExecutionResponse.

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/1260

### Use the API schema to generate selections ([PR #1255](https://github.com/apollographql/router/pull/1255))

When parsing the schema to generate selections for response formatting, we should use the API schema instead of the supergraph schema.

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/1255

## 🛠 Maintenance
## 📚 Documentation

### Update README link to the configuration file  ([PR #1208](https://github.com/apollographql/router/pull/1208))

As the structure of the documentation has changed, the link should point to the `YAML config file` section of the overview.

By [@gscheibel](https://github.com/gscheibel in https://github.com/apollographql/router/pull/1208

