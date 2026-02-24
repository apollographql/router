### Prevent duplicate tags in router spans added by dynamic attributes ([PR #8865](https://github.com/apollographql/router/pull/8865))

When dynamic attributes are added via `SpanDynAttribute::insert`, `SpanDynAttribute::extend`, `LogAttributes::insert`, `LogAttributes::extend`, `EventAttributes::insert`, or `EventAttributes::extend` and the key already exists, the router now replaces the existing value instead of creating duplicate attributes.

By [@rohan-b99](https://github.com/rohan-b99) in https://github.com/apollographql/router/pull/8865
