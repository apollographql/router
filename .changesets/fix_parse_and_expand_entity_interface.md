### Exclude entity interfaces from `_Entity` when expanding a subgraph with `Subgraph::parse_and_expand` ([PR #10371](https://github.com/apollographql/router/pull/10371))

`apollo_federation::subgraph::Subgraph::parse_and_expand` added every type with `@key` to the generated `_Entity` union, including interfaces with `@key` (entity interfaces, valid since Federation v2.3). Union members must be object types, so a valid subgraph using an entity interface failed to parse. Only object types are now included, matching composition and the router, which use a different subgraph pipeline and were not affected.

By [@inanna-apollo](https://github.com/inanna-apollo) in https://github.com/apollographql/router/pull/10371
