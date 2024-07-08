### Allow query batching and entity caching to work together ([PR #5598](https://github.com/apollographql/router/pull/5598))

Without co-ordination, entity caching and subgraph batching will not work together. This changes entity caching so that, if a subgraph request is identified as being part of a batch, entity caching is not applied to the request.

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/5598