### add a router request builder ([Issue #3267](https://github.com/apollographql/router/issues/3267))

The builder implementation was missing on the router request side, which means that router service level plugins cannot reuse the context if they unpack the request object

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/3430