### Limit how deeply connector selections can nest

A deeply nested `@connect(selection:)` could overflow the stack and abort composition or router startup, including a running router reloading a new supergraph. Selections are now limited to 128 levels and fail validation with `Selection nested too deeply` past that. Each `{`, `[`, `(` and `$(` counts as a level, as does each path step (a key, `.key`, `->method`, `$`, `@` or `?`) leading to it. This allows 63 levels of `a { ... }`, 127 levels of `a: { ... }`, 128 levels of arrays, or 127 path or method steps. Supergraphs with deeper selections, which previously composed, are now rejected.

By [@tninesling](https://github.com/tninesling) in https://github.com/apollographql/router/pull/10584
