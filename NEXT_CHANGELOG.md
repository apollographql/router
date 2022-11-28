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

### Router debug Docker images now run under the control of heaptrack ([Issue #2135](https://github.com/apollographql/router/pull/2142))

From the next release, our debug Docker image will invoke the router under the control of heaptrack. We are making this change to make it simple for users to investigate potential memory issues with the router.

Do not run debug images in performance sensitive contexts. The tracking of memory allocations will significantly impact performance. In general, the debug image should only be used in consultation with Apollo engineering and support.

Look at our documentation for examples of how to use the image in either Docker or Kubernetes.

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/2142

### Fix naming inconsistency of telemetry.metrics.common.attributes.router ([Issue #2076](https://github.com/apollographql/router/issues/2076))

Mirroring the rest of the config `router` should be `supergraph`

```yaml
telemetry:
  metrics:
    common:
      attributes:
        router: # old
```
becomes
```yaml
telemetry:
  metrics:
    common:
      attributes:
        supergraph: # new
```

By [@bryncooke](https://github.com/bryncooke) in https://github.com/apollographql/router/pull/2116

### CLI structure changes ([Issue #2123](https://github.com/apollographql/router/issues/2123))

As the Router gains functionality the limitations of the current CLI structure are becoming apparent.

There is now a separate subcommand for config related operations:
* `config`
  * `schema` - Output the configuration schema
  * `upgrade` - Upgrade the configuration with optional diff support.

`router --schema` has been deprecated and users should move to `router config schema`.

## 🚀 Features

### Add configuration for trace ID ([Issue #2080](https://github.com/apollographql/router/issues/2080))

If you want to expose in response headers the generated trace ID or the one you provided using propagation headers you can use this configuration:

```yaml title="router.yaml"
telemetry:
  tracing:
    experimental_response_trace_id:
      enabled: true # default: false
      header_name: "my-trace-id" # default: "apollo-trace-id"
    propagation:
      # If you have your own way to generate a trace id and you want to pass it via a custom request header
      request:
        header_name: my-trace-id
```

Using this configuration you will have a response header called `my-trace-id` containing the trace ID. It could help you to debug a specific query if you want to grep your log with this trace id to have more context.

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/2131

### Add configuration for logging and add more logs 

By default some logs containing sensible data (like request body, response body, headers) are not displayed even if we set the right log level.
For example if you need to display raw responses from one of your subgraph it won't be displayed by default. To enable them you have to configure it thanks to the `when_header` setting in the new section `experimental_logging`. It let's you set different headers to enable more logs (request/response headers/body for supergraph and subgraphs) when the request contains these headers with corresponding values/regex.
Here is an example how you can configure it:

```yaml title="router.yaml"
telemetry:
  experimental_logging:
    format: json # By default it's "pretty" if you are in an interactive shell session
    display_filename: true # Display filename where the log is coming from. Default: true
    display_line_number: false # Display line number in the file where the log is coming from. Default: true
    # If one of these headers matches we will log supergraph and subgraphs requests/responses
    when_header:
      - name: apollo-router-log-request
        value: my_client
        headers: true # default: false
        body: true # default: false
      # log request for all requests/responses headers coming from Iphones
      - name: user-agent
        match: ^Mozilla/5.0 (iPhone*
        headers: true
```

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

### Configuration upgrades ([Issue #2123](https://github.com/apollographql/router/issues/2123))

Occasionally we will make changes to the Router yaml configuration format.
When starting the Router if the configuration can be upgraded it will do so automatically and display a warning:

```
2022-11-22T14:01:46.884897Z  WARN router configuration contains deprecated options: 

  1. telemetry.tracing.trace_config.attributes.router has been renamed to 'supergraph' for consistency

These will become errors in the future. Run `router config upgrade <path_to_router.yaml>` to see a suggested upgraded configuration.
```

Note: If a configuration has errors after upgrading then the configuration will not be upgraded automatically.

From the CLI users can run:
* `router config upgrade <path_to_router.yaml>` to output configuration that has been upgraded to match the latest config format.
* `router config upgrade --diff <path_to_router.yaml>` to output a diff e.g.
```
 telemetry:
   apollo:
     client_name_header: apollographql-client-name
   metrics:
     common:
       attributes:
-        router:
+        supergraph:
           request:
             header:
             - named: "1" # foo
```

There are situations where comments and whitespace are not preserved. This may be improved in future.

By [@bryncooke](https://github.com/bryncooke) in https://github.com/apollographql/router/pull/2116, https://github.com/apollographql/router/pull/2162

### *Experimental* 🥼 subgraph request retry ([Issue #338](https://github.com/apollographql/router/issues/338), [Issue #1956](https://github.com/apollographql/router/issues/1956))

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

### *Experimental* 🥼 Caching configuration ([Issue #2075](https://github.com/apollographql/router/issues/2075))

Split Redis cache configuration for APQ and query planning:

```yaml
supergraph:
  apq:
    experimental_cache:
      in_memory:
        limit: 512
      redis:
        urls: ["redis://..."]
  query_planning:
    experimental_cache:
      in_memory:
        limit: 512
      redis:
        urls: ["redis://..."]
```

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/2155

## 🐛 Fixes

### Improve errors when subgraph returns non-GraphQL response with a non-2xx status code ([Issue #2117](https://github.com/apollographql/router/issues/2117))

The error response will now contain the status code and status name. Example: `HTTP fetch failed from 'my-service': 401 Unauthorized`

By [@col](https://github.com/col) in https://github.com/apollographql/router/pull/2118

### handle mutations containing @defer ([Issue #2099](https://github.com/apollographql/router/issues/2099))

The Router generates partial query shapes corresponding to the primary and deferred responses,
to validate the data sent back to the client. Those query shapes were invalid for mutations.

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/2102

## 🛠 Maintenance


### Refactor APQ ([PR #2129](https://github.com/apollographql/router/pull/2129))

Remove duplicated code.

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/2129


## 📚 Documentation

### Docs: Update cors match regex example ([Issue #2151](https://github.com/apollographql/router/issues/2151))

The docs CORS regex example now displays a working and safe way to allow `HTTPS` subdomains of `api.example.com`.

By [@col](https://github.com/o0Ignition0o) in https://github.com/apollographql/router/pull/2152


### update documentation to reflect new examples structure ([Issue #2095](https://github.com/apollographql/router/pull/2133))

We recently updated the examples directory structure. This fixes the documentation links to the examples. It also makes clear that rhai subgraph fields are read-only, since they are shared resources.

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/2133

