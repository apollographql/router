### Migrate away from unimplemented `coprocessor.subgraph.all.response.uri`

We have removed a completely unimplemented `coprocessor.subgraph.all.response.uri` key from our configuration.  It had no effect, but we will automatically migrate configurations which did use it, resulting in no breaking changes by this removal.

By [@o0ignition0o](https://github.com/o0ignition0o) in https://github.com/apollographql/router/pull/2863
