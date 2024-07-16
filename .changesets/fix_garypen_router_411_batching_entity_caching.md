### Allow query batching and entity caching to work together ([PR #5598](https://github.com/apollographql/router/pull/5598))

The router now supports entity caching and subgraph batching to run simultaneously. Specifically, this change updates entity caching to ignore a subgraph request if the request is part of a batch.

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/5598