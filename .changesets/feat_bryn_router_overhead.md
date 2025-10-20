### (feat) Add router overhead metric ([PR #8455](https://github.com/apollographql/router/pull/8455))

The `apollo.router.overhead` histogram provides a direct measurement of router processing overhead. This metric tracks the time the router spends on tasks other than waiting for subgraph requests—including GraphQL parsing, validation, query planning, response composition, and plugin execution.

The overhead calculation excludes time spent waiting for subgraph responses, giving you visibility into the router's actual processing time versus subgraph latency. This metric helps identify when the router itself is a bottleneck versus when delays are caused by downstream services.

```yaml title="router.yaml"
telemetry:
  instrumentation:
    instruments:
      router:
        apollo.router.overhead: true
```

**Note that the use of this metric is nuanced, and there is risk misinterpretation. See the full docs for this metric to help understand how it can be used.** 

By [@BrynCooke](https://github.com/BrynCooke) in https://github.com/apollographql/router/pull/8455
