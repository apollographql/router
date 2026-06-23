### Connectors: recursive input types no longer hang composition or crash expression validation ([PR #9524](https://github.com/apollographql/router/pull/9524))

A self-referential connector input type (e.g. `input Node { child: Node }`) previously caused two problems:

- During schema expansion, the input visitor's iterative `walk` would re-enter the same group indefinitely, consuming memory until composition was killed (previously reported as `Type "X" has already been pre-inserted`).
- During `@connect` expression validation, `resolve_shape` would recurse through the type's `Object` fields without a cycle guard, causing a stack overflow.

Recursive inputs now expand correctly and validate without unbounded recursion. When the validator re-enters a schema-defined named shape that is already on the resolution stack, it short-circuits to `Unknown` rather than walking the cycle.

By [@briannafugate](https://github.com/briannafugate) in https://github.com/apollographql/router/pull/9524
