### Demand control: size list-type slicing arguments by their coerced length ([PR #10787](https://github.com/apollographql/router/pull/10787))

When a `@listSize` directive names a list-type argument in `slicingArguments`, the router sizes the list by the length of the argument. GraphQL input coercion treats a single value sent for a list-type argument as a list with one item, but the router's static cost estimate used the value itself as the list size. A client sending `{"ids": 42}` instead of `{"ids": [42]}` was therefore estimated as if it had asked for 42 items, which could reject the operation in `enforce` mode.

The estimate now consults the argument's declared type, so a single value sent for a list-type slicing argument counts as one item. Numeric (`Int`) slicing arguments are unchanged.

To support this, the router now resolves each `slicingArguments` path against the schema when it loads the schema. A path that doesn't name an argument of the field (or a chain of input object fields under one) was previously ignored silently; the router now logs a warning for it at load time and continues to ignore it.

By [@sachindshinde](https://github.com/sachindshinde) in https://github.com/apollographql/router/pull/10787
