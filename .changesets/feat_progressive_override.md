### Introduce support for progressive @override ([PR #4521](https://github.com/apollographql/router/pull/4521))

The change brings support for progressive @override, which allows dynamically overriding root fields and entity fields in the schema. This feature is enterprise only and requires a license key to be used.

A new `label` argument is added to the `@override` directive in order to indicate the field is dynamically overridden. Labels can come in two forms:
1) String matching the form `percent(x)`: The router resolves these labels based on the `x` value. For example, `percent(50)` will route 50% of requests to the overridden field and 50% of requests to the original field.
2) Arbitrary string matching the regex `^[a-zA-Z][a-zA-Z0-9_-:./]*$`: These labels are expected to be resolved externally via coprocessor. A coprocessor a supergraph request hook can inspect and modify the context of a request in order to inform the router which labels to use during query planning.

Please consult the docs for more information on how to use this feature and how to implement a coprocessor for label resolution.

By [@TrevorScheer](https://github.com/TrevorScheer) in https://github.com/apollographql/router/pull/4521