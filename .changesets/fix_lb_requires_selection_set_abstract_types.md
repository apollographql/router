### Fix building entity representations with inline fragments ([PR #4441](https://github.com/apollographql/router/pull/4441))

Previously, when applying a selection set to an entity reference before it's used in a fetch node, the router would drop data from the reference when it selected using an inline fragment, for example `@requires(fields: "... on Foo { a } ... on Bar { b }")`).

This release uses a more flexible abstract type / concrete type check when applying a selection set to an entity reference before it's used in a fetch node. 

By [@lennyburdette](https://github.com/lennyburdette) in https://github.com/apollographql/router/pull/4441
