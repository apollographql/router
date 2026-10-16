### Update field types and member references when renaming a union type ([PR #10383](https://github.com/apollographql/router/pull/10383))

Renaming a union in `apollo-federation`'s internal schema representation left fields that return the union typed with the old, removed name, and left stale entries in the reference index for its members. Production code does not currently rename unions, so there is no user-visible change; this keeps the rename producing a valid schema for future callers.

By [@inanna-apollo](https://github.com/inanna-apollo) in https://github.com/apollographql/router/pull/10383
