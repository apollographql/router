### Fix quadratic API schema generation for contracts that hide most of a supergraph

Generating an API schema removed `@inaccessible` elements one at a time, and each removal updated
referencer sets spanning the whole schema. Contract variants that hide most of a large supergraph
took quadratic time, reaching tens of seconds. The removal now filters the schema in a single pass
and rebuilds referencers once, producing the same API schema.

By [@tninesling](https://github.com/tninesling) in https://github.com/apollographql/router/pull/10715
