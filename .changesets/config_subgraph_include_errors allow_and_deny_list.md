### `include_subgraph_errors` fine grained control ([Issue #6402](https://github.com/apollographql/router/pull/6402)

Update `include_subgraph_errors` with additional configuration options for both global and subgraph levels. This update provides finer control over error messages and extension keys for each subgraph. 
For more details, please read [subgraph error inclusion](https://www.apollographql.com/docs/graphos/routing/observability/subgraph-error-inclusion).

```yaml
include_subgraph_errors:
  all:
    redact_message: true
    allow_extensions_keys:
      - code
  subgraphs:
    product:
      redact_message: false  # Propagate original error messages
      allow_extensions_keys: # Extend global allow list - `code` and `reason` will be propagated
        - reason
      exclude_global_keys:   # Exclude `code` from global allow list - only `reason` will be propagated.
        - code
    account:
      deny_extensions_keys:  # Overrides global allow list
        - classification
    review: false            # Redact everything.

    # Undefined subgraphs inherits default global settings from `all`
``` 

**Note:** Using a `deny_extensions_keys` approach carries security risks because any sensitive information not explicitly included in the deny list will be exposed to clients. For better security, subgraphs should prefer to redact everything or `allow_extensions_keys` when possible.

By [@Samjin](https://github.com/Samjin) and [@bryncooke](https://github.com/bryncooke) in https://github.com/apollographql/router/pull/7164
