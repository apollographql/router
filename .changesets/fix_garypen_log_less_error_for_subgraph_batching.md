### Don't log response data upon notification failure for subgraph batching ([PR #6150](https://github.com/apollographql/router/pull/6150))

For a subgraph batching operation, the router now doesn't log the entire subgraph response when failing to notify a waiting batch participant. This saves the router from logging the large amount of data (PII and/or non-PII data) that a subgraph response may contain.

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/6150