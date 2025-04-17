### Allow expressions in more locations in Connectors URIs ([PR #7220](https://github.com/apollographql/router/pull/7220))

Previously, we only allowed expressions in very specific locations in Connectors URIs:

1. A path segment, like `/users/{$args.id}`
2. A query parameter's _value_, like `/users?id={$args.id}`

Expressions can now be used anywhere in or after the path of the URI.
For example, you can do
`@connect(http: {GET: "/users?{$args.filterName}={$args.filterValue}"})`.
The result of any expression will _always_ be percent encoded.

> Note: Parts of this feature are only available when composing with Apollo Federation v2.11 or above (currently in preview).

By [@dylan-apollo](https://github.com/dylan-apollo) in https://github.com/apollographql/router/pull/7220