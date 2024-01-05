### Fix TLS session ticket retries ([Issue #4305](https://github.com/apollographql/router/issues/4305))

In some cases, the router could retry TLS connections indefinitely with an invalid session ticket, making it impossible to contact a subgraph. We've provided a fix to the upstream `rustls` project with a fix, and brought in the updated dependency when it was published in v0.21.10.

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/4362