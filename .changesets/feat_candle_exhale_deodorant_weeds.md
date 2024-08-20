### Add warnings for invalid configuration on custom telemetry ([PR #5759](https://github.com/apollographql/router/issues/5759))

For example sometimes if you have configuration like this:

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
            subgraph.response.status:
              subgraph_response_status: code # This is a first warning because you can't access to the response if you're at the request stage
          condition:
            eq:
            - subgraph_name # Another warning because instead of writing subgraph_name: true which is the selector, you're asking for a comparison between 2 strings ("subgraph_name" and "product")
            - product
```

This configuration is syntaxically correct but wouldn't probably do what you would like to. I put comments to highlight 2 mistakes in this example.
Before it was silently computed, now you'll get warning when starting the router.

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/5759