### Authorization directives are analysed once per operation ([PR #10798](https://github.com/apollographql/router/pull/10798))

When a supergraph uses `@authenticated`, `@requiresScopes` or `@policy`, the router analysed every request's operation for these directives and read the supergraph's `@link` definitions several times to do it. On the largest supergraph measured, on a Router 3.x pre-release build, this cost about 300 µs of CPU per request, 213 µs of it spent reading `@link` definitions.

The router now reads the definitions once, when the schema loads. When router.yaml has an `authorization:` section, even an empty one, it also analyses each operation only the first time it is parsed and reuses the result for later requests with the same query, so a repeated operation skips both costs. Without the section, the router still analyses every request's operation. To get the full saving, add the section:

```yaml
authorization: {}
```

By [@BrynCooke](https://github.com/BrynCooke) in https://github.com/apollographql/router/pull/10798
