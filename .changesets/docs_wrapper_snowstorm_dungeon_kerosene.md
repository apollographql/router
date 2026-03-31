### Docs: Document http_client span attribute limitations ([PR #8967](https://github.com/apollographql/router/pull/8967))

Document that `http_client` span attributes do not support conditions or the `static` selector, causing a router startup failure when attempted.

By [@mabuyo](https://github.com/mabuyo) in https://github.com/apollographql/router/pull/8967