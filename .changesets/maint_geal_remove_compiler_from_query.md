### remove the compiler from Query ([Issue #3373](https://github.com/apollographql/router/issues/3373))

we don't really need to carry the compiler inside that object, since it is now cached at the query analysis layer. We also should not carry it in the supergraph request and execution request, because that makes the builders hard to manipulate for plugin authors. Since we are not exposing the compiler in the public API yet, we move it inside the context's private entries, where it will be easily accessible from internal code.

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/3367