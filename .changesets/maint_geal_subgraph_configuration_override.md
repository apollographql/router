### Subgraph configuration override ([Issue #2426](https://github.com/apollographql/router/issues/2426))

This introduces a new generic wrapper type for subgraph level configuration, with the following behaviour:
- if there's a config in all, it applies to all subgraphs. If it is not there, the default values apply
- if there's a config in subgraphs for a specific subgraphs:
  - the fields it specifies override the fields specified by all
  - the fields it does not specify use the values provided by all, or default values if applicable

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/2453