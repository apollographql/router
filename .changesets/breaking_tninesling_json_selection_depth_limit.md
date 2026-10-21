### Limit how deeply connector selections can nest

A deeply nested `@connect(selection:)` could overflow the stack and abort composition or router startup. Selections are now limited to 128 levels of parser recursion and fail validation with `Selection nested too deeply` past that. This allows about 42 levels of `a { ... }`, 126 levels of `a: { ... }` or arrays, or 125 path or method steps. Supergraphs with deeper selections, which previously composed, are now rejected.

By [@tninesling](https://github.com/tninesling) in https://github.com/apollographql/router/pull/10584
