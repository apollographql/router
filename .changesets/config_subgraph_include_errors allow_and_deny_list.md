### Brief but complete sentence that stands on its own ([Issue #6402](https://github.com/apollographql/router/pull/6402)

Update `include_subgraph_errors` with additional configuration options for both global and subgraph levels. This update provides finer control over error messages and extension keys for each subgraph. For example, you can configure per subgraph:

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
      exclude_global_keys:   # Exclude `code` from global allow list - `reason` will be propagated
        - code
    account:
      deny_extensions_keys:  # Overrides global allow list
        - classification
    # Undefined subgraphs inherits default global settings from `all`
``` 


By [@AUTHOR](https://github.com/Samjin) in https://github.com/apollographql/router/pull/6402
