### Fix introspection query deduplication ([Issue #6249](https://github.com/apollographql/router/issues/6249))

To reduce CPU usage, query planning and introspection queries are deduplicated. In some cases, deduplicated introspection queries were not receiving their result. This makes sure that answers are sent in all cases.

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/6257