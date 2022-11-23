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

### Provide multi-arch (amd64/arm64) Docker images for the Router ([Issue #1932](https://github.com/apollographql/router/pull/2138))

From the next release, our Docker images will be multi-arch.

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/2138

### Add a supergraph configmap option to the helm chart ([PR #2119](https://github.com/apollographql/router/pull/2119))

Adds the capability to create a configmap containing your supergraph schema. Here's an example of how you could make use of this from your values.yaml and with the `helm` install command.

```yaml
extraEnvVars:
  - name: APOLLO_ROUTER_SUPERGRAPH_PATH
    value: /data/supergraph-schema.graphql

extraVolumeMounts:
  - name: supergraph-schema
    mountPath: /data
    readOnly: true

extraVolumes:
  - name: supergraph-schema
    configMap:
      name: "{{ .Release.Name }}-supergraph"
      items:
        - key: supergraph-schema.graphql
          path: supergraph-schema.graphql
```

With that values.yaml content, and with your supergraph schema in a file name supergraph-schema.graphql, you can execute:

```
helm upgrade --install --create-namespace --namespace router-test --set-file supergraphFile=supergraph-schema.graphql router-test oci://ghcr.io/apollographql/helm-charts/router --version 1.0.0-rc.9 --values values.yaml
```

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/2119

### *Experimental* subgraph request retry ([Issue #338](https://github.com/apollographql/router/issues/338), [Issue #1956](https://github.com/apollographql/router/issues/1956))

Implements subgraph request retries, using Finagle's retry buckets algorithm:
- it defines a minimal number of retries per second (`min_per_sec`, default is 10 retries per second), to
bootstrap the system or for low traffic deployments
- for each successful request, we add a "token" to the bucket, those tokens expire after `ttl` (default: 10 seconds)
- the number of available additional retries is a part of the number of tokens, defined by `retry_percent` (default is 0.2)

This is activated in the `traffic_shaping` plugin, either globally or per subgraph:

```yaml
traffic_shaping:
  all:
    experimental_retry:
      min_per_sec: 10
      ttl: 10s
      retry_percent: 0.2
  subgraphs:
    accounts:
      experimental_retry:
        min_per_sec: 20
```

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/2006

## 🐛 Fixes

### Improve errors when subgraph returns non-GraphQL response with a non-2xx status code ([Issue #2117](https://github.com/apollographql/router/issues/2117))

The error response will now contain the status code and status name. Example: `HTTP fetch failed from 'my-service': 401 Unauthorized`

By [@col](https://github.com/col) in https://github.com/apollographql/router/pull/2118

## 🛠 Maintenance
## 📚 Documentation

### update documentation to reflect new examples structure ([Issue #2095](https://github.com/apollographql/router/pull/2133))

We recently updated the examples directory structure. This fixes the documentation links to the examples. It also makes clear that rhai subgraph fields are read-only, since they are shared resources.

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/2133

