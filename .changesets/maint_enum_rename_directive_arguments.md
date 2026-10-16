### Update directive argument types when renaming an enum type ([PR #10382](https://github.com/apollographql/router/pull/10382))

Renaming an enum in `apollo-federation`'s internal schema representation rewrote fields, field arguments and input fields that use it, but not directive definition arguments, which kept the old, removed name. Production code does not currently rename enums, so there is no user-visible change; this keeps the rename producing a valid schema for future callers.

By [@inanna-apollo](https://github.com/inanna-apollo) in https://github.com/apollographql/router/pull/10382
