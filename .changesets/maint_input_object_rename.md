### Update input value types and references when renaming an input object type ([PR #10384](https://github.com/apollographql/router/pull/10384))

Renaming an input object in `apollo-federation`'s internal schema representation left field arguments, input fields and directive arguments that take it typed with the old, removed name, and left stale entries in the reference index for its fields. Production code does not currently rename input objects, so there is no user-visible change; this keeps the rename producing a valid schema for future callers.

By [@inanna-apollo](https://github.com/inanna-apollo) in https://github.com/apollographql/router/pull/10384
