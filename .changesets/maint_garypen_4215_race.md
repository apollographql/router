### Identify and remove race condition in subgraph tests ([Issue #4215](https://github.com/apollographql/router/issues/4215))

Tokio TcpListener enables SO_REUSEADDR which means our tests are not as well isolated as we would like.

The fix is to ensure that tests using Tokio TcpListener are executed serially.

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/4297