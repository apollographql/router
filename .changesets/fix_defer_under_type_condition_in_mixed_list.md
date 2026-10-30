### Deliver `@defer` payloads under a type condition in a list of mixed types

A `@defer` inside a type-conditioned fragment, such as `... on Item { ... @defer { title } }`, lost its data when the list it sat under held objects of other types too. The router sent the primary response, then a final `{"hasNext":false}` with no `incremental` entry and no error.

The query plan and the deferred fetch were correct. The router dropped the data while formatting the deferred response. It built the selection for that response from the field names on the path and dropped the type conditions, so the deferred field was also required on objects of other types. Those objects failed the non-null check and the null propagated up to the root of the response.

The deferred selection now keeps the type conditions on its path. The router also resolves the path of each deferred response before formatting, while `__typename` is still present. Previously a type condition on that path matched every object, which produced an empty incremental entry for each object of another type.

A type condition on the deferred fragment itself, such as `... on Item @defer { title }` or `...ItemFields @defer`, no longer produces an empty incremental entry for each object of another type. A labeled `@defer` inside a fragment spread under several type conditions now delivers data for each of them, instead of an empty entry for all but one.

When a non-null violation in deferred data nulls an element of a nullable list, the router now sends the incremental entry for that element with its error. Previously it dropped the entry and the error.

By [@jeffutter](https://github.com/jeffutter)
