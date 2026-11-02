### A deduplicated subgraph request shares the timeout of the fetch it joins ([PR #10300](https://github.com/apollographql/router/pull/10300))

With query deduplication on, a subgraph request that joins an identical fetch already in flight now waits under that fetch's traffic shaping `timeout`, not a timer of its own. A request that joins late can therefore time out sooner than before, but never later.

By [@rohan-b99](https://github.com/rohan-b99) in https://github.com/apollographql/router/pull/10300
