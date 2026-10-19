### Composition: field type merging no longer depends on subgraph order ([PR #10463](https://github.com/apollographql/router/pull/10463))

When composition merged a field (or argument) whose type differed across three or more subgraphs, the result could depend on the order in which the subgraphs were listed. For example, a field declared as `[String!]`, `[String]!`, and `[String]` across three subgraphs has `[String]` as a common supertype, but composition would fail with `FIELD_TYPE_MISMATCH` if the subgraph declaring `[String]` happened to come last.

Composition now finds the common supertype (for output positions) or subtype (for input positions) regardless of subgraph order, and only reports a type mismatch when no such type exists among the declarations.

By [@sachindshinde](https://github.com/sachindshinde) in https://github.com/apollographql/router/pull/10463
