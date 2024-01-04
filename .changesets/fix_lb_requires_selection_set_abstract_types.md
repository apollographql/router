### fix: build entity representations with inline fragments correctly ([PR #4441](https://github.com/apollographql/router/pull/4441))

This uses a more flexible abstract type / concrete type check when applying a selection set to an entity reference before it's used in a fetch node. Previous to this change, we would drop data from the reference when it selected using an inline fragment (e.g. `@requires(fields: "... on Foo { a } ... on Bar { b }")`).

By [@lennyburdette](https://github.com/lennyburdette) in https://github.com/apollographql/router/pull/4441
