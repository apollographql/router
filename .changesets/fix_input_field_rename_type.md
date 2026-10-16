### Rename an input field's type, not its name, when a referenced type is renamed ([PR #10379](https://github.com/apollographql/router/pull/10379))

When a scalar or enum used as an input field's type was renamed during subgraph processing, the input field's name was overwritten with the new type name and its type kept the old, removed name, producing an invalid schema. In practice this was only reachable through a Federation 1 subgraph with an input field typed `_FieldSet`, which is renamed during the Federation 2 upgrade. The field name is now preserved and its type, including list and non-null wrappers, is updated.

By [@inanna-apollo](https://github.com/inanna-apollo) in https://github.com/apollographql/router/pull/10379
