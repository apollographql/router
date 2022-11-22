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

### Router debug Docker images now run under the control of heaptrack ([Issue #2135](https://github.com/apollographql/router/pull/XXXX))

From the next release, our debug Docker image will invoke the router under the control of heaptrack. We are making this change to make it simple for users to investigate potential memory issues with the router.

Do not run debug images in performance sensitive contexts. The tracking of memory allocations will significantly impact performance. In general, the debug image should only be used in consultation with Apollo engineering and support.

Look at our documentation for examples of how to use the image in either Docker or Kubernetes.

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/XXXX

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

## 🐛 Fixes

### Improve errors when subgraph returns non-GraphQL response with a non-2xx status code ([Issue #2117](https://github.com/apollographql/router/issues/2117))

The error response will now contain the status code and status name. Example: `HTTP fetch failed from 'my-service': 401 Unauthorized`

By [@col](https://github.com/col) in https://github.com/apollographql/router/pull/2118

## 🛠 Maintenance
## 📚 Documentation

### update documentation to reflect new examples structure ([Issue #2095](https://github.com/apollographql/router/pull/2133))

We recently updated the examples directory structure. This fixes the documentation links to the examples. It also makes clear that rhai subgraph fields are read-only, since they are shared resources.

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/2133

