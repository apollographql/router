### Fix typo in persisted query metric attribute ([PR #6332](https://github.com/apollographql/router/pull/6332))

The `apollo.router.operations.persisted_queries` metric reports an attribute when a persisted query was not found.
Previously, the attribute name was `persisted_quieries.not_found`, with one `i` too many. Now it's `persisted_queries.not_found`.

By [@goto-bus-stop](https://github.com/goto-bus-stop) in https://github.com/apollographql/router/pull/6332