[WIP] Federation core in Rust
-----------------------------

This private repository aims to ultimately house the rewrite of federation
composition and query planning (and any (and any federation specifics needed to
implement those) in rust.

At the time of this writing, it is extremely early stage and nothing here should
be relied on or discussed publicly.

Organisation wise, this is currently composed of 3 separate crates:
1. `apollo-at-link`: which aims at providing code to work with the `@link`
   directive and the mechanisms it provides.
2. `apollo-subgraph`: which aims at providing code to work with the
   specificities of federation subgraphs (essentially encode the knowledge
   of the various directives used by federation, their related validations,
   ...).
3. `apollo-supergraph`: which aims at provided code to work with federation
   supergraphs.
