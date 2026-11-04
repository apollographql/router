### Compose subgraphs whose renamed root type implements an entity interface

A subgraph can name its root query type something other than `Query` (for example, `schema { query: RootQuery }`); composition renames that type to `Query`. If the root type implemented an interface, the rename left the interface's list of implementations pointing at the old name. When that interface had a `@key`, composition then failed with an internal error (`Schema has no type "RootQuery"`) while checking that the interface's implementations also carry the key, even though the same subgraph composes when its root type is already named `Query`.

Renaming a type now updates the interfaces it implements, so such subgraphs compose.

By [@inanna-apollo](https://github.com/inanna-apollo) in https://github.com/apollographql/router/pull/10378
