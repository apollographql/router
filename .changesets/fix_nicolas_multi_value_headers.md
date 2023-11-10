### Propagate multi-value headers to subgraphs ([Issue #4153](https://github.com/apollographql/router/issues/4153))

Use `HeaderMap.append` instead of `insert` to prevent erasing previous values when using multiple headers with the same name.

By [@nmoutschen](https://github.com/nmoutschen) in https://github.com/apollographql/router/pull/4154