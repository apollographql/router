## 🚀 Features

### Add `hasNext` to SupergraphRequest ([Issue #4016](https://github.com/apollographql/router/issues/4016))

Coprocessors multi-part response support has been enhanced to include `hasNext`, allowing you to determine when a request has completed.

When `stage` is `SupergraphResponse`, `hasNext` if present and `true` indicates that there will be subsequent `SupergraphResponse` calls to the co-processor for each multi-part (`@defer`/subscriptions) response.

See the [coprocessor documentation](https://www.apollographql.com/docs/router/customizations/coprocessor/) for more details.

By [@BrynCooke](https://github.com/BrynCooke) in https://github.com/apollographql/router/pull/4017

### Expose the ability to set topology spread constraints on the helm chart ([3891](https://github.com/apollographql/router/issues/3891))

Give developers the ability to set topology spread constraints that can be used to guarantee that federation pods are spread out evenly across AZs.

By [bjoern](https://github.com/bjoernw) in https://github.com/apollographql/router/pull/3892

## 🐛 Fixes

### Ignore JWKS keys which aren't supported by the router ([Issue #3853](https://github.com/apollographql/router/issues/3853))

If you have a JWKS which contains a key which has an algorithm (alg) which the router doesn't recognise, then the entire JWKS is disregarded. This is unsatisfactory, since there are likely to be many other keys in the JWKS which the router could use.

We have changed the JWKS processing logic so that we remove entries with an unrecognised algorithm from the list of available keys. We print a warning with the name of the algorithm for each removed entry.

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/3922

### Fix panic when streaming responses to co-processor ([Issue #4013](https://github.com/apollographql/router/issues/4013))

Streamed responses will no longer cause a panic in the co-processor plugin. This affected defer and stream queries.

By [@BrynCooke](https://github.com/BrynCooke) in https://github.com/apollographql/router/pull/4014

### Only reject defer/subscriptions if actually part of a batch ([Issue #3956](https://github.com/apollographql/router/issues/3956))

Fix the checking logic so that deferred queries or subscriptions will only be rejected when experimental batching is enabled and the operations are part of a batch.

Without this fix, all subscriptions or deferred queries would be rejected when experimental batching support was enabled.

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/3959

### Fix requires selection in arrays ([Issue #3972](https://github.com/apollographql/router/issues/3972))

When a field has a `@requires` annotation that selects an array, and some fields are missing in that array or some of the elements are null, the router would short circuit the selection and remove the entire array. This relaxes the condition to allow nulls in the selected array

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/3975

### Fix router hang when opening the explorer, prometheus or health check page ([Issue #3941](https://github.com/apollographql/router/issues/3941))

The Router did not gracefully shutdown when an idle connections are made by a client, and would instead hang. In particular, web browsers make such connection in anticipation of future traffic.

This is now fixed, and the Router will now gracefully shut down in a timely fashion.

---

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/3969

### Fix hang and high CPU usage when compressing small responses ([PR #3961](https://github.com/apollographql/router/pull/3961))

When returning small responses (less than 10 bytes) and compressing them using gzip, the router could go into an infinite loop

---

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/3961

## 📃 Configuration

### Add `enabled` field for telemetry exporters ([PR #3952](https://github.com/apollographql/router/pull/3952))

Telemetry configuration now supports `enabled` on all exporters. This allows exporters to be disabled without removing them from the configuration and in addition allows for a more streamlined default configuration.

```diff
telemetry:
  tracing: 
    datadog:
+      enabled: true
    jaeger:
+      enabled: true
    otlp:
+      enabled: true
    zipkin:
+      enabled: true
```

Existing configurations will be migrated to the new format automatically on startup. However, you should update your configuration to use the new format as soon as possible. 

By [@BrynCooke](https://github.com/BrynCooke) in https://github.com/apollographql/router/pull/3952

## 🛠 Maintenance

### Create a replacement self-signed server certificate: 10 years lifespan ([Issue #3998](https://github.com/apollographql/router/issues/3998))

This certificate is only used for testing, so 10 years lifespan is acceptable.

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/4009

## 📚 Documentation

### Updated documentation for deploying router ([PR #3943](https://github.com/apollographql/router/pull/3943))

Updated documentation for containerized router deployments, with guides and examples for [deploying on Kubernetes](https://www.apollographql.com/docs/router/containerization/kubernetes) and [running on Docker](https://www.apollographql.com/docs/router/containerization/docker).

By [@shorgi](https://github.com/shorgi) in https://github.com/apollographql/router/pull/3943

### Document guidance for request and response buffering ([Issue #3838](https://github.com/apollographql/router/issues/3838))

Provides specific guidance on request and response buffering within the router.

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/3970
