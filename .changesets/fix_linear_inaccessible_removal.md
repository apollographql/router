### Fix quadratic API schema generation for contracts that hide most of a supergraph

Generating an API schema removed `@inaccessible` elements one at a time, and each removal updated
referencer sets spanning the whole schema. Contract variants that hide most of a large supergraph
took quadratic time, reaching tens of seconds. API schema generation now removes `@inaccessible`
and `@link`-imported elements in a single pass over the schema, producing the same API schema.

By [@tninesling](https://github.com/tninesling) in https://github.com/apollographql/router/pull/10715
