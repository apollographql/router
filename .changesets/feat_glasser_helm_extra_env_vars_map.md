### Add `extraEnvVarsMap` to the Helm chart for merging environment variables across values files ([Issue #4858](https://github.com/apollographql/router/issues/4858))

The router Helm chart's `extraEnvVars` value is a list, and Helm replaces lists (rather than merging them) when combining multiple values files or `--set` flags. That makes it awkward to define common environment variables in one values file and add environment-specific ones in another: the later file has to repeat the entire list.

The chart now also accepts `extraEnvVarsMap`, a map keyed by environment variable name. Because it is a map, Helm merges it across values files, so a later file can add variables, override a variable's value, or remove a variable by setting it to `null`. Each value is the rest of the env entry (everything except `name`), so it contains `value` or `valueFrom` just like an `extraEnvVars` entry:

```yaml
extraEnvVarsMap:
  APOLLO_ROUTER_LOG:
    value: info
  MY_SECRET:
    valueFrom:
      secretKeyRef:
        name: router-secrets
        key: my-secret
```

`extraEnvVarsMap` is now the recommended way to set environment variables from the chart. The existing `extraEnvVars` list still works and is unchanged; it remains useful when the order of the variables matters, since Kubernetes only expands a `$(VAR)` reference to a variable defined earlier in the list. If both are provided, the `extraEnvVars` list is rendered before the `extraEnvVarsMap` map.

By [@glasser](https://github.com/glasser) in https://github.com/apollographql/router/pull/10748
