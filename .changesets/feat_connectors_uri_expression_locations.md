### Expressions can now be used anywhere within Connectors URIs

Previously, we only allowed expressions in very specific locations:

1. A path segment, like `/users/{$args.id}`
2. A query parameter's _value_, like `/users?id={$args.id}`

Expressions can now be used anywhere in or after the path of the URI.
For example, you could do `@connect(http: {GET: "/users?{$args.filterName}={$args.filterValue}"})`.
The result of the expression will _always_ be percent encoded.

Parts of this feature will only be available when composing with Apollo Federation 2.11 or above (currently in preview).

By [@dylan-apollo](https://github.com/dylan-apollo) in https://github.com/apollographql/router/pull/7220