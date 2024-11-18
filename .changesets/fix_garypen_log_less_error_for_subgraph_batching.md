### If subgraph batching, do not log response data for notification failure ([PR #6150](https://github.com/apollographql/router/pull/6150))

A subgraph response may contain a lot of data and/or PII data.

For a subgraph batching operation, we should not log out the entire subgraph response when failing to notify a waiting batch participant.

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/6150