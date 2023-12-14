### Fix TLS session ticket retries ([Issue #4305](https://github.com/apollographql/router/issues/4305))

In some cases, the router could retry TLS connections indefinitely with an invalid session ticket, making it impossible to contact a subgraph. rustls 0.21.10 contains a fix for that

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/4362