### Keep executable directives and their argument types through connector expansion

Composing connector subgraphs alongside an executable directive whose argument references a custom type (for example `directive @ownership(owners: [Owner!]) on FIELD` with `enum Owner`) failed satisfiability validation with `INTERNAL_ERROR: Cannot wrap an undefined/null type` (Rust: `Directive argument's inner type does not refer to an existing input type`).

Connector expansion builds one synthetic subgraph per connector, but it never copied executable directive definitions, or the types their arguments reference, into those subgraphs. When another subgraph defined the directive, it still reached the expanded supergraph, but the synthetic subgraphs were recorded as lacking its argument types, so extracting subgraphs from the expanded supergraph produced invalid schemas. When every subgraph used connectors, the directive was dropped from the expanded supergraph entirely.

Each synthetic subgraph now carries the executable directive definitions from its source subgraph, along with their argument types (scalars, enums, and input objects, recursively).

By [@benjamn](https://github.com/benjamn) in https://github.com/apollographql/router/pull/9635
