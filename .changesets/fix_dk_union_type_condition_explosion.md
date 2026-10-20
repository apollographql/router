### Stop type-exploding union type conditions when the subgraph's union matches the supergraph ([PR #10570](https://github.com/apollographql/router/pull/10570))

A type condition on a union inside another abstract type, such as `u1 { ... on U2 { __typename } }`, was always rewritten into one fragment per member (`... on A { __typename } ... on B { __typename }`) in subgraph fetches, even when the subgraph defines `U2` with the same members as the supergraph. The query planner now sends `... on U2` as-is in that case, the same way it already does for interfaces.

This applies to supergraphs composed with federation 2.3 or later, which record each subgraph's union members with `@join__unionMember`. Supergraphs composed with earlier versions don't carry that information, so the planner keeps type-exploding union type conditions for them.

By [@dariuszkuc](https://github.com/dariuszkuc) in https://github.com/apollographql/router/pull/10570
