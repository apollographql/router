### Add warnings for invalid configuration of custom telemetry ([PR #5759](https://github.com/apollographql/router/issues/5759))

The router now logs warnings when running with telemetry that may have invalid custom configurations.
 

For example, you may customize telemetry using invalid conditions or inaccessible statuses:

```yaml
telemetry:
  instrumentation:
    events:
      subgraph:
        my.event:
          message: "Auditing Router Event"
          level: info
          on: request
          attributes:
            subgraph.response.status: code
              # Warning: should use selector for subgraph_name: true instead of comparing strings of subgraph_name and product
          condition:
            eq:
            - subgraph_name
            - product
```

Although the configuration is syntactically correct, its customization is invalid, and the router now outputs warnings for such invalid configurations.

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/5759