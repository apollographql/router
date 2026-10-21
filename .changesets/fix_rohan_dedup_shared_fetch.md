### A deduplicated subgraph request no longer restarts when its first caller goes away

With `deduplicate_query` enabled, identical subgraph queries in flight share one request. Previously that request belonged to the first caller. If that caller was cancelled, for example because its client disconnected, the request was dropped and a waiting caller sent it again. The subgraph received the query twice, and the waiting caller's response arrived later than it should have.

The shared request now keeps running while any caller still waits for it. Every caller receives the same response, and the subgraph receives the query once.

By [@rohan-b99](https://github.com/rohan-b99)
