### Adding support for specified log "message" / type parameter ([Issue #2777](https://github.com/apollographql/router/issues/2777))

The current logs only state the output for "rhai_*", which while mostly descriptive, doesn't cover the use-case of needing custom logs for observability. This change adds a second optional parameter to the `log_` commands in Rhai to specify the `message` attribute. For example: 

```rhai
fn subgraph_service(service, subgraph) {
    service.map_response(|response| {
        if response.body.errors != {} {
            log_error(response.body.errors, `Subgraph: ${subgraph}`);
        }
    });
}
```

Which would return an example log similar to: 

```json
{"timestamp":"2023-03-16T16:08:44.738148Z","level":"ERROR","out":"redacted","message":"Subgraph: 2"}
```

This will enable customers to have APMs effectively parse logs to be more meaningful to them. 

Previously, logs would only be bucketed under the `rhai_error` `message` value.

By [@lleadbet](https://github.com/lleadbet) in https://github.com/apollographql/router/pull/2797
