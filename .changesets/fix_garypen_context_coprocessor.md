### Improve subgraph co-processor context processing ([Issue #3058](https://github.com/apollographql/router/issues/3058))

Each call to a subgraph co-processor could update the entire request context as a single operation. This is racy and could lead to difficult to predict context modifications depending on the order in which subgraph requests and responses are processed by the router.

This fix modifies the router so that subgraph co-processor context updates are merged within the existing context. This is still racy, but means that subgraphs are only racing to perform updates at the context key level, rather than across the entire context. This is a substantial improvement on the current situation.

Future enhancements will provide a more comprehensive mechanism that will support some form of sequencing or change arbitration across subgraphs.

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/3054