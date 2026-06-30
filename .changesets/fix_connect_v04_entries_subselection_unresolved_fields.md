### Resolve fields selected beneath list-shaped arrow methods like `->entries` in connect v0.4 ([PR #9619](https://github.com/apollographql/router/pull/9619))

Composition with connect v0.4 reported spurious `CONNECTORS_UNRESOLVED_FIELD` errors for fields selected
beneath an `->entries` sub-selection — e.g. `attributes: attributes->entries { key value }` against
`attributes: [AttributesEntry]` left `AttributesEntry.key` and `AttributesEntry.value` "unresolved", even
though the selection plainly resolves them. The identical schema composed cleanly under connect v0.3.

Cause: v0.4's shape-based selection validator only collected seen fields for object-shaped selections;
list-valued shapes — produced by methods with statically known list outputs, like `->entries` — fell
through a catch-all and contributed no seen fields. The validator now walks `Array` shapes by validating
each item shape against the field's (already list-unwrapped) inner named type.

By [@fernando-apollo](https://github.com/fernando-apollo) in [PR #9619](https://github.com/apollographql/router/pull/9619)
